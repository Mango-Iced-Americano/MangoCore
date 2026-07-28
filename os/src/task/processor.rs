//! Per-CPU 处理器状态和调度主循环。
//!
//! 每个 CPU 通过 `PerCpu.task_state` 持有自己的 current 槽和 idle 上下文。
//! 当前用户任务仍只由 CPU0 调度，但 current 所有权已不再依赖全局单例。
//!
//! # Locking
//!
//! 本 CPU 的 `processor` 锁只保护当前任务槽和 idle 上下文。切换前必须释放该锁，
//! 否则切回调度器时会形成自锁。

use super::{
    __switch, do_wake_expired, has_zombie_queue_tasks_fast, take_one_interruptible_zombie,
    take_one_ready_zombie, take_zombie_tasks,
};
use super::{fetch_task, finish_switch_out};
use super::{TaskContext, TaskControlBlock};
use crate::hal::TrapContext;
use crate::net::config::NET_INTERFACE;
use alloc::sync::Arc;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

const BACKGROUND_NET_POLL_INTERVAL: usize = 64;
const IDLE_NET_POLL_INTERVAL: usize = 64;
const RV64_CONSOLE_POLL_INTERVAL: usize = 64;

#[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
static BOARD_FIRST_TASK_SWITCH: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 单个 CPU 独占的调度上下文。
struct Processor {
    /// 当前正在运行的任务
    current: Option<Arc<TaskControlBlock>>,
    /// 空闲任务的上下文，用于在任务切换时保存和恢复状态
    idle_task_cx: TaskContext,
}

impl Processor {
    const fn new() -> Self {
        Self {
            // 初始化时处理器为空闲
            current: None,
            // 空闲任务的上下文
            idle_task_cx: TaskContext::zero_init(),
        }
    }
    /// 返回 idle 任务上下文指针，供 `__switch` 保存/恢复寄存器。
    fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        &mut self.idle_task_cx as *mut _
    }

    /// 在 context switch 已完成后取出上一任务。
    ///
    /// # Semantics
    ///
    /// 只能由 idle 调度循环调用；任务仍在自身内核栈上时提前清空会破坏
    /// current slot 的所有权语义。
    fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        // 将current字段置空，并返回其中的值
        self.current.take()
    }
    /// 克隆当前正在运行的任务。
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)
    }

}

/// 嵌入 `PerCpu` 的任务调度状态。
///
/// PID/TID 在 current 槽有效期内不变，因此保留无锁快照。身份、进程组和
/// 页表 token 可在运行期修改，读取时必须访问 TCB/PCB 的权威原子 hint。
pub(crate) struct CpuTaskState {
    processor: Mutex<Processor>,
    current_pid: AtomicUsize,
    current_tid: AtomicUsize,
    /// 0 表示无记录，实际 syscall id 存为 id + 1。
    current_syscall_id: AtomicUsize,
}

impl CpuTaskState {
    pub(crate) const fn new() -> Self {
        Self {
            processor: Mutex::new(Processor::new()),
            current_pid: AtomicUsize::new(0),
            current_tid: AtomicUsize::new(0),
            current_syscall_id: AtomicUsize::new(0),
        }
    }
}

