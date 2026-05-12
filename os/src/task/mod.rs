mod context;
mod elf;
mod manager;
pub use manager::WaitQueue;
use spin::MutexGuard;
pub mod pid;
mod processor;
pub mod signal;
mod task;
pub mod threads;

use crate::hal::__switch;
use crate::{
    fs::{OpenFlags, ROOT_FD},
    mm::translated_ref_write,
    timer::TimeSpec,
    utils::error::{GeneralRet, SyscallErr},
};
use alloc::{collections::VecDeque, sync::Arc};
pub use context::TaskContext;
pub use elf::{load_elf_interp, AuxvEntry, AuxvType, ELFInfo};
use lazy_static::*;
use log::warn;
use manager::fetch_task;
pub use manager::{
    add_kernel_timer, add_task, do_oom, do_wake_expired, find_task_by_pid, find_task_by_tgid,
    kernel_timer_queue_len, procs_count, send_signal_to_interruptible, sleep_interruptible,
    task_manager_counts, wait_with_timeout, wake_interruptible, zombie_count, TimerAction,
};
// pub use pid::RecycleAllocator;
pub use pid::{pid_alloc, trap_cx_bottom_from_tid, ustack_bottom_from_tid, PidHandle};
pub use processor::{
    check_oom_kill, current_syscall_name, current_task, current_trap_cx, current_user_token,
    run_tasks, schedule, set_current_syscall_id, take_current_task,
};
pub use signal::*;
pub use task::{RobustList, Rusage, TaskControlBlock, TaskStatus};

use self::processor::PROCESSOR;
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

