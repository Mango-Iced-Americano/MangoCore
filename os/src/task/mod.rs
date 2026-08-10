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

/// 线程 CPU 时间冲刷到 PCB 的最大本地批量。
///
/// 进程级查询据此声明 1ms 精度；schedule-out 和 exit 不受该阈值限制，会强制
/// 冲刷尚未达到批量的尾数。
pub(crate) const PROCESS_CPU_ACCOUNT_BATCH_US: usize = 1_000;

use crate::fs::{self, vfs_lookup_absolute};
use crate::hal::__switch;
use crate::mm::{AddressSpace, AddressSpaceInner, PageTableImpl};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
pub use completion::Completion;
pub use context::TaskContext;
pub use elf::{load_elf_interp, AuxvEntry, AuxvType, ELFInfo};
use lazy_static::*;
pub use manager::{
    add_kernel_timer, all_pids, do_oom, do_wake_expired, has_ready_task, kernel_timer_queue_len,
    procs_count, publish_task, remove_zombie_tasks_by_pid, run_deferred_timer_work,
    run_task_safe_point, send_signal_to_interruptible, sleep_interruptible, task_manager_counts,
    timer_cpu_init, timer_interrupt_handler, update_ready_nice, wait_with_timeout,
    wake_interruptible, zombie_count, TimerAction, WaitQueue, WaitResult,
};
use manager::{fetch_task, finish_switch_out};
pub(crate) use manager::{
    publish_task_on, request_sibling_exit, set_remote_affinity, try_publish_task,
    try_publish_task_on,
};
pub(crate) use processor::zombie_queue_count_fast;
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
    unregister_writable_inode, PosixTimer, ProcessControlBlock, ProcessState,
};
pub(crate) use process::{ExecutableMappingGuard, IntervalTimerKind, LimitPair, ProcessLimits};
pub(crate) use process_manager::CloneScheduleOutcome;
pub use process_manager::ProcessManager;
pub use processor::{
    current_egid, current_euid, current_gid, current_parent_pid, current_pgid, current_pid,
    current_sgid, current_sid, current_suid, current_syscall_name, current_task, current_tid,
    current_uid, current_user_token, has_zombie_queue_tasks_fast, run_tasks, schedule,
    set_current_syscall_id, take_zombie_tasks, try_current_user_token,
};
pub(crate) use processor::{current_trap_task, try_current_task};
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
    any_seccomp_enabled, FsStatus, RobustList, RseqRegistration, Rusage, SeccompFilterInsn,
    TaskControlBlock, TaskStatus, UtsNamespace,
};
pub(crate) use task::BlockedReason;

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

/// 结算当前任务的内核态时间，并返回切换上下文。
///
/// 线程组 CPU 增量在释放 `task.inner` 后才冲刷到 PCB；当前实现只触碰原子
/// 计数器，但保持这个顺序可防止未来慢路径意外形成 `task -> process` 锁边。
fn prepare_current_switch(task: &Arc<TaskControlBlock>) -> *mut TaskContext {
    let mut inner = task.acquire_inner_lock();
    let task_cx_ptr = &mut inner.task_cx as *mut TaskContext;
    let (user_us, system_us) = inner.update_process_times_schedule_out();
    task.sched_vruntime_hint
        .store(inner.sched_vruntime, core::sync::atomic::Ordering::Relaxed);
    drop(inner);
    task.process.account_cpu_time(user_us, system_us);
    task_cx_ptr
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

    let task_cx_ptr = prepare_current_switch(&task);

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

    let task_cx_ptr = prepare_current_switch(&task);

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

    let task_cx_ptr = prepare_current_switch(&task);

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

    let task_cx_ptr = prepare_current_switch(&task);

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

    let task_cx_ptr = prepare_current_switch(&task);

    sleep_interruptible(task.clone());
    if !should_block(&task) {
        let _ = wake_interruptible(task.clone());
    }
    drop(lock);
    schedule(task_cx_ptr);
}

