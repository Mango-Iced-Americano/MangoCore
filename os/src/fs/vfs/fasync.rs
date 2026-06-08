use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;

use crate::fs::vfs::File;
use crate::fs::vfs::file::{FileOwnerSnapshot, FileOwnerTarget};
use crate::fs::vfs::FileFlags;
use crate::task::{
    find_process_by_pid, find_task_by_tid,
    send_process_signal,
    send_thread_signal,
    signal::Signals,
    ProcessManager,
};
use crate::utils::error::SyscallErr;

/// An entry in the fasync list, tracking a file descriptor registered for
/// asynchronous I/O notification (SIGIO).
#[derive(Clone)]
pub struct FAsyncItem {
    file: Weak<File>,
    fd: i32,
}

/// Per-inode list of registered fasync watchers.
///
/// When I/O readiness changes (e.g., data written to pipe), the inode walks
/// this list and sends SIGIO (or the configured signal) to each registered
/// file owner that still has O_ASYNC set.
pub struct FAsyncItems {
    items: Mutex<Vec<FAsyncItem>>,
}

impl FAsyncItems {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
        }
    }

    /// Register a file descriptor for async notification.
    /// Replaces any existing entry for the same fd.
    pub fn add(&self, file: &Arc<File>, fd: i32) {
        let mut items = self.items.lock();
        items.retain(|i| i.fd != fd);
        items.push(FAsyncItem {
            file: Arc::downgrade(file),
            fd,
        });
    }

    /// Unregister a file descriptor.
    pub fn remove(&self, fd: i32) {
        self.items.lock().retain(|i| i.fd != fd);
    }

    /// Send signal to all registered file owners.
    ///
    /// Follows the DragonOS pattern: snap the owner info BEFORE dropping
    /// the fasync lock.  Only files that still have `O_ASYNC` set receive
    /// the signal (the flag may have been cleared between registration and
    /// delivery).
    ///
    /// `signum_override` selects the signal number.  When `None`, the
    /// per-file `signum` (set via `F_SETSIG`) is used, falling back to
    /// `SIGIO` (29) when the stored value is out of range.
    pub fn send_sigio(&self, signum_override: Option<i32>) {
        let items: Vec<FAsyncItem> = self.items.lock().clone();
        for item in &items {
            if let Some(file) = item.file.upgrade() {
                if file.flags().contains(FileFlags::O_ASYNC) {
                    let snapshot = file.owner_snapshot();
                    drop(file);

                    let signum = signum_override.unwrap_or(snapshot.signum);
                    let sig = if signum > 0 && signum <= 64 {
                        match Signals::from_signum(signum as usize) {
                            Ok(s) => s,
                            Err(_) => Signals::SIGIO,
                        }
                    } else {
                        Signals::SIGIO
                    };

                    match &snapshot.target {
                        FileOwnerTarget::Pid(pid) => {
                            if let Some(pcb) = find_process_by_pid(*pid) {
                                send_process_signal(&pcb, sig);
                            }
                        }
                        FileOwnerTarget::Pgrp(pgid) => {
                            ProcessManager::send_signal_to_group(*pgid, sig);
                        }
                        FileOwnerTarget::Tid(tid) => {
                            if let Some(task) = find_task_by_tid(*tid) {
                                let _ = send_thread_signal(&task, sig);
                            }
                        }
                        FileOwnerTarget::None => {}
                    }
                }
            }
        }
    }
}

/// Enable or disable fasync notification for a file descriptor on its
/// associated inode.
///
/// Called from `fcntl(F_SETFL)` when the `O_ASYNC` bit toggles.  Inodes
/// that do not support fasync (default for most FS inodes) silently
/// return `Ok(())`.
pub fn set_file_fasync(
    file: &Arc<File>,
    fd: i32,
    enabled: bool,
) -> Result<(), SyscallErr> {
    let fasync_items = match file.inode.fasync_items() {
        Some(items) => items,
        None => return Ok(()),
    };
    if enabled {
        fasync_items.add(file, fd);
    } else {
        fasync_items.remove(fd);
    }
    Ok(())
}
