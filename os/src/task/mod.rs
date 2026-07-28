//! 任务、进程和调度子系统入口。
//!
//! 统一导出 TCB/PCB、调度队列、PID/TID、信号、namespace、sleep、timer、
//! WaitQueue 和当前任务访问接口。调度上下文已归属于 Per-CPU 状态，
//! 任务切换通过架构相关的 `__switch` 汇编完成。
//!
//! # Locking
//!
//! 阻塞路径必须先把任务状态和等待队列状态提交，再释放调用方传入的锁，
//! 以免在“条件检查”和“睡眠入队”之间丢失唤醒。信号检查也必须在释放
//! `TaskControlBlockInner` 锁后执行。

mod completion;
mod context;
mod elf;
pub mod ipc_namespace;
mod manager;
pub mod mount_namespace;
pub mod net_namespace;
use spin::MutexGuard;
pub mod perf;
pub mod pid;
mod process;
mod process_manager;
pub(crate) mod processor;
pub mod quota;
mod registry;
mod run_queue;
pub mod signal;
mod sleep;
mod task;
pub mod threads;

use crate::fs::{self, vfs_lookup_absolute};
use crate::hal::__switch;
use crate::mm::{AddressSpace, PageTableImpl};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
pub use completion::Completion;
pub use context::TaskContext;
pub use elf::{load_elf_interp, AuxvEntry, AuxvType, ELFInfo};
use lazy_static::*;
use manager::{fetch_task, finish_switch_out};
pub use manager::{
    add_kernel_timer, all_pids, do_oom, do_wake_expired, has_ready_task,
    has_zombie_queue_tasks_fast, kernel_timer_queue_len, procs_count, remove_tasks_from_queues,
    remove_zombie_tasks_by_pid, publish_task, run_deferred_timer_at_task_safe_point,
    run_deferred_timer_work, send_signal_to_interruptible, sleep_interruptible,
    take_one_interruptible_zombie, take_zombie_tasks, task_manager_counts,
    timer_interrupt_handler, timer_subsystem_init, update_ready_nice, wait_with_timeout,
    wake_interruptible, zombie_count, TimerAction, WaitQueue, WaitResult,
};
// pub use pid::RecycleAllocator;
pub use ipc_namespace::{IpcNamespace, INIT_IPC_NAMESPACE};
pub use mount_namespace::{MountNamespace, INIT_MOUNT_NAMESPACE};
pub use net_namespace::{NetNamespace, INIT_NET_NAMESPACE};
pub use pid::{
    ns_last_pid, set_ns_last_pid, tid_alloc, trap_cx_bottom_from_slot, ustack_bottom_from_slot,
    TidHandle,
};
pub use process::{
    is_executable_inode_busy, is_writable_inode_busy, register_writable_inode,
    unregister_writable_inode, ProcessControlBlock, ProcessState,
};
pub use process_manager::ProcessManager;
pub use processor::{
    current_egid, current_euid, current_gid, current_parent_pid, current_pgid, current_pid,
    current_sgid, current_sid, current_suid, current_syscall_name, current_task, current_tid,
    current_trap_cx, current_uid, current_user_token, run_tasks, schedule, set_current_syscall_id,
    try_current_user_token,
};
pub(crate) use processor::try_current_task;
pub use registry::{
    all_processes, find_process_by_pid, find_processes_by_pgid, find_task_by_pid_tid,
    find_task_by_tid,
};
pub use signal::*;
pub use sleep::{
    sleep_relative_interruptible, sleep_until_interruptible, sleep_until_realtime_interruptible,
    wake_realtime_abstime_sleepers_after_clock_set,
};
pub use task::{
    any_seccomp_enabled, FsStatus, PosixTimer, RobustList, Rusage, SeccompFilterInsn,
    TaskControlBlock, TaskStatus, UtsNamespace,
};

/// 返回指定 CPU 的精确 runqueue 长度，供诊断和 SMP focused test 使用。
pub(crate) fn run_queue_count(cpu: usize) -> usize {
    run_queue::stats(cpu).0
}