/// 在可响应本地 IPI 的窗口内完成当前线程清理并切回 idle。
///
/// fatal signal 从 trap-return 的 IRQ-off 边界进入，普通 syscall exit 则从
/// IRQ-on 窗口进入。这里先统一关闭，再用受控窗口开放中断；这样 user-memory
/// 清理和 TLB shootdown 等待都不会阻塞本 CPU 对其它 shootdown 的 ack。
fn finish_current_exit(task: Arc<TaskControlBlock>, exit_code: u32) -> ! {
    let _ = crate::hal::local_irq_save();
    crate::hal::with_local_interrupts_enabled(|| {
        // 第一次读取让本线程的 clear_child_tid 等清理尽早使用统一退出码。
        // 它不是最终线性化点：另一个 live sibling 仍可能随后发布 group exit。
        let thread_exit_code = task.process.group_exit_code().unwrap_or(exit_code);
        if task.exit_thread_resources(thread_exit_code) {
            // live token 已经归零后，不再有其它线程能新发起 group exit；此处
            // Acquire 复读才决定 wait 可见的进程退出码。remove_thread() 的
            // AcqRel 退出链保证我们也能观察 sibling 先前发布的统一退出码。
            let process_exit_code = task.process.group_exit_code().unwrap_or(thread_exit_code);
            crate::syscall::fs::release_fcntl_locks_for_pid(task.pid());
            crate::syscall::shm_detach_process(task.pid());
            // auto-reap 在 current 切回 idle 前运行：这里只会提前摘取已经
            // 位于 local_zombies 的 sibling；current TCB 随后由 owner CPU
            // 的 finish_switch_out() 入队并在 idle 栈上回收。
            task.process.finish_exit(&task, process_exit_code);
        }
        // noreturn schedule 不会析构当前 Rust 栈；本地 clone 必须提前释放。
        drop(task);
        let mut _unused = TaskContext::zero_init();
        schedule(&mut _unused as *mut _);
        panic!("Unreachable");
    })
}

/// 退出当前线程并切回调度器。
///
/// # Semantics
///
/// 当前任务会进入 zombie 队列；函数不返回。因为代码仍运行在当前任务的内核栈
/// 上，最后一个 `Arc<TaskControlBlock>` 必须延迟到切回 idle 后释放。
pub fn exit_current_and_run_next(exit_code: u32) -> ! {
    let task = current_task().unwrap();
    finish_current_exit(task, exit_code)
}

/// 请求整个线程组退出，并退出当前线程。
///
/// # Semantics
///
/// 第一个调用者原子关闭 clone 发布门禁并固定退出码；每个 sibling 收到
/// SIGKILL/RESCHEDULE 后只在自己的安全点释放资源。最后一个 live token 的
/// 持有者完成进程级清理，任何 CPU 都不会远程释放仍在使用的内核栈或用户资源。
pub fn exit_group_and_run_next(exit_code: u32) -> ! {
    let task = current_task().unwrap();
    let (exit_code, threads) = task.process.begin_group_exit(exit_code);
    request_sibling_exit(&threads, task.gettid());
    // noreturn schedule 不会析构当前 Rust 栈；所有 sibling Arc 必须在这里释放。
    drop(threads);
    finish_current_exit(task, exit_code)
}

lazy_static! {
    /// 启动阶段创建的 init 进程。
    ///
    /// 优先加载 `/init`，缺失时兼容传统镜像里的 `/initproc`。
    pub static ref INITPROC: Arc<TaskControlBlock> = {
        let init_path = crate::hal::platform::default_init_path();
        // 优先使用 /init（initramfs 模式），fallback 到 boot-profile 默认路径。
        let (_init_path, inode) = match vfs_lookup_absolute("/init") {
            Ok(inode) => ("/init", inode),
            Err(_) => (
                init_path,
                vfs_lookup_absolute(init_path)
                    .unwrap_or_else(|_| panic!("[kernel] no /init or {} found", init_path)),
            ),
        };
        #[cfg(feature = "boot_la_uboot_dmw")]
        boot_trace!("[bringup][init:01] selected userspace entry {}", _init_path);
        let elf = fs::vfs::File::new(inode, fs::vfs::FileFlags::O_RDONLY).unwrap();
        #[cfg(feature = "boot_la_uboot_dmw")]
        boot_trace!("[bringup][init:02] entry file opened; building initial task");
        let task = TaskControlBlock::new(elf);
        #[cfg(feature = "boot_la_uboot_dmw")]
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
        let reaper = new_kernel_process(tid_handle, None, "[ktest]");
        reaper.set_child_subreaper(true);
        reaper
    };
}

/// 将 init 进程加入 ready 队列。
pub fn add_initproc() {
    #[cfg(feature = "boot_la_uboot_dmw")]
    boot_trace!("[bringup][init:04] enqueue initial task");
    publish_task(INITPROC.clone());
    #[cfg(feature = "boot_la_uboot_dmw")]
    boot_trace!("[bringup][init:05] initial task is on ready queue");
}

// ── ktest multi-task harness ────────────────────────────────────────

