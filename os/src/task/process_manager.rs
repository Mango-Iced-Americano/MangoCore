//! 进程查找、wait 和进程组信号投递入口。
//!
//! `ProcessManager` 是 syscall 层使用的静态门面：它不持有状态，只把 registry、
//! child wait 队列和信号投递组合成 Linux 兼容的进程级操作。

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;

use crate::syscall::{errno::*, CloneFlags};

use super::signal::Signals;
use super::{
    add_task, current_task_ref, quota, registry, signal::send_process_signal, ProcessControlBlock,
    ProcessState, TaskControlBlock, WaitQueue, WaitResult,
};

#[derive(Clone, Copy, Debug)]
/// `wait4`/`waitid` 内部返回的子进程状态。
pub(crate) struct WaitChildResult {
    /// 被匹配到的子进程 PID。
    pub pid: usize,
    /// Linux wait status 编码。
    pub status: u32,
}

/// 进程管理静态门面。
pub struct ProcessManager;

impl ProcessManager {
    /// 返回当前任务所属进程。
    pub fn current_process() -> Option<Arc<ProcessControlBlock>> {
        current_task_ref().map(|task| task.process.clone())
    }

    /// 按 PID 查找进程。
    pub fn find_process(pid: usize) -> Option<Arc<ProcessControlBlock>> {
        registry::find_process_by_pid(pid)
    }

    /// 按 TID 查找非 zombie 任务。
    pub fn find_task(tid: usize) -> Option<Arc<TaskControlBlock>> {
        registry::find_task_by_tid(tid)
    }

    /// 在指定进程内按 TID 查找任务。
    pub fn find_task_in_process(pid: usize, tid: usize) -> Option<Arc<TaskControlBlock>> {
        registry::find_task_by_pid_tid(pid, tid)
    }

    /// 返回所有仍存活的进程引用。
    pub fn all_processes() -> Vec<Arc<ProcessControlBlock>> {
        registry::all_processes()
    }

    /// 返回指定进程组内的进程。
    pub fn find_processes_by_pgid(pgid: usize) -> Vec<Arc<ProcessControlBlock>> {
        registry::find_processes_by_pgid(pgid)
    }

    /// 返回当前任务 quota 使用数，饱和到 `u16`。
    pub fn process_count() -> u16 {
        quota::allocated_task_count().min(u16::MAX as usize) as u16
    }

    /// 将已构造的 clone 子进程发布到父进程 child tree。
    ///
    /// # Errors
    ///
    /// 子进程向父进程 children 列表扩容失败时返回 `-ENOMEM`。
    pub fn publish_clone_child(
        parent: &Arc<TaskControlBlock>,
        child: Arc<TaskControlBlock>,
        flags: CloneFlags,
    ) -> Result<(), isize> {
        parent.publish_clone_child(child, flags)
    }

    /// 将已 publish 的子进程加入调度器并进入就绪队列。
    /// 调用之后 child 已存活，不可再走 unpublished cleanup 回滚。
    /// vfork 等待使用不可中断 completion —— 若用 Interrupted 循环重试，
    /// 父进程在有 actionable signal 时会在内核自旋，子进程无法被调度。
    pub fn schedule_clone_child(
        parent: &Arc<TaskControlBlock>,
        child: Arc<TaskControlBlock>,
        flags: CloneFlags,
    ) {
        if flags.contains(CloneFlags::CLONE_VFORK) {
            child.process.set_vfork_parent(parent);
            add_task(child.clone());
            child.process.wait_vfork_done_uninterruptible();
        } else {
            add_task(child);
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
        // `try_reap_child` 在持有 `process.inner` 时只检查/移动 child 列表；
        // 真正释放 quota、PID 和 zombie TCB 的路径会避免持有 TASK_MANAGER 锁。
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

        fn child_is_ptraced(child: &Arc<ProcessControlBlock>) -> bool {
            child
                .any_live_thread()
                .map(|task| task.acquire_inner_lock().ptrace_traceme)
                .unwrap_or(false)
        }

        let wait_status = Cell::new(0);
        let try_wait_attached_tracee = || -> Option<isize> {
            if pid <= 0 {
                return None;
            }
            let tracee = Self::find_process(pid as usize)?;
            if !tracee.ptrace_traced_by(process.pid) {
                return None;
            }
            if report_stopped || report_exited {
                if let Some(status) = tracee.take_stopped_status(nowait) {
                    wait_status.set(status);
                    return Some(tracee.pid as isize);
                }
            }
            None
        };
        let try_reap_child = || -> Option<isize> {
            let mut process_inner = process.acquire_inner_lock();
            let caller_pgid = process_inner.pgid;

            let mut has_matching_child = false;
            let mut zombie_idx = None;
            for (idx, child) in process_inner.children.iter().enumerate() {
                let child_inner = child.acquire_inner_lock();
                let matched = child_matches_pid(child.pid, child_inner.pgid, caller_pgid, pid);
                let is_zombie = child_inner.state == ProcessState::Zombie;
                drop(child_inner);
                if !matched {
                    continue;
                }
                has_matching_child = true;
                if report_stopped || (report_exited && child_is_ptraced(child)) {
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
                if report_exited && is_zombie && zombie_idx.is_none() {
                    zombie_idx = Some(idx);
                }
            }

            if !has_matching_child {
                if let Some(value) = try_wait_attached_tracee() {
                    return Some(value);
                }
                return Some(ECHILD);
            }
            if !report_exited {
                return None;
            }
            if let Some(idx) = zombie_idx {
                let child = if nowait {
                    process_inner.children[idx].clone()
                } else {
                    process_inner.children.swap_remove(idx)
                };
                let found_pid = child.pid;
                wait_status.set(child.exit_code());
                if !nowait {
                    child.release_pid();
                    process_inner.child_rusage.add_child(child.wait_rusage());
                    child.set_parent(None);
                    registry::unregister_process(child.pid);
                    // 立即释放 clone quota —— 不等 zombie TCB 被调度器清理
                    child.release_process_quota_once();
                    // 同步从调度队列清除 zombie TCB，释放 PCB 及关联资源
                    crate::task::remove_zombie_tasks_by_pid(child.pid);
                }
                Some(found_pid as isize)
            } else {
                if let Some(value) = try_wait_attached_tracee() {
                    return Some(value);
                }
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

        // `child_exit_wait` 由子进程 `finish_exit`、stop/continue 事件和 ptrace
        // stop 唤醒。条件闭包必须可重复执行，且返回 `ECHILD` 作为 Ready 值。
        match WaitQueue::wait_event_interruptible(&process.child_exit_wait, || try_reap_child()) {
            WaitResult::Ready(value) => decode(value),
            WaitResult::Interrupted => Err(ERESTART),
            WaitResult::TimedOut => Ok(None),
        }
    }

    /// 向指定 PID 进程投递信号。
    pub fn send_signal_to_process(pid: usize, signal: Signals) -> isize {
        if let Some(process) = Self::find_process(pid) {
            send_process_signal(&process, signal);
            SUCCESS
        } else {
            ESRCH
        }
    }

    /// 向当前进程所在进程组投递信号。
    pub fn send_signal_to_current_group(signal: Signals) -> isize {
        let process = match Self::current_process() {
            Some(process) => process,
            None => return ESRCH,
        };
        Self::send_signal_to_group(process.getpgid(), signal)
    }

    /// 向指定进程组投递信号。
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

    /// 向除 init 和当前进程外的所有进程投递信号。
    pub fn send_signal_to_all(signal: Signals) -> isize {
        let current_pid = current_task_ref().map(|task| task.pid()).unwrap_or(0);
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
