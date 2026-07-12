//! Sharded POSIX record lock manager with blocking support.
//!
//! Uses 53 shards keyed by `(dev_id, inode_id)`.  Each shard maps a `LockKey`
//! to a `PosixLockEntry` (sorted range-lock list + `WaitQueue`).
//!
//! Lock ownership is tracked per `FdTable::lock_owner_id` so that fork()d
//! processes do not share record locks.  F_SETLKW implements blocking via
//! `WaitQueue::wait_event_interruptible` with deadlock detection through a
//! per-manager wait-graph.

use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use super::fcntl::{PosixFlock, F_RDLCK, F_UNLCK, F_WRLCK};
use super::File;
use crate::task::WaitQueue;
use crate::task::WaitResult;
use crate::utils::error::SyscallErr;

// ── Constants ────────────────────────────────────────────────────────────

const SHARDS: usize = 53;

const SEEK_SET: i16 = 0;
const SEEK_CUR: i16 = 1;
const SEEK_END: i16 = 2;

// ── LockKey ──────────────────────────────────────────────────────────────

/// Uniquely identifies a file in the lock shard map.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct LockKey {
    pub dev_id: usize,
    pub inode_id: usize,
}

impl LockKey {
    pub fn from_file(file: &File) -> Self {
        let (dev, ino) = file.posix_lock_key();
        Self {
            dev_id: dev,
            inode_id: ino,
        }
    }

    /// Hash into one of the 53 shards.
    fn shard(&self) -> usize {
        let mut h: u32 = 0;
        h = h.wrapping_add(self.dev_id as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
        h = h.wrapping_add(self.inode_id as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
        h = h.wrapping_add(h << 3);
        h ^= h >> 11;
        h = h.wrapping_add(h << 15);
        (h as usize) % SHARDS
    }
}

// ── Lock types ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockType {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockOwner {
    Posix { owner_id: usize, owner_pid: i32 },
    Ofd { open_file_id: usize },
}

// ── Range lock record ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockRecord {
    pub owner: LockOwner,
    pub owner_pid: i32,
    pub lock_type: LockType,
    pub start: i64,
    pub end: i64, // inclusive
}

// ── Entry state ──────────────────────────────────────────────────────────

#[derive(Default)]
pub struct EntryState {
    pub records: Vec<LockRecord>,
}

// ── PosixLockEntry ───────────────────────────────────────────────────────

pub struct PosixLockEntry {
    /// Lock state (sorted list of non-overlapping ranges).
    pub state: Mutex<EntryState>,
    /// Wait queue for blocked F_SETLKW callers.
    pub waitq: Mutex<WaitQueue>,
}

// ── Shard map ────────────────────────────────────────────────────────────

struct ShardMap(Mutex<BTreeMap<LockKey, Arc<PosixLockEntry>>>);

/// Wait-graph: waiter_id -> { blocker_id -> count }.
/// Used for deadlock detection in F_SETLKW.
type EdgeMap = BTreeMap<usize, BTreeMap<usize, usize>>;

pub struct PosixLockManager {
    shards: Vec<ShardMap>,
    wait_graph: Mutex<EdgeMap>,
}

static MANAGER: spin::once::Once<PosixLockManager> = spin::once::Once::new();

pub fn mgr() -> &'static PosixLockManager {
    MANAGER
        .get()
        .expect("PosixLockManager not initialised; call init_posix_lock_manager first")
}