/// 运行调度主循环。
///
/// # Semantics
///
/// 循环执行定时唤醒、网络轮询、文件缓存回收、zombie 清理和 ready 队列取任务。
/// 找到任务后发布本 CPU current 槽、释放 processor 锁并切换到任务上下文。
///
/// # Locking
///
/// 调用 `__switch` 前必须释放本 CPU processor 锁；被切入任务后该函数直到任务主动
/// `schedule()` 回 idle 才会继续执行。
pub fn run_tasks() {
    let mut schedule_tick = 0usize;
    let cpu = crate::smp::cpu_id();
    let task_state = crate::smp::local_task_state();
    loop {
        // `schedule()` 保证 idle 总是以 IRQ-off 状态恢复。Phase 2 仍保持这个
        // legacy scheduler 边界；console/net/FS reclaim 完成 IRQ 并发审计前，
        // 不能把整个 housekeeping 循环扩大为开中断区间。
        // schedule() 可能在内核 timer 打断长 syscall 后直接切回 idle。
        // 在获取 processor 或队列锁之前消费 pending，保证 callback 不跨锁，
        // 且已经处于 idle 调度上下文时无需再次 context switch。
        let _ = super::run_deferred_timer_work();
        let sched_profile = sched_profile_enabled();
        if sched_profile {
            SCHED_LOOPS.fetch_add(1, SchedOrdering::Relaxed);
        }
        let loop_t0 = sched_profile_start(sched_profile);
        schedule_tick = schedule_tick.wrapping_add(1);
        // Read one character from UART per iteration. Handle in priority order:
        // 1. Magic key (Ctrl+T) → trace dump + shutdown
        // 2. Other input → stash, then feed the TTY line discipline.  The
        //    production path owns both task and epoll readiness notification.
        //
        // On rv64 this is an SBI ecall, so do not pay it on every context
        // switch. Blocked readers are covered by the scheduler's periodic
        // console poll and the existing wait-IO fallback timer.
        #[cfg(target_arch = "riscv64")]
        let should_poll_console = schedule_tick % RV64_CONSOLE_POLL_INTERVAL == 0;
        #[cfg(not(target_arch = "riscv64"))]
        let should_poll_console = true;
        if should_poll_console {
            let stage_t0 = sched_profile_start(sched_profile);
            let ch = crate::hal::console_getchar() as u8;
            if ch != 0xFF {
                if crate::trace::check_magic_key(ch, "schedule") {
                    // check_magic_key → dump_from → shutdown, never returns.
                } else {
                    crate::trace::stash_char(ch);
                    crate::fs::dev::tty::Teletype::receive_stashed();
                }
            }
            sched_record_stage(
                sched_profile,
                &SCHED_STAGE_CONSOLE_CALLS,
                &SCHED_STAGE_CONSOLE_CYCLES_TOTAL,
                &SCHED_STAGE_CONSOLE_CYCLES_MAX,
                stage_t0,
            );
        }
        // Keep the legacy timeout sweep until every wait path has been proven
        // to be driven solely by timer interrupts. Removing it can strand early
        // boot networking waits before init reaches the test runner.
        let stage_t0 = sched_profile_start(sched_profile);
        do_wake_expired();
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_WAKE_EXPIRED_CALLS,
            &SCHED_STAGE_WAKE_EXPIRED_CYCLES_TOTAL,
            &SCHED_STAGE_WAKE_EXPIRED_CYCLES_MAX,
            stage_t0,
        );
        if schedule_tick % BACKGROUND_NET_POLL_INTERVAL == 0 {
            let stage_t0 = sched_profile_start(sched_profile);
            NET_INTERFACE.try_poll();
            sched_record_stage(
                sched_profile,
                &SCHED_STAGE_NET_POLL_CALLS,
                &SCHED_STAGE_NET_POLL_CYCLES_TOTAL,
                &SCHED_STAGE_NET_POLL_CYCLES_MAX,
                stage_t0,
            );
        }
        let rec_t0 = sched_profile_start(sched_profile);
        crate::fs::reclaim::maybe_reclaim_fs_caches();
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_RECLAIM_CALLS,
            &SCHED_STAGE_RECLAIM_CYCLES_TOTAL,
            &SCHED_STAGE_RECLAIM_CYCLES_MAX,
            rec_t0,
        );
        if sched_profile {
            let rec_dt = sched_rdcycle().saturating_sub(rec_t0);
            SCHED_RECLAIM_CALL_CYCLES_TOTAL.fetch_add(rec_dt, SchedOrdering::Relaxed);
            sched_atomic_max(&SCHED_RECLAIM_CALL_CYCLES_MAX, rec_dt);
        }
        // Drain one Dying MountFS backend lifecycle per tick when available.
        // Backend teardown (on_umount) runs outside any lock.
        if schedule_tick % 128 == 0 {
            crate::fs::vfs::drain_one_dying_lifecycle();
        }
        // 当前任务退出后先进入专用 zombie 队列；切回 idle 后即可安全 drop。
        // 这样避免把不可运行的 TCB 塞进 ready_queue 再扫描剔除。
        let stage_t0 = sched_profile_start(sched_profile);
        if has_zombie_queue_tasks_fast() {
            let zombies = take_zombie_tasks(64);
            let drained_zombies = zombies.len();
            drop(zombies);
            super::perf::record_zombie_drain(drained_zombies);
            crate::task::perf::record_zombie_drain_full(0, 1, drained_zombies);
        }
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_ZOMBIE_QUEUE_CALLS,
            &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_TOTAL,
            &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_MAX,
            stage_t0,
        );
        // 兜底清理旧队列中的 zombie，避免异常路径留下不可运行任务。
        // Also do full queue scan for stats zombie/nice counters every 64 ticks.
        if schedule_tick % 64 == 0 {
            let stage_t0 = sched_profile_start(sched_profile);
            for _ in 0..8 {
                let a = take_one_ready_zombie();
                let b = take_one_interruptible_zombie();
                if a.is_none() && b.is_none() {
                    break;
                }
                drop(a);
                drop(b);
            }
            let (ready_z, int_z, nnice) = {
                let manager = crate::task::manager::TASK_MANAGER.lock();
                let mut ready_zombie = 0usize;
                let mut int_zombie = 0usize;
                let mut nonzero_nice = 0usize;
                for t in &manager.ready_queue {
                    if t.is_zombie() {
                        ready_zombie += 1;
                    }
                    if t.sched_nice_hint.load(Ordering::Relaxed) != 0 {
                        nonzero_nice += 1;
                    }
                }
                for t in &manager.interruptible_queue {
                    if t.is_zombie() {
                        int_zombie += 1;
                    }
                }
                (ready_zombie, int_zombie, nonzero_nice)
            };
            crate::task::perf::record_taskq_queue_lens(
                crate::task::manager::ready_count_fast() as usize,
                crate::task::manager::interruptible_count_fast() as usize,
                ready_z,
                int_z,
                nnice,
            );
            sched_record_stage(
                sched_profile,
                &SCHED_STAGE_STALE_ZOMBIE_CALLS,
                &SCHED_STAGE_STALE_ZOMBIE_CYCLES_TOTAL,
                &SCHED_STAGE_STALE_ZOMBIE_CYCLES_MAX,
                stage_t0,
            );
        } else {
            crate::task::perf::record_taskq_queue_lens(
                crate::task::manager::ready_count_fast() as usize,
                crate::task::manager::interruptible_count_fast() as usize,
                0,
                0,
                0,
            );
        }
        // 降频清理 PROCESS_SHARED_FUTEX 空 WaitQueue 键
        let stage_t0 = sched_profile_start(sched_profile);
        super::threads::compact_shared_futex();
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_FUTEX_COMPACT_CALLS,
            &SCHED_STAGE_FUTEX_COMPACT_CYCLES_TOTAL,
            &SCHED_STAGE_FUTEX_COMPACT_CYCLES_MAX,
            stage_t0,
        );
        let stage_t0 = sched_profile_start(sched_profile);
        // 取任务时不持有 processor，避免形成 processor -> TASK_MANAGER 的
        // 嵌套锁顺序；fetch 成功后再单独发布 current slot。
        let next_task = fetch_task(cpu);
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_FETCH_TASK_CALLS,
            &SCHED_STAGE_FETCH_TASK_CYCLES_TOTAL,
            &SCHED_STAGE_FETCH_TASK_CYCLES_MAX,
            stage_t0,
        );
        let stage_t0 = sched_profile_start(sched_profile);
        if let Some((ready_len, interruptible_len)) = super::task_manager_counts() {
            sched_record_queue_sample(ready_len as u64, interruptible_len as u64);
        }
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_QUEUE_SAMPLE_CALLS,
            &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_TOTAL,
            &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_MAX,
            stage_t0,
        );
        if sched_profile {
            if next_task.is_some() {
                SCHED_FETCH.fetch_add(1, SchedOrdering::Relaxed);
            } else {
                SCHED_IDLE.fetch_add(1, SchedOrdering::Relaxed);
            }
        }
        super::perf::record_schedule_loop(next_task.is_some());
        if let Some(task) = next_task {
            #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
            let trace_first_switch = !BOARD_FIRST_TASK_SWITCH.swap(true, Ordering::Relaxed);
            let stage_t0 = sched_profile_start(sched_profile);
            // 先在不持有 processor 锁时更新任务时间并取得上下文指针。
            // 这样不会形成 `processor -> task.inner`，为后续跨核信号/退出消除锁序负担。
            let next_task_cx_ptr = {
                let mut task_inner = task.acquire_inner_lock();
                task_inner.update_process_times_schedule_in();
                &task_inner.task_cx as *const TaskContext
            };
            let mut processor = task_state.processor.lock();
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            // 先发布与槽位同生命周期的不变身份，再把 Arc 所有权交给 current。
            // 当前 CPU 在这段期间关中断，不会有本核读者看到半发布状态。
            task_state.current_pid.store(task.pid(), Ordering::Relaxed);
            task_state
                .current_tid
                .store(task.gettid(), Ordering::Relaxed);
            processor.current = Some(task);
            // 手动释放处理器
            drop(processor);
            sched_record_stage(
                sched_profile,
                &SCHED_STAGE_SWITCH_PREP_CALLS,
                &SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL,
                &SCHED_STAGE_SWITCH_PREP_CYCLES_MAX,
                stage_t0,
            );
            if sched_profile {
                SCHED_SWITCHES.fetch_add(1, SchedOrdering::Relaxed);
            }
            sched_record_loop_cycles(sched_profile, loop_t0);
            #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
            if trace_first_switch {
                // 安全性：选中任务仍由 `processor.current` 持有，在 `__switch` 使用
                // 该上下文前不会发生修改。
                let (resume_pc, resume_sp) = unsafe { (&*next_task_cx_ptr).bringup_resume_state() };
                println!(
                    "[bringup][sched:01] switching idle -> init: pid={} tid={} task_cx={:#x} resume_pc={:#x} expected_pc={:#x} resume_sp={:#x}",
                    current_pid(),
                    current_tid(),
                    next_task_cx_ptr as usize,
                    resume_pc,
                    crate::hal::trap_return as usize,
                    resume_sp
                );
            }
            // Safety: `idle_task_cx_ptr` points into this CPU's `Processor.idle_task_cx`
            // and `next_task_cx_ptr` points into the selected task's TCB. The
            // processor lock has been dropped, so the switched-in task can later
            // call `schedule()` without deadlocking on the local processor lock.
            unsafe {
                crate::task::perf::record_context_switch();
                __switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
            // 此时已经运行在 idle 栈上。先撤销 current slot，再根据任务留下的
            // Running/Blocking/Zombie 状态完成唯一一次容器交接。
            finish_current_switch_out(cpu);
            #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
            if trace_first_switch {
                println!("[bringup][sched:02] first init context returned to idle scheduler");
            }
        } else {
            // 没有就绪的任务 → CPU idle
            let stage_t0 = sched_profile_start(sched_profile);
            if schedule_tick % IDLE_NET_POLL_INTERVAL == 0 {
                NET_INTERFACE.poll();
            } else {
                spin_loop();
            }
            sched_record_stage(
                sched_profile,
                &SCHED_STAGE_IDLE_CALLS,
                &SCHED_STAGE_IDLE_CYCLES_TOTAL,
                &SCHED_STAGE_IDLE_CYCLES_MAX,
                stage_t0,
            );
            sched_record_loop_cycles(sched_profile, loop_t0);
        }
    }
}

