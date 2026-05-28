mod completion;
mod context;
mod elf;
mod manager;
use spin::MutexGuard;
pub mod pid;
mod process;
mod process_manager;
mod processor;
mod registry;
mod sleep;
pub mod signal;
mod task;
pub mod threads;

use crate::hal::__switch;
use crate::fs::{self, vfs_lookup_absolute};
use alloc::{sync::Arc, vec::Vec};
pub use context::TaskContext;
pub use elf::{load_elf_interp, AuxvEntry, AuxvType, ELFInfo};
use lazy_static::*;
use manager::fetch_task;
pub use completion::Completion;
pub use manager::{
    add_kernel_timer, add_task, all_pids, do_oom, do_wake_expired, drain_ready_zombies,
    kernel_timer_queue_len, procs_count, remove_tasks_from_queues,
    send_signal_to_interruptible, sleep_interruptible, task_manager_counts, update_ready_nice,
    wait_with_timeout, wake_interruptible, zombie_count, TimerAction, WaitQueue, WaitResult,
};
// pub use pid::RecycleAllocator;
pub use pid::{
    ns_last_pid, set_ns_last_pid, tid_alloc, trap_cx_bottom_from_slot, ustack_bottom_from_slot,
    TidHandle,
};
pub use processor::{
    check_oom_kill, current_syscall_name, current_task, current_trap_cx, current_user_token,
    run_tasks, schedule, set_current_syscall_id, take_current_task,
};
pub use process::{is_executable_inode_busy, ProcessControlBlock, ProcessState};
pub use process_manager::ProcessManager;
pub use registry::{
    all_processes, find_process_by_pid, find_processes_by_pgid, find_task_by_pid_tid,
    find_task_by_tid,
};
pub use signal::*;
pub use sleep::{sleep_relative_interruptible, sleep_until_interruptible};
pub use task::{
    FsStatus, PosixTimer, RobustList, Rusage, TaskControlBlock, TaskStatus, UtsNamespace,
};

pub use self::processor::PROCESSOR;
#[allow(unused)]
pub fn try_yield() {
    let lock = PROCESSOR.lock();
    let mut do_suspend = false;
    if !lock.is_vacant() {
        do_suspend = true;
    }
    drop(lock);
    if do_suspend {
        suspend_current_and_run_next()
    }
}
pub fn suspend_current_and_run_next() {
    // There must be an application running.
    let task = take_current_task().unwrap();

    // ---- hold current PCB lock
    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready
    task_inner.task_status = TaskStatus::Ready;
    drop(task_inner);
    // ---- release current PCB lock

    // push back to ready queue.
    add_task(task);
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

pub(crate) fn block_current_and_run_next() {
    // There must be an application running.
    let task = take_current_task().unwrap();

    // ---- hold current PCB lock
    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Interruptible
    task_inner.task_status = TaskStatus::Interruptible;
    drop(task_inner);
    // ---- release current PCB lock

    // push to interruptible queue of scheduler, so that it won't be scheduled.
    sleep_interruptible(task);
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

/// 先把当前任务放入 interruptible 队列，再执行一次调用方提供的阻塞条件检查。
/// 这用于信号等待这类路径，避免信号在“检查 pending”和“进入睡眠队列”
/// 之间到达时丢失唤醒。
pub(crate) fn block_current_and_run_next_checked(
    should_block: impl FnOnce(&Arc<TaskControlBlock>) -> bool,
) {
    let task = take_current_task().unwrap();

    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.task_status = TaskStatus::Interruptible;
    drop(task_inner);

    sleep_interruptible(task.clone());
    if !should_block(&task) {
        let mut task_inner = task.acquire_inner_lock();
        if task_inner.task_status == TaskStatus::Interruptible {
            task_inner.task_status = TaskStatus::Ready;
            drop(task_inner);
            wake_interruptible(task.clone());
        }
    }
    schedule(task_cx_ptr);
}

// 带释放锁的阻塞调度，确保任务真正进入 interruptible_queue 后再丢锁，
// 避免在丢锁到睡眠之间丢失唤醒。
// 注意不要重复丢锁。
pub(crate) fn block_current_and_run_next_with_lock<T>(lock: MutexGuard<'_, T>) {
    // There must be an application running.
    let task = take_current_task().unwrap();

    // ---- hold current PCB lock
    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;

    task_inner.task_status = TaskStatus::Interruptible;

    drop(task_inner);
    // ---- release current PCB lock

    // push to interruptible queue of scheduler, so that it won't be scheduled.
    sleep_interruptible(task);
    drop(lock);
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

// 带释放锁和阻塞条件复查的调度入口。
// WaitQueue 使用它保证“入队 -> 条件复查 -> 睡眠”之间不会丢失唤醒。
pub(crate) fn block_current_and_run_next_with_lock_checked<T>(
    lock: MutexGuard<'_, T>,
    should_block: impl FnOnce(&Arc<TaskControlBlock>) -> bool,
) {
    let task = take_current_task().unwrap();

    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.task_status = TaskStatus::Interruptible;
    drop(task_inner);

    sleep_interruptible(task.clone());
    if !should_block(&task) {
        let mut task_inner = task.acquire_inner_lock();
        if task_inner.task_status == TaskStatus::Interruptible {
            task_inner.task_status = TaskStatus::Ready;
            drop(task_inner);
            wake_interruptible(task.clone());
        }
    }
    drop(lock);
    schedule(task_cx_ptr);
}

fn do_exit(task: Arc<TaskControlBlock>, exit_code: u32) {
    if task.exit_thread_resources(exit_code) && task.process.live_thread_count() == 0 {
        task.process.finish_exit(&task, exit_code);
    }
}

pub fn exit_current_and_run_next(exit_code: u32) -> ! {
    let task = take_current_task().unwrap();
    do_exit(task.clone(), exit_code);
    // 当前任务仍在自己的内核栈上运行，不能在切栈前释放最后一个 Arc。
    add_task(task);
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
    panic!("Unreachable");
}

pub fn exit_group_and_run_next(exit_code: u32) -> ! {
    let task = take_current_task().unwrap();
    let process = task.process.clone();
    process.request_group_exit(exit_code);
    let exit_list: Vec<_> = process
        .threads()
        .into_iter()
        .filter(|thread| thread.tid.0 != task.tid.0)
        .collect();
    manager::remove_tasks_from_queues(&exit_list);

    for task in exit_list.into_iter() {
        task.exit_thread_resources(exit_code);
    }
    do_exit(task.clone(), exit_code);
    // 当前任务仍在自己的内核栈上运行，不能在切栈前释放最后一个 Arc。
    add_task(task);
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
    panic!("Unreachable");
}

lazy_static! {
    pub static ref INITPROC: Arc<TaskControlBlock> = {
        let inode = vfs_lookup_absolute("/initproc").unwrap();
        let elf = fs::vfs::File::new(inode, fs::vfs::FileFlags::O_RDONLY).unwrap();
        TaskControlBlock::new(elf)
    };
}

pub fn add_initproc() {
    add_task(INITPROC.clone());
}