pub fn init_posix_lock_manager() {
    MANAGER.call_once(|| PosixLockManager {
        shards: (0..SHARDS)
            .map(|_| ShardMap(Mutex::new(BTreeMap::new())))
            .collect(),
        wait_graph: Mutex::new(BTreeMap::new()),
    });
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn same_owner(a: LockOwner, b: LockOwner) -> bool {
    match (a, b) {
        (LockOwner::Posix { owner_id: a_id, .. }, LockOwner::Posix { owner_id: b_id, .. }) => {
            a_id == b_id
        }
        (LockOwner::Ofd { open_file_id: a_id }, LockOwner::Ofd { open_file_id: b_id }) => {
            a_id == b_id
        }
        _ => false,
    }
}

/// Convert a LockOwner to a graph-safe waiter/blocker ID for deadlock detection.
/// OFD IDs are tagged with bit 62 to avoid collision with POSIX owner IDs.
#[inline]
fn owner_graph_id(owner: LockOwner) -> usize {
    match owner {
        LockOwner::Posix { owner_id, .. } => owner_id,
        LockOwner::Ofd { open_file_id } => open_file_id ^ (1usize << 62),
    }
}

fn conflict(a: LockType, a_s: i64, a_e: i64, b: LockType, b_s: i64, b_e: i64) -> bool {
    if a_e < b_s || b_e < a_s {
        return false;
    }
    a == LockType::Write || b == LockType::Write
}

// ── Range manipulation ───────────────────────────────────────────────────

/// Parse a `PosixFlock` into a `(start, end)` inclusive pair.
fn resolve_range(file: &File, flock: &PosixFlock) -> Result<(i64, i64), SyscallErr> {
    let base: i128 = match flock.l_whence {
        SEEK_SET => 0,
        SEEK_CUR => file.offset() as i128,
        SEEK_END => file.metadata().map_err(|_| SyscallErr::EINVAL)?.size as i128,
        _ => return Err(SyscallErr::EINVAL),
    };
    let start = base
        .checked_add(flock.l_start as i128)
        .ok_or(SyscallErr::EOVERFLOW)?;
    let len = flock.l_len as i128;
    let (s, l) = if len < 0 {
        (start.checked_add(len).ok_or(SyscallErr::EOVERFLOW)?, -len)
    } else {
        (start, len)
    };
    if s < 0 || l > i64::MAX as i128 {
        return Err(SyscallErr::EINVAL);
    }
    let end = if l == 0 {
        i64::MAX
    } else {
        let e = s.checked_add(l).ok_or(SyscallErr::EOVERFLOW)?;
        if e > i64::MAX as i128 {
            i64::MAX
        } else {
            (e - 1) as i64
        }
    };
    Ok((s as i64, end))
}

/// Remove all locks of `owner` that overlap `[start, end]` (inclusive),
/// trimming any non-overlapping portions.  Then coalesce.
fn remove_range(state: &mut EntryState, owner: LockOwner, start: i64, end: i64) {
    let mut v = Vec::new();
    for r in &state.records {
        if !same_owner(r.owner, owner) {
            v.push(*r);
            continue;
        }
        if r.end < start || r.start > end {
            v.push(*r);
        } else {
            if r.start < start {
                v.push(LockRecord {
                    start: r.start,
                    end: start.saturating_sub(1),
                    ..*r
                });
            }
            if r.end > end {
                v.push(LockRecord {
                    start: end.saturating_add(1),
                    end: r.end,
                    ..*r
                });
            }
        }
    }
    state.records = v;
    coalesce(state);
}

/// Sort by start and merge adjacent/overlapping records of the same
/// owner and type.
fn coalesce(state: &mut EntryState) {
    state.records.sort_by_key(|r| r.start);
    let mut i = 0;
    while i + 1 < state.records.len() {
        let a = state.records[i];
        let b = state.records[i + 1];
        if same_owner(a.owner, b.owner)
            && a.lock_type == b.lock_type
            && a.end.saturating_add(1) >= b.start
        {
            state.records[i].end = state.records[i].end.max(b.end);
            state.records.remove(i + 1);
        } else {
            i += 1;
        }
    }
}

/// Try to apply a lock (or unlock) to the entry state.
/// Returns `true` on success, `false` if a conflicting lock from a
/// *different* owner exists.
fn apply_lock(
    state: &mut EntryState,
    owner: LockOwner,
    owner_pid: i32,
    ltype: i16,
    start: i64,
    end: i64,
) -> bool {
    let lt = match ltype {
        F_RDLCK => LockType::Read,
        F_WRLCK => LockType::Write,
        _ => {
            // F_UNLCK — just remove the range.
            remove_range(state, owner, start, end);
            return true;
        }
    };

    // Check conflicts with DIFFERENT owners only.
    for r in &state.records {
        if !same_owner(r.owner, owner) && conflict(r.lock_type, r.start, r.end, lt, start, end) {
            return false;
        }
    }

    // Remove all same-owner locks in range, then insert new.
    remove_range(state, owner, start, end);
    state.records.push(LockRecord {
        owner,
        owner_pid,
        lock_type: lt,
        start,
        end,
    });
    coalesce(state);
    true
}

/// Validate that `file` has the required access mode for the lock type.
fn validate_access(file: &File, ltype: i16) -> Result<(), SyscallErr> {
    match ltype {
        F_RDLCK => file.readable(),
        F_WRLCK => file.writable(),
        F_UNLCK => Ok(()),
        _ => Err(SyscallErr::EINVAL),
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// F_GETLK / F_OFD_GETLK: report the first lock that would conflict with the given
/// `flock`.  The result is written back into `flock` in-place.
pub fn posix_lock_get(
    file: &File,
    owner: LockOwner,
    flock: &mut PosixFlock,
) -> Result<(), SyscallErr> {
    let key = LockKey::from_file(file);
    let map = mgr().shards[key.shard()].0.lock();
    let entry = match map.get(&key) {
        Some(e) => e.clone(),
        None => {
            flock.l_type = F_UNLCK;
            return Ok(());
        }
    };
    let state = entry.state.lock();
    let (s, e) = resolve_range(file, flock)?;

    let query_type = match flock.l_type {
        F_RDLCK => LockType::Read,
        F_WRLCK => LockType::Write,
        _ => LockType::Read,
    };
    let conflict = state.records.iter().find(|r| {
        !same_owner(r.owner, owner) && conflict(r.lock_type, r.start, r.end, query_type, s, e)
    });

    match conflict {
        Some(r) => {
            flock.l_type = if r.lock_type == LockType::Write {
                F_WRLCK
            } else {
                F_RDLCK
            };
            flock.l_pid = r.owner_pid;
            flock.l_start = r.start;
            flock.l_len = if r.end == i64::MAX {
                0
            } else {
                r.end - r.start + 1
            };
            flock.l_whence = SEEK_SET;
        }
        None => {
            flock.l_type = F_UNLCK;
        }
    }
    Ok(())
}

/// F_SETLK / F_SETLKW / F_OFD_SETLK / F_OFD_SETLKW: acquire or release a
/// record lock.
///
/// - `owner`  — `LockOwner::Posix` for F_SETLK/SETLKW,
///              `LockOwner::Ofd` for F_OFD_SETLK/SETLKW.
/// - `blocking`  — `true` for SETLKW (block until available).
pub fn posix_lock_set(
    file: &File,
    owner: LockOwner,
    flock: &PosixFlock,
    blocking: bool,
) -> Result<(), SyscallErr> {
    if flock.l_type != F_UNLCK {
        validate_access(file, flock.l_type)?;
    }

    let query_type = match flock.l_type {
        F_RDLCK => LockType::Read,
        F_WRLCK => LockType::Write,
        _ => LockType::Read,
    };
    let was_unlock = flock.l_type == F_UNLCK;

    let key = LockKey::from_file(file);
    let (s, e) = resolve_range(file, flock)?;

    let owner_pid = match owner {
        LockOwner::Posix { owner_pid, .. } => owner_pid,
        LockOwner::Ofd { .. } => -1,
    };

    // Get or create the shard entry.
    let mut map = mgr().shards[key.shard()].0.lock();
    let entry = map
        .entry(key)
        .or_insert_with(|| {
            Arc::new(PosixLockEntry {
                state: Mutex::new(EntryState::default()),
                waitq: Mutex::new(WaitQueue::new()),
            })
        })
        .clone();
    drop(map);

    // ── Non-blocking path ────────────────────────────────────────────
    if !blocking {
        let mut state = entry.state.lock();
        if apply_lock(&mut state, owner, owner_pid, flock.l_type, s, e) {
            if was_unlock {
                drop(state);
                entry.waitq.lock().wake_all();
            }
            return Ok(());
        }
        return Err(SyscallErr::EAGAIN);
    }

    // ── Blocking F_SETLKW — retry loop with deadlock detection ───────
    let waiter_id = owner_graph_id(owner);
    loop {
        mgr().wait_graph.lock().remove(&waiter_id);

        let mut state = entry.state.lock();
        if apply_lock(&mut state, owner, owner_pid, flock.l_type, s, e) {
            if was_unlock {
                drop(state);
                entry.waitq.lock().wake_all();
            }
            return Ok(());
        }
        drop(state);

        let edeadlk_sentinel = -(SyscallErr::EDEADLK as isize);
        let result = WaitQueue::wait_event_interruptible(&entry.waitq, || {
            // Re-evaluate the FULL condition on every wake:
            // try lock → find blocker → update wait_graph → deadlock check.
            let mut state = entry.state.lock();
            if apply_lock(&mut state, owner, owner_pid, flock.l_type, s, e) {
                return Some(0);
            }

            // Find current blocker and update wait graph.
            let blocker_id = state
                .records
                .iter()
                .find(|r| {
                    !same_owner(r.owner, owner)
                        && conflict(r.lock_type, r.start, r.end, query_type, s, e)
                })
                .map(|r| owner_graph_id(r.owner));
            drop(state);

            if let Some(bid) = blocker_id {
                let mut wg = mgr().wait_graph.lock();
                wg.entry(waiter_id)
                    .or_default()
                    .entry(bid)
                    .and_modify(|c| *c += 1)
                    .or_insert(1);

                // DFS: does blocker transitively wait for us?
                let mut visited = BTreeSet::new();
                let mut stack = vec![bid];
                let mut deadlock = false;
                while let Some(n) = stack.pop() {
                    if n == waiter_id {
                        deadlock = true;
                        break;
                    }
                    if visited.insert(n) {
                        if let Some(edges) = wg.get(&n) {
                            for &t in edges.keys() {
                                stack.push(t);
                            }
                        }
                    }
                }

                if deadlock {
                    wg.remove(&waiter_id);
                    return Some(edeadlk_sentinel);
                }
            }

            None
        });

        match result {
            WaitResult::Interrupted => {
                mgr().wait_graph.lock().remove(&waiter_id);
                return Err(SyscallErr::EINTR);
            }
            WaitResult::Ready(v) if v == edeadlk_sentinel => {
                return Err(SyscallErr::EDEADLK);
            }
            WaitResult::Ready(_) => {
                mgr().wait_graph.lock().remove(&waiter_id);
                return Ok(());
            }
            _ => {}
        }
    }
}

/// Release all POSIX record locks for the given `owner_id` on `file`.
///
/// Called from `FdTable::drop_fd` to ensure locks are dropped when a
/// file descriptor is closed.
pub fn release_posix_for_owner(file: &File, owner_id: usize) {
    let key = LockKey::from_file(file);
    let mut map = mgr().shards[key.shard()].0.lock();
    if let Some(entry) = map.get(&key) {
        let mut state = entry.state.lock();
        let before = state.records.len();
        state.records.retain(|r| match r.owner {
            LockOwner::Posix { owner_id: id, .. } => id != owner_id,
            _ => true,
        });
        if state.records.len() < before {
            // We removed something — wake waiters.
            let _ = entry.waitq.lock().wake_all();
        }
        if state.records.is_empty() {
            drop(state);
            map.remove(&key);
        }
    }
}

/// Release all OFD record locks for the given `file`.
///
/// Called from `File::drop` to ensure OFD locks are released when the last
/// reference to an open file description is dropped.
pub fn release_ofd_for_file(file: &File) {
    let key = LockKey::from_file(file);
    let open_file_id = file.open_file_id();
    let mut map = mgr().shards[key.shard()].0.lock();
    if let Some(entry) = map.get(&key) {
        let mut state = entry.state.lock();
        let before = state.records.len();
        state.records.retain(|r| match r.owner {
            LockOwner::Ofd { open_file_id: id } => id != open_file_id,
            _ => true,
        });
        if state.records.len() < before {
            // We removed something — wake waiters.
            let _ = entry.waitq.lock().wake_all();
        }
        if state.records.is_empty() {
            drop(state);
            map.remove(&key);
        }
    }
}