#[allow(unused)]
/// 在当前处理器已有运行任务时主动让出 CPU。
///
/// # Semantics
///
/// 若当前处理器处于空闲态则不做任何事；否则将当前任务重新放回 ready
/// 队列并切回调度器。
pub fn try_yield() {
    // `current_task()` 克隆出的临时 Arc 在条件判断后立即释放，
    // 不得跨越可能不返回的 context switch。
    if current_task().is_some() {
        suspend_current_and_run_next()
    }
}
/// 将当前任务切回 idle，由 idle 在切栈完成后交还 ready queue。
///
/// # Locking
///
/// 当前任务在 `__switch` 返回 idle 之前始终保持 `Running(cpu)`，避免仍使用
/// 自身内核栈时就被另一个调度循环再次选中。
pub fn suspend_current_and_run_next() {
    // There must be an application running.
    let task = current_task().unwrap();

    // ---- hold current PCB lock
    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.update_process_times_schedule_out();
    task.sched_vruntime_hint.store(
        task_inner.sched_vruntime,
        core::sync::atomic::Ordering::Relaxed,
    );
    drop(task_inner);
    // ---- release current PCB lock

    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

/// 将当前任务置为 `Blocked` 并切回调度器。
///
/// # Semantics
///
/// 当前任务进入 interruptible 队列，不再被 ready 队列选中，直到被唤醒或
/// 超时路径重新置为 ready。
pub(crate) fn block_current_and_run_next() {
    // There must be an application running.
    let task = current_task().unwrap();

    // ---- hold current PCB lock
    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.update_process_times_schedule_out();
    task.sched_vruntime_hint.store(
        task_inner.sched_vruntime,
        core::sync::atomic::Ordering::Relaxed,
    );
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
///
/// # Locking
///
/// `should_block` 在任务已进入 interruptible 队列后执行。闭包内部不得持有
/// 调度器队列锁再获取当前任务 inner 锁。
pub(crate) fn block_current_and_run_next_checked(
    should_block: impl FnOnce(&Arc<TaskControlBlock>) -> bool,
) {
    let task = current_task().unwrap();

    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.update_process_times_schedule_out();
    task.sched_vruntime_hint.store(
        task_inner.sched_vruntime,
        core::sync::atomic::Ordering::Relaxed,
    );
    drop(task_inner);

    sleep_interruptible(task.clone());
    if !should_block(&task) {
        let _ = wake_interruptible(task.clone());
    }
    schedule(task_cx_ptr);
}

/// 带释放锁的阻塞调度。
///
/// # Locking
///
/// 当前任务先进入 interruptible 队列，再释放调用方传入的锁，最后切回调度器。
/// 这保证唤醒方不会在“释放锁”和“睡眠入队”之间丢失唤醒。
pub(crate) fn block_current_and_run_next_with_lock<T>(lock: MutexGuard<'_, T>) {
    // There must be an application running.
    let task = current_task().unwrap();

    // ---- hold current PCB lock
    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;

    task_inner.update_process_times_schedule_out();
    task.sched_vruntime_hint.store(
        task_inner.sched_vruntime,
        core::sync::atomic::Ordering::Relaxed,
    );

    drop(task_inner);
    // ---- release current PCB lock

    // push to interruptible queue of scheduler, so that it won't be scheduled.
    sleep_interruptible(task);
    drop(lock);
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

/// 带释放锁和阻塞条件复查的调度入口。
///
/// # Semantics
///
/// WaitQueue 使用该入口保证“入队 -> 条件复查 -> 睡眠”的顺序。如果复查时
/// 条件已经满足，会把任务从 interruptible 状态恢复为 ready。
pub(crate) fn block_current_and_run_next_with_lock_checked<T>(
    lock: MutexGuard<'_, T>,
    should_block: impl FnOnce(&Arc<TaskControlBlock>) -> bool,
) {
    let task = current_task().unwrap();

    let mut task_inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.update_process_times_schedule_out();
    task.sched_vruntime_hint.store(
        task_inner.sched_vruntime,
        core::sync::atomic::Ordering::Relaxed,
    );
    drop(task_inner);

    sleep_interruptible(task.clone());
    if !should_block(&task) {
        let _ = wake_interruptible(task.clone());
    }
    drop(lock);
    schedule(task_cx_ptr);
}

fn do_exit(task: &TaskControlBlock, exit_code: u32) {
    if task.exit_thread_resources(exit_code) {
        if task.process.live_thread_count() == 0 {
            crate::syscall::fs::release_fcntl_locks_for_pid(task.pid());
            crate::syscall::shm_detach_process(task.pid());
            task.process.finish_exit(task, exit_code);
        }
    }
}

/// 退出当前线程并切回调度器。
///
/// # Semantics
///
/// 当前任务会进入 zombie 队列；函数不返回。因为代码仍运行在当前任务的内核栈
/// 上，最后一个 `Arc<TaskControlBlock>` 必须延迟到切回 idle 后释放。
pub fn exit_current_and_run_next(exit_code: u32) -> ! {
    let task = current_task().unwrap();
    do_exit(&task, exit_code);
    // current 槽仍保留强引用直到 idle 完成 switch-out。这个本地 Arc
    // 必须在切栈前释放，否则退出任务的栈帧永不返回，会泄漏引用计数。
    drop(task);
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
    panic!("Unreachable");
}

/// 请求整个线程组退出，并退出当前线程。
///
/// # Semantics
///
/// 其他线程先从调度队列移除并释放线程级资源，当前线程最后进入 zombie 队列。
/// 函数不返回，且不能让任何本地 `Arc` 跨过最终的 `schedule()`。
pub fn exit_group_and_run_next(exit_code: u32) -> ! {
    let task = current_task().unwrap();

    // 把 process Arc 的生命周期限制在 schedule() 之前。
    // 此函数返回 !，schedule() 切栈后本地变量永不析构——不能有任何 Arc 跨过它。
    let exit_list: Vec<_> = {
        let process = task.process.clone();
        process.request_group_exit(exit_code);
        process
            .threads()
            .into_iter()
            .filter(|thread| thread.tid.0 != task.tid.0)
            .collect()
    }; // process Arcs 在此 drop

    manager::remove_tasks_from_queues(&exit_list);

    for task in exit_list.into_iter() {
        task.exit_thread_resources(exit_code);
    }
    do_exit(&task, exit_code);
    drop(task);
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
    panic!("Unreachable");
}

lazy_static! {
    /// 启动阶段创建的 init 进程。
    ///
    /// 优先加载 `/init`，缺失时兼容传统镜像里的 `/initproc`。
    pub static ref INITPROC: Arc<TaskControlBlock> = {
        // 优先使用 /init（initramfs 模式），fallback 到 /initproc（传统模式）
        let (_init_path, inode) = match vfs_lookup_absolute("/init") {
            Ok(inode) => ("/init", inode),
            Err(_) => (
                "/initproc",
                vfs_lookup_absolute("/initproc").expect("[kernel] no /init or /initproc found"),
            ),
        };
        #[cfg(feature = "board_2k1000")]
        boot_trace!("[bringup][init:01] selected userspace entry {}", _init_path);
        let elf = fs::vfs::File::new(inode, fs::vfs::FileFlags::O_RDONLY).unwrap();
        #[cfg(feature = "board_2k1000")]
        boot_trace!("[bringup][init:02] entry file opened; building initial task");
        let task = TaskControlBlock::new(elf);
        #[cfg(feature = "board_2k1000")]
        boot_trace!(
            "[bringup][init:03] initial task built: pid={} tid={}",
            task.pid(),
            task.gettid()
        );
        task
    };

    /// Ktest-only orphan reaper.
    ///
    /// Ktest enters the scheduler without constructing `INITPROC`; this PCB
    /// owns no TCB and exists only to keep ktest child/zombie ownership from
    /// falling back to the normal-boot reaper.
    static ref KTEST_REAPER: Arc<ProcessControlBlock> = {
        let tid_handle = tid_alloc();
        let reaper = new_ktest_process(tid_handle, None);
        reaper.set_child_subreaper(true);
        reaper
    };
}

/// 将 init 进程加入 ready 队列。
pub fn add_initproc() {
    #[cfg(feature = "board_2k1000")]
    boot_trace!("[bringup][init:04] enqueue initial task");
    publish_task(INITPROC.clone());
    #[cfg(feature = "board_2k1000")]
    boot_trace!("[bringup][init:05] initial task is on ready queue");
}

// ── ktest multi-task harness ────────────────────────────────────────

/// Build a kernel-only PCB for a ktest task without loading `/init`.
///
/// The root VFS and devfs are initialized before ktest enters this path.  The
/// PCB therefore has a valid root cwd but an empty descriptor table and bare
/// address space; ktest tasks never enter userspace and do not need tty fds.
fn new_ktest_process(
    tid_handle: Arc<TidHandle>,
    parent: Option<Weak<ProcessControlBlock>>,
) -> Arc<ProcessControlBlock> {
    let root_inode = fs::vfs_root().mountpoint_root_inode();
    let root_file = fs::vfs::File::new(
        root_inode,
        fs::vfs::FileFlags::O_RDONLY | fs::vfs::FileFlags::O_DIRECTORY,
    )
    .expect("ktest root VFS must be initialized before task creation");
    let pid = tid_handle.0;

    Arc::new(ProcessControlBlock::new(
        pid,
        tid_handle.0,
        tid_handle,
        quota::TaskQuotaGuard::acquire_for_init(),
        pid,
        pid,
        parent,
        Arc::new(spin::Mutex::new(root_file.clone())),
        String::from("[ktest]"),
        Arc::new(spin::Mutex::new(fs::vfs::FdTable::new())),
        Arc::new(spin::Mutex::new(FsStatus {
            working_inode: root_file,
            working_path: String::from("/"),
            root_inode: None,
            umask: 0,
        })),
        Arc::new(spin::Mutex::new(UtsNamespace::new())),
        INIT_NET_NAMESPACE.clone(),
        INIT_MOUNT_NAMESPACE.clone(),
        INIT_IPC_NAMESPACE.clone(),
        Arc::new(spin::Mutex::new(AddressSpace::<PageTableImpl>::new_bare())),
        Arc::new(spin::Mutex::new(Sighand::new())),
        Arc::new(spin::Mutex::new(threads::Futex::new())),
        Arc::new(spin::Mutex::new(pid::RecycleAllocator::new())),
    ))
}

/// Trampoline for ktest kernel tasks.
///
/// Called via `TaskContext.ra` when the scheduler first switches to a
/// ktest-spawned task. It invokes the stored function, then exits the
/// task without process-level cleanup.
extern "C" fn ktest_trampoline() -> ! {
    // 入口属于当前 TCB，不再通过全局“下一任务函数”传递；多个 CPU 首次
    // 切入不同 kernel-only 任务时不会互相覆盖 trampoline 参数。
    let f = current_task()
        .and_then(|task| task.kernel_entry())
        .expect("kernel-only task has no entry function");
    f();
    zombify_current_and_run_next();
}

/// Spawn a minimal kernel task for ktest mode only.
///
/// The task has a bare kernel stack, kernel-only PCB, no user memory, and no
/// file descriptors.  It never touches `INITPROC` or parses an init ELF.
/// It runs `f()` and then calls [`zombify_current_and_run_next`].
///
/// 调用前必须完成 VFS 与任务 registry 的全局初始化。
pub fn spawn_ktest_task(f: fn()) -> Arc<TaskControlBlock> {
    spawn_ktest_task_on(crate::smp::BOOT_CPU_ID, f)
}

/// 在指定 CPU 创建一个 kernel-only ktest 任务。
///
/// 该入口只用于验证 AP 调度闭环，不向普通任务开放 CPU 选择。普通用户任务、
/// blocked wake 和迁移仍固定 CPU0，直到远端 TLB shootdown 与共享子系统审计完成。
/// `f` 只能访问原子量、CPU-local/task 调度原语和已明确加锁的 registry；不得
/// 在 AP 上进入 console、网络、文件系统、设备或用户 MM 路径。
pub(crate) fn spawn_ktest_task_on(cpu: usize, f: fn()) -> Arc<TaskControlBlock> {
    assert!(cpu < crate::smp::configured_cpu_count());
    if cpu != crate::smp::BOOT_CPU_ID {
        assert!(
            crate::smp::schedulers_released(),
            "cannot publish AP task before scheduler-ready"
        );
        assert_ne!(
            crate::smp::online_cpu_mask() & (1usize << cpu),
            0,
            "cannot publish task to offline CPU {}",
            cpu
        );
    }
    let tid_handle = tid_alloc();
    let kstack = crate::hal::kstack_alloc();
    let kstack_top = kstack.get_top();
    if cpu != crate::smp::cpu_id() {
        // kstack_alloc 只刷新创建者的本地 TLB。目标 CPU 必须在任务可见前
        // 确认新映射，避免 __switch 刚换到高地址栈就命中旧的无效翻译。
        crate::smp::synchronize_kernel_mapping(cpu).unwrap_or_else(|error| {
            panic!("failed to publish kernel stack to CPU {}: {:?}", cpu, error)
        });
    }
    let task_cx = TaskContext::goto_address(ktest_trampoline as usize, kstack_top);
    let pcb = new_ktest_process(tid_handle.clone(), Some(Arc::downgrade(&KTEST_REAPER)));
    let tcb = TaskControlBlock::new_ktest_independent(tid_handle, pcb, kstack, task_cx, f);
    tcb.process.add_thread(&tcb);
    registry::register_process(&tcb.process);
    registry::register_task(&tcb);
    let handle = tcb.clone();
    run_queue::publish(tcb, cpu);
    if cpu != crate::smp::cpu_id() {
        // publish 已经释放目标 runqueue 锁；doorbell 绝不能发生在队列锁内。
        crate::smp::request_reschedule(cpu).unwrap_or_else(|error| {
            panic!("failed to wake CPU {} after remote enqueue: {}", cpu, error)
        });
    }
    handle
}

/// Minimal exit for ktest tasks: mark as zombie and schedule away.
///
/// Unlike [`exit_current_and_run_next`], this does NOT call process-level
/// cleanup. It only marks the task as [`TaskStatus::Zombie`] and switches
/// back to idle; idle then transfers the retained current Arc to the zombie queue.
/// Ktest 的 live-thread 计数会在 zombie queue 最终 drop TCB 时由 `Drop` 收回。
/// 纯内核 ktest 不向用户态暴露 rusage，因此这里不重复生产退出路径的 CPU 时间结算。
pub fn zombify_current_and_run_next() -> ! {
    let task = current_task().unwrap();
    task.mark_zombie("ktest task exit");
    drop(task);
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
    panic!("Unreachable after zombify_current_and_run_next");
}
