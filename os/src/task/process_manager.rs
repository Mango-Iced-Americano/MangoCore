use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;
use log::trace;

use crate::syscall::{errno::*, CloneFlags};

use super::signal::Signals;
use super::{
    add_task, current_task, manager::procs_count, registry, signal::send_process_signal,
    ProcessControlBlock, ProcessState, TaskControlBlock, WaitQueue, WaitResult,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct WaitChildResult {
    pub pid: usize,
    pub status: u32,
}

pub struct ProcessManager;

impl ProcessManager {
    pub fn current_process() -> Option<Arc<ProcessControlBlock>> {
        current_task().map(|task| task.process.clone())
    }

    pub fn find_process(pid: usize) -> Option<Arc<ProcessControlBlock>> {
        registry::find_process_by_pid(pid)
    }

    pub fn find_task(tid: usize) -> Option<Arc<TaskControlBlock>> {
        registry::find_task_by_tid(tid)
    }

    pub fn find_task_in_process(pid: usize, tid: usize) -> Option<Arc<TaskControlBlock>> {
        registry::find_task_by_pid_tid(pid, tid)
    }

    pub fn all_processes() -> Vec<Arc<ProcessControlBlock>> {
        registry::all_processes()
    }

    pub fn find_processes_by_pgid(pgid: usize) -> Vec<Arc<ProcessControlBlock>> {
        registry::find_processes_by_pgid(pgid)
    }

    pub fn process_count() -> u16 {
        procs_count()
    }

    pub fn publish_clone_child(
        parent: &Arc<TaskControlBlock>,
        child: Arc<TaskControlBlock>,
        flags: CloneFlags,
    ) -> Result<(), isize> {
        parent.publish_clone_child(child, flags)
    }

    pub fn schedule_clone_child(
        parent: &Arc<TaskControlBlock>,
        child: Arc<TaskControlBlock>,
        flags: CloneFlags,
    ) -> Result<(), isize> {
        if flags.contains(CloneFlags::CLONE_VFORK) {
            child.process.set_vfork_parent(parent);
        }
        add_task(child.clone());
        if flags.contains(CloneFlags::CLONE_VFORK) {
            match child.process.wait_vfork_done_interruptible() {
                WaitResult::Ready(_) => Ok(()),
                WaitResult::Interrupted => Err(ERESTART),
                WaitResult::TimedOut => Ok(()),
            }
        } else {
            Ok(())
        }
    }

    pub(crate) fn wait_child(
        process: &Arc<ProcessControlBlock>,
        pid: isize,
        nohang: bool,
        report_exited: bool,
        report_stopped: bool,
        report_continued: bool,
        nowait: bool,
    ) -> Result<Option<WaitChildResult>, isize> {
        fn child_matches_pid(
            child_pid: usize,
            child_pgid: usize,
            caller_pgid: usize,
            pid: isize,
        ) -> bool {
            if pid == -1 {
                true
            } else if pid > 0 {
                pid as usize == child_pid
            } else if pid == 0 {
                child_pgid == caller_pgid
            } else {
                child_pgid == (-pid) as usize
            }
        }

        let wait_status = Cell::new(0);
        let try_reap_child = || -> Option<isize> {
            let mut process_inner = process.acquire_inner_lock();
            let caller_pgid = process_inner.pgid;

            let has_child = process_inner.children.iter().any(|child| {
                let child_inner = child.acquire_inner_lock();
                child_matches_pid(child.pid, child_inner.pgid, caller_pgid, pid)
            });
            if !has_child {
                return Some(ECHILD);
            }

            for child in process_inner.children.iter() {
                let child_inner = child.acquire_inner_lock();
                let matched = child_matches_pid(child.pid, child_inner.pgid, caller_pgid, pid);
                drop(child_inner);
                if !matched {
                    continue;
                }
                if report_stopped {
                    if let Some(status) = child.take_stopped_status(nowait) {
                        wait_status.set(status);
                        return Some(child.pid as isize);
                    }
                }
                if report_continued {
                    if let Some(status) = child.take_continued_status(nowait) {
                        wait_status.set(status);
                        return Some(child.pid as isize);
                    }
                }
            }

            if !report_exited {
                return None;
            }

            let pair = process_inner.children.iter().enumerate().find(|(_, child)| {
                let child_inner = child.acquire_inner_lock();
                child_inner.state == ProcessState::Zombie
                    && child_matches_pid(child.pid, child_inner.pgid, caller_pgid, pid)
            });

            if let Some((idx, _)) = pair {
                let child = if nowait {
                    process_inner.children[idx].clone()
                } else {
                    process_inner.children.remove(idx)
                };
                if !nowait {
                    trace!(
                        "[wait4] release zombie process, leader_tid: {}, pid: {}",
                        child.leader_tid,
                        child.pid
                    );
                }
                let found_pid = child.pid;
                wait_status.set(child.exit_code());
                if !nowait {
                    child.release_pid();
                    process_inner.child_rusage.add_child(child.wait_rusage());
                }
                Some(found_pid as isize)
            } else {
                None
            }
        };

        let decode = |value: isize| {
            if value < 0 {
                Err(value)
            } else {
                Ok(Some(WaitChildResult {
                    pid: value as usize,
                    status: wait_status.get(),
                }))
            }
        };

        if nohang {
            return match try_reap_child() {
                Some(value) => decode(value),
                None => Ok(None),
            };
        }

        match WaitQueue::wait_event_interruptible(&process.child_exit_wait, || try_reap_child()) {
            WaitResult::Ready(value) => decode(value),
            WaitResult::Interrupted => Err(ERESTART),
            WaitResult::TimedOut => Ok(None),
        }
    }

    pub fn send_signal_to_process(pid: usize, signal: Signals) -> isize {
        if let Some(process) = Self::find_process(pid) {
            send_process_signal(&process, signal);
            SUCCESS
        } else {
            ESRCH
        }
    }

    pub fn send_signal_to_current_group(signal: Signals) -> isize {
        let process = match Self::current_process() {
            Some(process) => process,
            None => return ESRCH,
        };
        Self::send_signal_to_group(process.getpgid(), signal)
    }

    pub fn send_signal_to_group(pgid: usize, signal: Signals) -> isize {
        let targets = Self::find_processes_by_pgid(pgid);
        if targets.is_empty() {
            ESRCH
        } else {
            for process in targets {
                send_process_signal(&process, signal);
            }
            SUCCESS
        }
    }

    pub fn send_signal_to_all(signal: Signals) -> isize {
        let current_pid = current_task().map(|task| task.pid()).unwrap_or(0);
        let mut sent = false;
        for process in Self::all_processes() {
            if process.pid == 1 || process.pid == current_pid {
                continue;
            }
            send_process_signal(&process, signal);
            sent = true;
        }
        if sent {
            SUCCESS
        } else {
            ESRCH
        }
    }
}