pub fn block_current_and_run_next() {
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

// 带释放锁的阻塞调度，确保任务真正进入 interruptible_queue 后再丢锁，
// 避免在丢锁到睡眠之间丢失唤醒。
// 注意不要重复丢锁。
pub fn block_current_and_run_next_with_lock<T>(lock: MutexGuard<'_, T>) {
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

// 判断该task的sigpending中是否已经有可操作的未遮蔽信号
// 被忽略的信号（SIG_IGN）或默认动作是忽略的信号（如SIGCHLD）不算
fn has_unblocked_signal(task: &Arc<TaskControlBlock>) -> bool {
    has_actionable_signal(task)
}

//等待一段时间直到达到deadline
pub fn wait_interruptible_timeout(deadline: TimeSpec) -> GeneralRet<()> {
    let task = current_task().unwrap();
    if has_unblocked_signal(&task) {
        return Err(SyscallErr::ERESTART);
    }
    if TimeSpec::now() >= deadline {
        return Ok(());
    }
    wait_with_timeout(Arc::downgrade(&task), deadline);
    block_current_and_run_next();
    if has_unblocked_signal(&task) {
        Err(SyscallErr::ERESTART)
    } else {
        Ok(())
    }
}

//等待直到下一个信号传来
pub fn wait_interruptible() -> GeneralRet<()> {
    let task = current_task().unwrap();
    //有信号则直接抛错退出
    if has_unblocked_signal(&task) {
        return Err(SyscallErr::ERESTART);
    }
    block_current_and_run_next();
    //醒后检查
    if has_unblocked_signal(&task) {
        Err(SyscallErr::ERESTART)
    } else {
        Ok(())
    }
}

pub fn do_exit(task: Arc<TaskControlBlock>, exit_code: u32) {
    // **** hold current PCB lock
    let mut inner = task.acquire_inner_lock();
    if !task.exit_signal.is_empty() {
        if let Some(parent) = inner.parent.as_ref() {
            let parent_task = parent.upgrade().unwrap(); // this will acquire inner of current task
            let mut parent_inner = parent_task.acquire_inner_lock();
            parent_inner.add_signal(task.exit_signal);

            if parent_inner.task_status == TaskStatus::Interruptible {
                // wake up parent if parent is waiting.
                parent_inner.task_status = TaskStatus::Ready;
                drop(parent_inner);
                // push back to ready queue.
                wake_interruptible(parent_task);
            }
        } else {
            warn!("[do_exit] parent is None");
        }
    }
    log::trace!(
        "[do_exit] Trying to exit pid {} with {}",
        task.pid.0,
        exit_code
    );
    // Change status to Zombie
    inner.task_status = TaskStatus::Zombie;
    // Record exit code
    inner.exit_code = exit_code;

    // move children to initproc
    if !inner.children.is_empty() {
        let mut initproc_inner = INITPROC.acquire_inner_lock();
        while let Some(child) = inner.children.pop() {
            child.acquire_inner_lock().parent = Some(Arc::downgrade(&INITPROC));
            initproc_inner.children.push(child);
        }
        if initproc_inner.task_status == TaskStatus::Interruptible {
            // wake up initproc if initproc is waiting.
            initproc_inner.task_status = TaskStatus::Ready;
            // push back to ready queue.
            wake_interruptible(INITPROC.clone());
        }
    }

    inner.children.clear();
    if inner.clear_child_tid != 0 {
        log::debug!(
            "[do_exit] do futex wake on clear_child_tid: {:X}",
            inner.clear_child_tid
        );
        //let phys_ref =
        match translated_ref_write(task.get_user_token(), inner.clear_child_tid as *mut u32) {
            Ok(phys_ref) => {
                *phys_ref = 0;
                task.futex.lock().wake(phys_ref as *const u32 as usize, 1);
            }
            Err(_) => log::warn!("invalid clear_child_tid"),
        };
    }
    // deallocate user resource (trap context and user stack)
    task.vm.lock().dealloc_user_res(task.tid);
    // deallocate whole user space in advance, or if its parent do not call wait,
    // this resource may not be recycled in a long period of time.
    if Arc::strong_count(&task.vm) == 1 {
        task.vm.lock().recycle_data_pages();
    }
    // 关闭所有文件描述符，释放管道/Socket等的 Arc 引用，
    // 确保读端能收到 EOF（all_write_ends_closed() == true）。
    // SocketFile 通过 fd_table 管理，无需额外清理。
    {
        let mut fd_table = task.files.lock();
        for fd_opt in fd_table.iter_mut() {
            *fd_opt = None;
        }
    }
    drop(inner);
    // **** release current PCB lock
    // drop task manually to maintain rc correctly
    log::info!("[do_exit] Pid {} exited with {}", task.pid.0, exit_code);

    // 打印资源统计诊断信息
    crate::utils::stats::print_resource_stats();
}

pub fn exit_current_and_run_next(exit_code: u32) -> ! {
    // take from Processor
    let task = take_current_task().unwrap();
    do_exit(task, exit_code);
    // we do not have to save task context
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
    panic!("Unreachable");
}

pub fn exit_group_and_run_next(exit_code: u32) -> ! {
    // exit current, take from Processor
    let task = take_current_task().unwrap();
    let tgid = task.tgid;
    do_exit(task, exit_code);

    let mut exit_list = VecDeque::new();

    let mut manager = manager::TASK_MANAGER.lock();
    let mut remain = manager.ready_queue.len();
    while let Some(task) = manager.ready_queue.pop_front() {
        if task.tgid == tgid {
            exit_list.push_back(task);
        } else {
            manager.ready_queue.push_back(task);
        }
        remain -= 1;
        if remain == 0 {
            break;
        }
    }
    let mut remain = manager.interruptible_queue.len();
    while let Some(task) = manager.interruptible_queue.pop_front() {
        if task.tgid == tgid {
            exit_list.push_back(task);
        } else {
            manager.interruptible_queue.push_back(task);
        }
        remain -= 1;
        if remain == 0 {
            break;
        }
    }
    drop(manager);

    for task in exit_list.into_iter() {
        do_exit(task, exit_code);
    }
    // we do not have to save task context
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
    panic!("Unreachable");
}

lazy_static! {
    pub static ref INITPROC: Arc<TaskControlBlock> = Arc::new({
        let elf = ROOT_FD.open("initproc", OpenFlags::O_RDONLY, true).unwrap();
        TaskControlBlock::new(elf)
    });
}

pub fn add_initproc() {
    add_task(INITPROC.clone());
}