/// 构造不加载 `/init` 的 kernel-only 进程容器。
///
/// ktest 与常驻内核 worker 共用这条构造路径：保留有效根目录，但不创建用户
/// 地址空间、TTY fd 或 ELF 上下文。`comm` 只用于诊断，不参与任务身份判定。
fn new_kernel_process(
    tid_handle: Arc<TidHandle>,
    parent: Option<Weak<ProcessControlBlock>>,
    comm: &str,
) -> Arc<ProcessControlBlock> {
    let root_inode = fs::vfs_root().mountpoint_root_inode();
    let root_file = fs::vfs::File::new(
        root_inode,
        fs::vfs::FileFlags::O_RDONLY | fs::vfs::FileFlags::O_DIRECTORY,
    )
    .expect("kernel task root VFS must be initialized before task creation");
    let pid = tid_handle.0;

    Arc::new(ProcessControlBlock::new(
        pid,
        tid_handle,
        quota::TaskQuotaGuard::acquire_for_init(),
        pid,
        pid,
        parent,
        Arc::new(spin::Mutex::new(root_file.clone())),
        String::from(comm),
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
        AddressSpace::new(AddressSpaceInner::<PageTableImpl>::new_bare()),
        Arc::new(spin::Mutex::new(Sighand::new())),
        Arc::new(spin::Mutex::new(threads::FutexTable::new())),
        Arc::new(spin::Mutex::new(pid::RecycleAllocator::new())),
        ProcessLimits::default(),
    ))
}

/// 所有 kernel-only 任务的统一入口。
///
/// 调度器第一次切入任务时从 TCB 取得独占的入口函数；入口返回后只回收当前
/// kernel-only 线程，不执行用户进程级清理。
extern "C" fn kernel_task_trampoline() -> ! {
    // 入口属于当前 TCB，不再通过全局“下一任务函数”传递；多个 CPU 首次
    // 切入不同 kernel-only 任务时不会互相覆盖 trampoline 参数。
    let f = current_task()
        .and_then(|task| task.kernel_entry())
        .expect("kernel-only task has no entry function");
    f();
    zombify_current_and_run_next();
}

/// 在 ktest 模式创建最小内核任务。
///
/// 任务具有独立内核栈和 kernel-only PCB，不解析用户 ELF，也不创建用户地址空间
/// 或文件描述符。入口函数返回后由统一 trampoline 转为 Zombie。
/// 调用前必须完成 VFS 与任务 registry 的全局初始化。
pub fn spawn_ktest_task(f: fn()) -> Arc<TaskControlBlock> {
    spawn_ktest_task_on(crate::smp::BOOT_CPU_ID, f)
}

/// 构造尚未发布的 kernel-only 任务。
///
/// affinity 必须在 `New` 状态设置，随后统一通过 `publish_task()` 完成唯一一次
/// `New -> Queued(cpu)`；这样测试任务和常驻 worker 都不会绕过调度状态机。
fn build_kernel_task(
    cpu: usize,
    comm: &str,
    parent: Option<Weak<ProcessControlBlock>>,
    f: fn(),
) -> Arc<TaskControlBlock> {
    assert!(cpu < crate::smp::configured_cpu_count());
    let tid_handle = tid_alloc();
    let kstack = crate::hal::kstack_alloc();
    let task_cx = TaskContext::goto_address(kernel_task_trampoline as usize, kstack.get_top());
    let pcb = new_kernel_process(tid_handle.clone(), parent, comm);
    let tcb = TaskControlBlock::new_kernel_only(tid_handle, pcb, kstack, task_cx, f);
    tcb.set_initial_cpus_allowed(1usize << cpu);
    registry::register_process(&tcb.process);
    tcb
}

/// 在指定 CPU 创建一个 kernel-only ktest 任务。
///
/// 该入口只用于验证 AP 调度闭环，不向普通任务开放 CPU 选择。用户探针通过
/// `publish_task_on()` 单独发布，B29 还只在显式 yield 安全点做一次受控迁移；
/// 本入口会把任务的初始 `cpus_allowed` 收紧到指定 CPU，防止后续 wake 或
/// owner 交接绕过测试声明的 placement。
/// 普通用户任务的初始 mask 仍默认为 CPU0。`f` 只能访问原子量、CPU-local/task
/// 调度原语和已明确加锁的 registry；不得在 AP 上进入 console、网络、文件系统、
/// 设备或用户 MM 路径。
pub(crate) fn spawn_ktest_task_on(cpu: usize, f: fn()) -> Arc<TaskControlBlock> {
    let tcb = build_kernel_task(cpu, "[ktest]", Some(Arc::downgrade(&KTEST_REAPER)), f);
    let handle = tcb.clone();
    // 单 bit mask 保证仍精确到达指定 CPU，同时让所有 AP focused 用例
    // 覆盖普通任务使用的 affinity-aware 初始放置入口。
    publish_task(tcb);
    handle
}

/// 在 CPU0 创建一个运行至 shutdown 的内核 worker。
///
/// worker 使用独立 PCB，避免被 ktest reaper 当成临时测试子任务；它第一次运行
/// 后应进入自己的 WaitQueue，空闲时不占用调度时间片。
pub fn spawn_kernel_worker(comm: &str, f: fn()) -> Arc<TaskControlBlock> {
    let tcb = build_kernel_task(crate::smp::BOOT_CPU_ID, comm, None, f);
    let handle = tcb.clone();
    publish_task(tcb);
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
