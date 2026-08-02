use super::errno::*;
use super::fs::{FlockLock, FLOCK_LOCKS};
use crate::fs::vfs::posix_lock::LockKey;
use crate::task::current_task;

const LOCK_SH: u32 = 1;
const LOCK_EX: u32 = 2;
const LOCK_NB: u32 = 4;
const LOCK_UN: u32 = 8;

/// Implements `flock(2)` — apply or remove an advisory lock on an open file.
///
/// # Linux 6.6 semantics
///
/// - `LOCK_SH` — shared lock. Multiple shared locks may coexist.
/// - `LOCK_EX` — exclusive lock. Only one description may hold it.
/// - `LOCK_UN` — remove all locks held by this file description on this file.
/// - `LOCK_NB` — non-blocking: return `EAGAIN` immediately if a conflict exists.
///
/// Flock locks are **owned by the open file description** (`struct file *`),
/// NOT by process.  Two fds obtained via `dup(2)` share the same description
/// and therefore never conflict with each other.
///
/// # Lock upgrade / downgrade (Linux-compatible)
///
/// When a description already holds a lock of one type on a file and requests
/// the other type, Linux atomically releases the old lock and acquires the new
/// one (no "upgrade without gap" guarantee — a brief window exists where
/// another waiter can grab the lock between the release and reacquire).
pub fn sys_flock(fd: usize, operation: u32) -> isize {
    let nonblock = (operation & LOCK_NB) != 0;
    let op = operation & !LOCK_NB;

    match op {
        LOCK_SH | LOCK_EX | LOCK_UN => {}
        _ => return EINVAL,
    }

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(f) => f,
        Err(e) => return -(e as isize),
    };
    let key = LockKey::from_file(file.as_ref());
    let owner = file.description_id();
    // Drop fd table lock before touching global flock table
    // to avoid lock ordering issues between fd table and FLOCK_LOCKS.
    drop(fd_table);
    drop(files_ref);

    let mut locks = FLOCK_LOCKS.lock();

    if op == LOCK_UN {
        // Release all locks held by this description on this file.
        locks.retain(|l| !(l.key == key && l.owner_description == owner));
        // LOCK_UN always succeeds (Linux: no error for unlocking an unlocked file).
        return 0;
    }

    let want_ex = op == LOCK_EX;

    // Same description already holds a lock on this file? → no-op if same type,
    // replace (upgrade/downgrade) if different type.
    if let Some(index) = locks
        .iter()
        .position(|l| l.key == key && l.owner_description == owner)
    {
        if locks[index].exclusive == want_ex {
            // Already holds the requested lock type — nothing to do.
            return 0;
        }
        // Linux: atomically remove the old lock and acquire the new one.
        // The replacement is done by removing the existing entry below
        // and falling through to the normal acquire path.
        locks.remove(index);
    }

    // Conflict check: another description holds an exclusive lock,
    // OR we want an exclusive lock and another description holds a shared lock.
    let conflict = locks
        .iter()
        .any(|l| l.key == key && l.owner_description != owner && (l.exclusive || want_ex));

    if conflict {
        if nonblock {
            return EAGAIN;
        }
        // Blocking wait (WaitQueue-based) left as future work.
        // Linux would put the caller to sleep until the lock is released.
        return EAGAIN;
    }

    locks.push(FlockLock {
        key,
        owner_description: owner,
        exclusive: want_ex,
    });
    0
}
