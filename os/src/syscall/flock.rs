use alloc::collections::BTreeMap;
use spin::Mutex;

use super::errno::*;
use crate::fs::vfs::InodeId;
use crate::task::current_task;

const LOCK_SH: u32 = 1;
const LOCK_EX: u32 = 2;
const LOCK_NB: u32 = 4;
const LOCK_UN: u32 = 8;

/// Global per-inode advisory lock table.
/// Key = (dev_id, inode_id). Entry present = file is exclusively locked.
/// TODO: Track lock owner for LOCK_SH and blocking wait support.
static FLOCK_TABLE: Mutex<BTreeMap<(usize, InodeId), ()>> = Mutex::new(BTreeMap::new());

pub fn sys_flock(fd: usize, operation: u32) -> isize {
    let op = operation & 0xf;
    let nb = operation & LOCK_NB;

    match op {
        LOCK_SH | LOCK_EX | LOCK_UN => {}
        _ => return -EINVAL,
    }

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(f) => f,
        Err(e) => return -(e as isize),
    };
    let meta = match file.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    // Drop fd table lock before touching global flock table
    // to avoid lock ordering issues between fd table and FLOCK_TABLE.
    drop(fd_table);
    drop(files_ref);

    let key = (meta.dev_id, meta.inode_id);
    let mut table = FLOCK_TABLE.lock();

    match op {
        LOCK_UN => {
            table.remove(&key);
            0
        }
        LOCK_SH | LOCK_EX => {
            if table.contains_key(&key) {
                if nb != 0 {
                    -EAGAIN
                } else {
                    // Blocking lock: apk uses LOCK_EX|LOCK_NB so this is rare.
                    // Full WaitQueue-based blocking left as future work.
                    -EAGAIN
                }
            } else {
                table.insert(key, ());
                0
            }
        }
        _ => -EINVAL,
    }
}