/// 清空与本 CPU current 槽位同生命周期的无锁快照。
fn clear_current_task_cache(task_state: &CpuTaskState) {
    task_state.current_pid.store(0, Ordering::Relaxed);
    task_state.current_tid.store(0, Ordering::Relaxed);
    task_state.current_syscall_id.store(0, Ordering::Relaxed);
}

/// 在 idle 栈上收回 current slot，并把上一任务交给调度状态机收尾。
fn finish_current_switch_out(cpu: usize) {
    let task_state = crate::smp::local_task_state();
    let task = {
        let mut processor = task_state.processor.lock();
        clear_current_task_cache(task_state);
        processor
            .take_current()
            .expect("idle resumed without a current task")
    };
    finish_switch_out(task, cpu);
}

/// 获取当前正在运行任务的 `Arc`。
///
/// # Semantics
///
/// 从本 CPU 的 current 槽位克隆强引用。不再发布裸指针，因此返回的
/// `Arc` 可以安全跨越普通函数调用，但仍不应跨非返回的 context switch。
#[inline(always)]
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    crate::smp::local_task_state().processor.lock().current()
}

/// panic 诊断使用的非阻塞 current 读取。
///
/// `Err(())` 表示 CPU-local 尚未安装或本 CPU processor 锁已被占用；
/// `Ok(None)` 才表示已经确认 current 槽为空。
pub(crate) fn try_current_task() -> Result<Option<Arc<TaskControlBlock>>, ()> {
    let task_state = crate::smp::try_local_task_state().ok_or(())?;
    let processor = task_state.processor.try_lock().ok_or(())?;
    Ok(processor.current())
}

#[inline(always)]
pub fn current_pid() -> usize {
    crate::smp::try_local_task_state()
        .map(|state| state.current_pid.load(Ordering::Relaxed))
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_tid() -> usize {
    crate::smp::try_local_task_state()
        .map(|state| state.current_tid.load(Ordering::Relaxed))
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_parent_pid() -> usize {
    current_task()
        .map(|task| task.process.parent_pid())
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_pgid() -> usize {
    current_task()
        .map(|task| task.process.getpgid())
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_sid() -> usize {
    current_task()
        .map(|task| task.process.getsid())
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_uid() -> u32 {
    current_task().map(|task| task.uid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_euid() -> u32 {
    current_task().map(|task| task.euid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_suid() -> u32 {
    current_task().map(|task| task.suid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_gid() -> u32 {
    current_task().map(|task| task.gid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_egid() -> u32 {
    current_task().map(|task| task.egid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_sgid() -> u32 {
    current_task().map(|task| task.sgid()).unwrap_or(0)
}

/// 获取当前系统调用名称（用于 OOM 诊断）。
pub fn current_syscall_name() -> &'static str {
    let Some(task_state) = crate::smp::try_local_task_state() else {
        return "<none>";
    };
    match task_state.current_syscall_id.load(Ordering::Relaxed) {
        0 => "<none>",
        id => crate::syscall::syscall_name(id - 1),
    }
}

/// 设置当前系统调用 ID。
///
/// # Semantics
///
/// 默认性能构建不会记录该字段；仅在 `heap_trace` 或 `perf_stats` 构建中写入。
#[inline(always)]
pub fn set_current_syscall_id(id: Option<usize>) {
    if cfg!(any(feature = "heap_trace", feature = "perf_stats")) {
        if let Some(task_state) = crate::smp::try_local_task_state() {
            task_state
                .current_syscall_id
                .store(id.map(|id| id + 1).unwrap_or(0), Ordering::Relaxed);
        }
    }
}

/// 获取当前任务的用户态页表 token。
#[inline(always)]
pub fn try_current_user_token() -> Option<usize> {
    current_task().map(|task| task.process.user_token())
}

/// 获取当前任务的用户态页表 token。
///
/// # Panics
///
/// 当前处理器没有运行任务时 panic。
#[inline(always)]
pub fn current_user_token() -> usize {
    try_current_user_token().unwrap()
}

/// 获取当前任务的陷阱上下文。
///
/// # Locking
///
/// 返回的引用来自当前任务 inner 锁保护的数据。调用方只能在立即读写 trap
/// context 的短路径中使用，不能跨阻塞点保存。
pub fn current_trap_cx() -> &'static mut TrapContext {
    current_task()
        .unwrap()
        .acquire_inner_lock()
        .get_trap_cx()
}

/// 从当前任务切换回 idle 调度上下文。
///
/// # Semantics
///
/// `switched_task_cx_ptr` 必须指向当前任务的 `TaskContext`，用于保存被切出时
/// 的 callee-saved 寄存器。函数在 idle 再次调度该任务时返回。
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    // `TaskContext` / 切换汇编不保存 sstatus.SIE 或 CRMD.IE。任务可能
    // 从 syscall 受控窗口带着开中断状态进入；必须在获取任何 scheduler
    // 锁之前先快照并关闭，让 idle 始终接管 IRQ-off CPU。
    //
    // idle 调度器在整个调度循环期间保持 IRQ 关闭。当前任务被切走后
    // `__switch` 不会直接恢复其 IRQ 状态；只有当该任务再次被调度回来
    // 并从 `schedule()` 返回时，才通过 `local_irq_restore` 还原之前的
    // 中断状态。
    let irq_was_enabled = crate::hal::local_irq_save();
    // idle 上下文必须来自正在执行该任务的同一 CPU；任务迁移只能
    // 发生在下次 dispatch 之前，不能在一次 context switch 中途更换归属。
    let idle_task_cx_ptr = crate::smp::local_task_state()
        .processor
        .lock()
        .get_idle_task_cx_ptr();
    if sched_profile_enabled() {
        SCHED_SWITCHES.fetch_add(1, SchedOrdering::Relaxed);
    }
    // Safety: `switched_task_cx_ptr` is provided by the currently running TCB
    // and `idle_task_cx_ptr` points into this CPU's idle context. The local
    // processor lock is not held across the assembly context switch.
    unsafe {
        crate::task::perf::record_context_switch();
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
    // 只有原任务被再次调度时 `__switch` 才会返回。idle 切回任务时
    // 仍为关中断，因此这里才恢复该任务自己的入口快照。
    crate::hal::local_irq_restore(irq_was_enabled);
}

// ── sched debug profile counters ────────────────────────────────────────
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering as SchedOrdering};

static SCHED_PROFILE_ENABLED: AtomicBool = AtomicBool::new(false);
static SCHED_LOOPS: AtomicU64 = AtomicU64::new(0);
static SCHED_FETCH: AtomicU64 = AtomicU64::new(0);
static SCHED_IDLE: AtomicU64 = AtomicU64::new(0);
static SCHED_SWITCHES: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_INTS: AtomicU64 = AtomicU64::new(0);
static SCHED_LOOP_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_LOOP_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_RECLAIM_CALL_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_RECLAIM_CALL_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_READY_LEN_SUM: AtomicU64 = AtomicU64::new(0);
static SCHED_READY_LEN_SAMPLES: AtomicU64 = AtomicU64::new(0);
static SCHED_READY_LEN_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_INTERRUPTIBLE_LEN_SUM: AtomicU64 = AtomicU64::new(0);
static SCHED_INTERRUPTIBLE_LEN_SAMPLES: AtomicU64 = AtomicU64::new(0);
static SCHED_INTERRUPTIBLE_LEN_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_CONSOLE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_CONSOLE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_CONSOLE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_WAKE_EXPIRED_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_WAKE_EXPIRED_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_WAKE_EXPIRED_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_NET_POLL_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_NET_POLL_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_NET_POLL_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_RECLAIM_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_RECLAIM_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_RECLAIM_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_ZOMBIE_QUEUE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_STALE_ZOMBIE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_STALE_ZOMBIE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_STALE_ZOMBIE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FUTEX_COMPACT_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FUTEX_COMPACT_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FUTEX_COMPACT_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FETCH_TASK_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FETCH_TASK_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FETCH_TASK_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_QUEUE_SAMPLE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_QUEUE_SAMPLE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_QUEUE_SAMPLE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_SWITCH_PREP_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_SWITCH_PREP_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_IDLE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_IDLE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_IDLE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_TRAP_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_TRAP_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_HANDLER_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_HANDLER_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_PROGRAM_TIMER_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_PROGRAM_TIMER_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_PROGRAM_TIMER_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_SBI_SET_TIMER_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_SBI_SET_TIMER_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_SBI_SET_TIMER_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

#[inline(always)]
fn sched_profile_enabled() -> bool {
    SCHED_PROFILE_ENABLED.load(SchedOrdering::Relaxed)
}

#[inline(always)]
fn sched_rdcycle() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let cycles: usize;
        // Safety: `rdcycle` only reads the architectural cycle counter and
        // writes the output register.
        unsafe {
            core::arch::asm!("rdcycle {}", out(reg) cycles);
        }
        cycles as u64
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let lo: usize;
        let hi: usize;
        // Safety: `rdtime.d` only reads the architectural timer and writes the
        // two output registers.
        unsafe {
            core::arch::asm!("rdtime.d {}, {}", out(reg) lo, out(reg) hi);
        }
        lo as u64
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        0
    }
}
fn sched_atomic_max(slot: &AtomicU64, v: u64) {
    let mut cur = slot.load(SchedOrdering::Relaxed);
    while v > cur {
        match slot.compare_exchange_weak(cur, v, SchedOrdering::Relaxed, SchedOrdering::Relaxed) {
            Ok(_) => break,
            Err(n) => cur = n,
        }
    }
}

#[inline(always)]
fn sched_profile_start(enabled: bool) -> u64 {
    if enabled {
        sched_rdcycle()
    } else {
        0
    }
}

#[inline(always)]
pub(crate) fn sched_profile_cycle_start() -> u64 {
    sched_profile_start(sched_profile_enabled())
}

#[inline(always)]
fn sched_record_cycles(enabled: bool, total: &AtomicU64, max: &AtomicU64, start: u64) {
    if !enabled {
        return;
    }
    let dt = sched_rdcycle().saturating_sub(start);
    total.fetch_add(dt, SchedOrdering::Relaxed);
    sched_atomic_max(max, dt);
}

#[inline(always)]
fn sched_record_stage(
    enabled: bool,
    calls: &AtomicU64,
    total: &AtomicU64,
    max: &AtomicU64,
    start: u64,
) {
    if !enabled {
        return;
    }
    calls.fetch_add(1, SchedOrdering::Relaxed);
    sched_record_cycles(true, total, max, start);
}

#[inline(always)]
fn sched_record_loop_cycles(enabled: bool, start: u64) {
    sched_record_cycles(
        enabled,
        &SCHED_LOOP_CYCLES_TOTAL,
        &SCHED_LOOP_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
fn sched_record_queue_sample(ready_len: u64, interruptible_len: u64) {
    if !sched_profile_enabled() {
        return;
    }
    SCHED_READY_LEN_SUM.fetch_add(ready_len, SchedOrdering::Relaxed);
    SCHED_READY_LEN_SAMPLES.fetch_add(1, SchedOrdering::Relaxed);
    sched_atomic_max(&SCHED_READY_LEN_MAX, ready_len);
    SCHED_INTERRUPTIBLE_LEN_SUM.fetch_add(interruptible_len, SchedOrdering::Relaxed);
    SCHED_INTERRUPTIBLE_LEN_SAMPLES.fetch_add(1, SchedOrdering::Relaxed);
    sched_atomic_max(&SCHED_INTERRUPTIBLE_LEN_MAX, interruptible_len);
}

#[inline(always)]
pub(crate) fn record_sched_timer_interrupt() {
    if !sched_profile_enabled() {
        return;
    }
    SCHED_TIMER_INTS.fetch_add(1, SchedOrdering::Relaxed);
}

#[inline(always)]
pub(crate) fn record_sched_timer_trap_cycles(start: u64) {
    sched_record_cycles(
        sched_profile_enabled(),
        &SCHED_TIMER_TRAP_CYCLES_TOTAL,
        &SCHED_TIMER_TRAP_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
pub(crate) fn record_sched_timer_handler_cycles(start: u64) {
    sched_record_cycles(
        sched_profile_enabled(),
        &SCHED_TIMER_HANDLER_CYCLES_TOTAL,
        &SCHED_TIMER_HANDLER_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
pub(crate) fn record_sched_program_timer_cycles(start: u64) {
    if !sched_profile_enabled() {
        return;
    }
    SCHED_PROGRAM_TIMER_CALLS.fetch_add(1, SchedOrdering::Relaxed);
    sched_record_cycles(
        true,
        &SCHED_PROGRAM_TIMER_CYCLES_TOTAL,
        &SCHED_PROGRAM_TIMER_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
pub(crate) fn record_sched_sbi_set_timer_cycles(start: u64) {
    if !sched_profile_enabled() {
        return;
    }
    SCHED_SBI_SET_TIMER_CALLS.fetch_add(1, SchedOrdering::Relaxed);
    sched_record_cycles(
        true,
        &SCHED_SBI_SET_TIMER_CYCLES_TOTAL,
        &SCHED_SBI_SET_TIMER_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
fn reset_stage(calls: &AtomicU64, total: &AtomicU64, max: &AtomicU64) {
    calls.store(0, SchedOrdering::Relaxed);
    total.store(0, SchedOrdering::Relaxed);
    max.store(0, SchedOrdering::Relaxed);
}

pub fn reset_sched_profile() {
    SCHED_PROFILE_ENABLED.store(false, SchedOrdering::Relaxed);
    SCHED_LOOPS.store(0, SchedOrdering::Relaxed);
    SCHED_FETCH.store(0, SchedOrdering::Relaxed);
    SCHED_IDLE.store(0, SchedOrdering::Relaxed);
    SCHED_SWITCHES.store(0, SchedOrdering::Relaxed);
    SCHED_TIMER_INTS.store(0, SchedOrdering::Relaxed);
    SCHED_LOOP_CYCLES_TOTAL.store(0, SchedOrdering::Relaxed);
    SCHED_LOOP_CYCLES_MAX.store(0, SchedOrdering::Relaxed);
    SCHED_RECLAIM_CALL_CYCLES_TOTAL.store(0, SchedOrdering::Relaxed);
    SCHED_RECLAIM_CALL_CYCLES_MAX.store(0, SchedOrdering::Relaxed);
    SCHED_READY_LEN_SUM.store(0, SchedOrdering::Relaxed);
    SCHED_READY_LEN_SAMPLES.store(0, SchedOrdering::Relaxed);
    SCHED_READY_LEN_MAX.store(0, SchedOrdering::Relaxed);
    SCHED_INTERRUPTIBLE_LEN_SUM.store(0, SchedOrdering::Relaxed);
    SCHED_INTERRUPTIBLE_LEN_SAMPLES.store(0, SchedOrdering::Relaxed);
    SCHED_INTERRUPTIBLE_LEN_MAX.store(0, SchedOrdering::Relaxed);
    reset_stage(
        &SCHED_STAGE_CONSOLE_CALLS,
        &SCHED_STAGE_CONSOLE_CYCLES_TOTAL,
        &SCHED_STAGE_CONSOLE_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_WAKE_EXPIRED_CALLS,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_TOTAL,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_NET_POLL_CALLS,
        &SCHED_STAGE_NET_POLL_CYCLES_TOTAL,
        &SCHED_STAGE_NET_POLL_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_RECLAIM_CALLS,
        &SCHED_STAGE_RECLAIM_CYCLES_TOTAL,
        &SCHED_STAGE_RECLAIM_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_ZOMBIE_QUEUE_CALLS,
        &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_TOTAL,
        &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_STALE_ZOMBIE_CALLS,
        &SCHED_STAGE_STALE_ZOMBIE_CYCLES_TOTAL,
        &SCHED_STAGE_STALE_ZOMBIE_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_FUTEX_COMPACT_CALLS,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_TOTAL,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_FETCH_TASK_CALLS,
        &SCHED_STAGE_FETCH_TASK_CYCLES_TOTAL,
        &SCHED_STAGE_FETCH_TASK_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_QUEUE_SAMPLE_CALLS,
        &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_TOTAL,
        &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_SWITCH_PREP_CALLS,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_IDLE_CALLS,
        &SCHED_STAGE_IDLE_CYCLES_TOTAL,
        &SCHED_STAGE_IDLE_CYCLES_MAX,
    );
    SCHED_TIMER_TRAP_CYCLES_TOTAL.store(0, SchedOrdering::Relaxed);
    SCHED_TIMER_TRAP_CYCLES_MAX.store(0, SchedOrdering::Relaxed);
    SCHED_TIMER_HANDLER_CYCLES_TOTAL.store(0, SchedOrdering::Relaxed);
    SCHED_TIMER_HANDLER_CYCLES_MAX.store(0, SchedOrdering::Relaxed);
    reset_stage(
        &SCHED_PROGRAM_TIMER_CALLS,
        &SCHED_PROGRAM_TIMER_CYCLES_TOTAL,
        &SCHED_PROGRAM_TIMER_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_SBI_SET_TIMER_CALLS,
        &SCHED_SBI_SET_TIMER_CYCLES_TOTAL,
        &SCHED_SBI_SET_TIMER_CYCLES_MAX,
    );
    SCHED_PROFILE_ENABLED.store(true, SchedOrdering::Relaxed);
}

pub fn disable_sched_profile() {
    SCHED_PROFILE_ENABLED.store(false, SchedOrdering::Relaxed);
}

fn dump_stage(label: &str, calls: &AtomicU64, total: &AtomicU64, max: &AtomicU64) {
    println!(
        "sched_stage_{} calls={} cycles_total={} cycles_max={}",
        label,
        calls.load(SchedOrdering::Relaxed),
        total.load(SchedOrdering::Relaxed),
        max.load(SchedOrdering::Relaxed)
    );
}

pub fn dump_sched_profile(label: &str) {
    println!("[sched_profile] {}", label);
    println!(
        "sched_profile enabled={}",
        SCHED_PROFILE_ENABLED.load(SchedOrdering::Relaxed) as usize
    );
    println!(
        "sched loops={} fetch={} idle={} switches={} timer_ints={}",
        SCHED_LOOPS.load(SchedOrdering::Relaxed),
        SCHED_FETCH.load(SchedOrdering::Relaxed),
        SCHED_IDLE.load(SchedOrdering::Relaxed),
        SCHED_SWITCHES.load(SchedOrdering::Relaxed),
        SCHED_TIMER_INTS.load(SchedOrdering::Relaxed)
    );
    println!("sched loop_cycles_total={} loop_cycles_max={} reclaim_call_cycles_total={} reclaim_call_cycles_max={}",
        SCHED_LOOP_CYCLES_TOTAL.load(SchedOrdering::Relaxed), SCHED_LOOP_CYCLES_MAX.load(SchedOrdering::Relaxed),
        SCHED_RECLAIM_CALL_CYCLES_TOTAL.load(SchedOrdering::Relaxed), SCHED_RECLAIM_CALL_CYCLES_MAX.load(SchedOrdering::Relaxed));
    let ready_samples = SCHED_READY_LEN_SAMPLES.load(SchedOrdering::Relaxed);
    let interruptible_samples = SCHED_INTERRUPTIBLE_LEN_SAMPLES.load(SchedOrdering::Relaxed);
    println!("sched ready_len_sum={} ready_len_samples={} ready_len_max={} ready_len_avg={} interruptible_len_sum={} interruptible_len_samples={} interruptible_len_max={} interruptible_len_avg={}",
        SCHED_READY_LEN_SUM.load(SchedOrdering::Relaxed),
        ready_samples,
        SCHED_READY_LEN_MAX.load(SchedOrdering::Relaxed),
        if ready_samples > 0 {
            SCHED_READY_LEN_SUM.load(SchedOrdering::Relaxed) / ready_samples
        } else {
            0
        },
        SCHED_INTERRUPTIBLE_LEN_SUM.load(SchedOrdering::Relaxed),
        interruptible_samples,
        SCHED_INTERRUPTIBLE_LEN_MAX.load(SchedOrdering::Relaxed),
        if interruptible_samples > 0 {
            SCHED_INTERRUPTIBLE_LEN_SUM.load(SchedOrdering::Relaxed) / interruptible_samples
        } else {
            0
        });
    dump_stage(
        "console",
        &SCHED_STAGE_CONSOLE_CALLS,
        &SCHED_STAGE_CONSOLE_CYCLES_TOTAL,
        &SCHED_STAGE_CONSOLE_CYCLES_MAX,
    );
    dump_stage(
        "wake_expired",
        &SCHED_STAGE_WAKE_EXPIRED_CALLS,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_TOTAL,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_MAX,
    );
    dump_stage(
        "net_poll",
        &SCHED_STAGE_NET_POLL_CALLS,
        &SCHED_STAGE_NET_POLL_CYCLES_TOTAL,
        &SCHED_STAGE_NET_POLL_CYCLES_MAX,
    );
    dump_stage(
        "reclaim",
        &SCHED_STAGE_RECLAIM_CALLS,
        &SCHED_STAGE_RECLAIM_CYCLES_TOTAL,
        &SCHED_STAGE_RECLAIM_CYCLES_MAX,
    );
    dump_stage(
        "zombie_queue",
        &SCHED_STAGE_ZOMBIE_QUEUE_CALLS,
        &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_TOTAL,
        &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_MAX,
    );
    dump_stage(
        "stale_zombie",
        &SCHED_STAGE_STALE_ZOMBIE_CALLS,
        &SCHED_STAGE_STALE_ZOMBIE_CYCLES_TOTAL,
        &SCHED_STAGE_STALE_ZOMBIE_CYCLES_MAX,
    );
    dump_stage(
        "futex_compact",
        &SCHED_STAGE_FUTEX_COMPACT_CALLS,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_TOTAL,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_MAX,
    );
    dump_stage(
        "fetch_task",
        &SCHED_STAGE_FETCH_TASK_CALLS,
        &SCHED_STAGE_FETCH_TASK_CYCLES_TOTAL,
        &SCHED_STAGE_FETCH_TASK_CYCLES_MAX,
    );
    dump_stage(
        "queue_sample",
        &SCHED_STAGE_QUEUE_SAMPLE_CALLS,
        &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_TOTAL,
        &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_MAX,
    );
    dump_stage(
        "switch_prep",
        &SCHED_STAGE_SWITCH_PREP_CALLS,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_MAX,
    );
    dump_stage(
        "idle",
        &SCHED_STAGE_IDLE_CALLS,
        &SCHED_STAGE_IDLE_CYCLES_TOTAL,
        &SCHED_STAGE_IDLE_CYCLES_MAX,
    );
    println!(
        "sched_timer trap_cycles_total={} trap_cycles_max={} handler_cycles_total={} handler_cycles_max={} program_timer_calls={} program_timer_cycles_total={} program_timer_cycles_max={} sbi_set_timer_calls={} sbi_set_timer_cycles_total={} sbi_set_timer_cycles_max={}",
        SCHED_TIMER_TRAP_CYCLES_TOTAL.load(SchedOrdering::Relaxed),
        SCHED_TIMER_TRAP_CYCLES_MAX.load(SchedOrdering::Relaxed),
        SCHED_TIMER_HANDLER_CYCLES_TOTAL.load(SchedOrdering::Relaxed),
        SCHED_TIMER_HANDLER_CYCLES_MAX.load(SchedOrdering::Relaxed),
        SCHED_PROGRAM_TIMER_CALLS.load(SchedOrdering::Relaxed),
        SCHED_PROGRAM_TIMER_CYCLES_TOTAL.load(SchedOrdering::Relaxed),
        SCHED_PROGRAM_TIMER_CYCLES_MAX.load(SchedOrdering::Relaxed),
        SCHED_SBI_SET_TIMER_CALLS.load(SchedOrdering::Relaxed),
        SCHED_SBI_SET_TIMER_CYCLES_TOTAL.load(SchedOrdering::Relaxed),
        SCHED_SBI_SET_TIMER_CYCLES_MAX.load(SchedOrdering::Relaxed)
    );
}
