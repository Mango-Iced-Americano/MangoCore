//! 单核处理器状态和调度主循环。
//!
//! `PROCESSOR` 保存当前任务和 idle 上下文。热路径通过一组 relaxed atomic
//! 缓存当前任务身份信息，减少 syscall 入口查询当前任务时获取调度器锁的成本。
//!
//! # Locking
//!
//! `PROCESSOR` 锁只保护当前任务槽和 idle 上下文指针。切换到任务前必须释放该锁，
//! 否则切回调度器时会形成自锁。
//!
//! # Safety
//!
//! `CURRENT_TASK_PTR` 只在单核调度器持有 `PROCESSOR.current` 的强引用期间发布。
//! `take_current_task()` 在切走当前任务前清空该指针。

use super::{
    __switch, do_wake_expired, has_zombie_queue_tasks_fast, take_one_interruptible_zombie,
    take_one_ready_zombie, take_zombie_tasks,
};
use super::{fetch_task, TaskStatus};
use super::{TaskContext, TaskControlBlock};
use crate::hal::TrapContext;
use crate::net::config::NET_INTERFACE;
use alloc::sync::Arc;
use core::hint::spin_loop;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use lazy_static::*;
use spin::Mutex;

const BACKGROUND_NET_POLL_INTERVAL: usize = 64;
const IDLE_NET_POLL_INTERVAL: usize = 64;

#[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
static BOARD_FIRST_TASK_SWITCH: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 当前 CPU 的调度状态。
///
/// # Semantics
///
/// MangoCore 当前只支持单核运行，因此该对象同时代表全局处理器状态。
pub struct Processor {
    /// 当前正在运行的任务
    current: Option<Arc<TaskControlBlock>>,
    /// 空闲任务的上下文，用于在任务切换时保存和恢复状态
    idle_task_cx: TaskContext,
}

impl Processor {
    pub fn new() -> Self {
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

    /// 取出当前正在运行的任务。
    ///
    /// # Semantics
    ///
    /// 调用方随后必须把任务重新入队、转为 zombie，或完成退出清理。
    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        // 将current字段置空，并返回其中的值
        self.current.take()
    }
    /// 克隆当前正在运行的任务。
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)
    }

    /// 返回当前处理器是否没有运行任务。
    pub fn is_vacant(&self) -> bool {
        self.current.is_none()
    }
}

/// 当前正在执行的系统调用 ID（用于诊断构建）。
///
/// 默认性能构建不维护该字段，避免每次 syscall 入口产生原子写开销。
/// 诊断构建中 0 表示无记录，实际 syscall id 存为 id + 1。
static CURRENT_SYSCALL_ID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_TASK_PTR: AtomicPtr<TaskControlBlock> = AtomicPtr::new(ptr::null_mut());
static CURRENT_PID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_TID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_PARENT_PID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_USER_TOKEN: AtomicUsize = AtomicUsize::new(0);
static CURRENT_UID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_EUID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_SUID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_GID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_EGID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_SGID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_PGID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_SID: AtomicUsize = AtomicUsize::new(0);

lazy_static! {
    /// 全局处理器对象。
    ///
    /// # Locking
    ///
    /// 持锁期间只能更新当前任务槽或读取 idle 上下文指针，不能跨 `__switch`
    /// 或任何可能阻塞的路径持有该锁。
    pub static ref PROCESSOR: Mutex<Processor> = Mutex::new(Processor::new());
}

/// 运行调度主循环。
///
/// # Semantics
///
/// 循环执行定时唤醒、网络轮询、文件缓存回收、zombie 清理和 ready 队列取任务。
/// 找到任务后发布当前任务缓存、释放 `PROCESSOR` 锁并切换到任务上下文。
///
/// # Locking
///
/// 调用 `__switch` 前必须释放 `PROCESSOR` 锁；被切入任务后该函数直到任务主动
/// `schedule()` 回 idle 才会继续执行。
pub fn run_tasks() {
    let mut schedule_tick = 0usize;
    loop {
        let sched_profile = sched_profile_enabled();
        if sched_profile {
            SCHED_LOOPS.fetch_add(1, SchedOrdering::Relaxed);
        }
        let loop_t0 = sched_profile_start(sched_profile);
        schedule_tick = schedule_tick.wrapping_add(1);
        #[cfg(target_arch = "riscv64")]
        crate::hal::arch::riscv::plic::report_unhandled_irq();
        #[cfg(target_arch = "riscv64")]
        {
            let stage_t0 = sched_profile_start(sched_profile);
            let irq_pending = crate::hal::arch::riscv::sbi::take_runtime_console_rx_interrupt();
            let polled = crate::hal::arch::riscv::sbi::poll_runtime_console_rx();
            if irq_pending || polled {
                crate::hal::arch::riscv::sbi::drain_runtime_console_rx(|ch| {
                    if crate::trace::check_magic_key(ch, "schedule") {
                        true
                    } else {
                        crate::fs::dev::tty::Teletype::receive_console_char(ch)
                    }
                });
            }
            crate::hal::arch::riscv::sbi::resume_runtime_console_rx(
                crate::fs::dev::tty::Teletype::input_has_space(),
            );
            crate::hal::arch::riscv::sbi::report_runtime_console_rx_overruns();
            sched_record_stage(
                sched_profile,
                &SCHED_STAGE_CONSOLE_CALLS,
                &SCHED_STAGE_CONSOLE_CYCLES_TOTAL,
                &SCHED_STAGE_CONSOLE_CYCLES_MAX,
                stage_t0,
            );
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
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
        let net_interrupt_pending = NET_INTERFACE.take_rx_interrupt();
        if net_interrupt_pending || schedule_tick % BACKGROUND_NET_POLL_INTERVAL == 0 {
            let stage_t0 = sched_profile_start(sched_profile);
            let polled = NET_INTERFACE.try_poll();
            if net_interrupt_pending && !polled {
                NET_INTERFACE.notify_rx_interrupt();
            }
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
                    if t.acquire_inner_lock().is_zombie() {
                        ready_zombie += 1;
                    }
                    if t.sched_nice_hint.load(Ordering::Relaxed) != 0 {
                        nonzero_nice += 1;
                    }
                }
                for t in &manager.interruptible_queue {
                    if t.acquire_inner_lock().is_zombie() {
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
        let mut processor = PROCESSOR.lock();
        let next_task = fetch_task();
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
            #[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
            let trace_first_switch = !BOARD_FIRST_TASK_SWITCH.swap(true, Ordering::Relaxed);
            let stage_t0 = sched_profile_start(sched_profile);
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            // 独占地访问即将运行的任务的 TCB
            let next_task_cx_ptr = {
                let mut task_inner = task.acquire_inner_lock();
                if task_inner.task_status == TaskStatus::Zombie {
                    drop(task_inner);
                    sched_record_stage(
                        sched_profile,
                        &SCHED_STAGE_SWITCH_PREP_CALLS,
                        &SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL,
                        &SCHED_STAGE_SWITCH_PREP_CYCLES_MAX,
                        stage_t0,
                    );
                    sched_record_loop_cycles(sched_profile, loop_t0);
                    continue;
                }
                task_inner.task_status = TaskStatus::Running;
                task_inner.update_process_times_schedule_in();
                &task_inner.task_cx as *const TaskContext
            };
            // 设置当前正在运行的任务
            CURRENT_TASK_PTR.store(
                Arc::as_ptr(&task) as *mut TaskControlBlock,
                Ordering::Relaxed,
            );
            CURRENT_PID.store(task.pid(), Ordering::Relaxed);
            CURRENT_TID.store(task.gettid(), Ordering::Relaxed);
            CURRENT_PARENT_PID.store(task.process.parent_pid(), Ordering::Relaxed);
            CURRENT_USER_TOKEN.store(task.process.user_token(), Ordering::Relaxed);
            CURRENT_UID.store(task.uid() as usize, Ordering::Relaxed);
            CURRENT_EUID.store(task.euid() as usize, Ordering::Relaxed);
            CURRENT_SUID.store(task.suid() as usize, Ordering::Relaxed);
            CURRENT_GID.store(task.gid() as usize, Ordering::Relaxed);
            CURRENT_EGID.store(task.egid() as usize, Ordering::Relaxed);
            CURRENT_SGID.store(task.sgid() as usize, Ordering::Relaxed);
            CURRENT_PGID.store(task.process.getpgid(), Ordering::Relaxed);
            CURRENT_SID.store(task.process.getsid(), Ordering::Relaxed);
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
            #[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
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
            // Safety: `idle_task_cx_ptr` points into `PROCESSOR.idle_task_cx`
            // and `next_task_cx_ptr` points into the selected task's TCB. The
            // processor lock has been dropped, so the switched-in task can later
            // call `schedule()` without deadlocking on `PROCESSOR`.
            unsafe {
                crate::task::perf::record_context_switch();
                __switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
            #[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
            if trace_first_switch {
                println!("[bringup][sched:02] first init context returned to idle scheduler");
            }
        } else {
            // 没有就绪的任务 → CPU idle
            drop(processor);
            let stage_t0 = sched_profile_start(sched_profile);
            let net_interrupt_pending = NET_INTERFACE.take_rx_interrupt();
            if net_interrupt_pending {
                if !NET_INTERFACE.try_poll() {
                    NET_INTERFACE.notify_rx_interrupt();
                }
            } else if schedule_tick % IDLE_NET_POLL_INTERVAL == 0 {
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

/// 取出当前正在运行的任务并清空当前任务缓存。
///
/// # Semantics
///
/// 这是切出当前任务的唯一入口。清空 atomic 缓存后，热路径查询会回退到
/// `PROCESSOR.current` 或返回空闲状态。
#[inline(always)]
pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {
    CURRENT_TASK_PTR.store(ptr::null_mut(), Ordering::Relaxed);
    CURRENT_PID.store(0, Ordering::Relaxed);
    CURRENT_TID.store(0, Ordering::Relaxed);
    CURRENT_PARENT_PID.store(0, Ordering::Relaxed);
    CURRENT_USER_TOKEN.store(0, Ordering::Relaxed);
    CURRENT_UID.store(0, Ordering::Relaxed);
    CURRENT_EUID.store(0, Ordering::Relaxed);
    CURRENT_SUID.store(0, Ordering::Relaxed);
    CURRENT_GID.store(0, Ordering::Relaxed);
    CURRENT_EGID.store(0, Ordering::Relaxed);
    CURRENT_SGID.store(0, Ordering::Relaxed);
    CURRENT_PGID.store(0, Ordering::Relaxed);
    CURRENT_SID.store(0, Ordering::Relaxed);
    PROCESSOR.lock().take_current()
}

/// 获取当前正在运行任务的 `Arc`。
///
/// # Semantics
///
/// 单核调度器在 `CURRENT_TASK_PTR` 非空期间由 `PROCESSOR.current` 持有强引用。
/// 本函数直接从 raw pointer 增加强引用计数，避免 syscall 热路径获取调度器锁。
#[inline(always)]
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    let ptr = CURRENT_TASK_PTR.load(Ordering::Relaxed);
    if ptr.is_null() {
        return None;
    }
    // Safety: MangoCore is single-core. `PROCESSOR.current` owns a strong
    // reference while `CURRENT_TASK_PTR` is published, and `take_current_task`
    // clears the pointer before removing that owner.
    unsafe {
        Arc::increment_strong_count(ptr);
        Some(Arc::from_raw(ptr))
    }
}

/// 获取当前正在运行任务的短生命周期引用。
///
/// MangoCore 当前是单核；调度器在 `PROCESSOR.current` 持有 Arc 时同步发布这个指针，
/// `take_current_task()` 会在切走当前任务前清空它。调用者不能把引用跨调度点保存。
///
/// # Safety
///
/// 返回类型为 `'static` 是为了适配内核内部调用约定，实际生命周期只到下一次
/// 调度点。调用者不能缓存该引用或在释放 CPU 后继续使用。
#[inline(always)]
pub fn current_task_ref() -> Option<&'static TaskControlBlock> {
    let ptr = CURRENT_TASK_PTR.load(Ordering::Relaxed);
    if ptr.is_null() {
        None
    } else {
        // Safety: see the function contract above. The raw pointer is published
        // only while `PROCESSOR.current` owns the task.
        Some(unsafe { &*ptr })
    }
}

#[inline(always)]
pub fn current_pid() -> usize {
    CURRENT_PID.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn current_tid() -> usize {
    CURRENT_TID.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn current_parent_pid() -> usize {
    CURRENT_PARENT_PID.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn current_pgid() -> usize {
    CURRENT_PGID.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn current_sid() -> usize {
    CURRENT_SID.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn current_uid() -> u32 {
    CURRENT_UID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub fn current_euid() -> u32 {
    CURRENT_EUID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub fn current_suid() -> u32 {
    CURRENT_SUID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub fn current_gid() -> u32 {
    CURRENT_GID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub fn current_egid() -> u32 {
    CURRENT_EGID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub fn current_sgid() -> u32 {
    CURRENT_SGID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub(super) fn refresh_current_identity_hints(
    tid: usize,
    uid: u32,
    euid: u32,
    suid: u32,
    gid: u32,
    egid: u32,
    sgid: u32,
) {
    if CURRENT_TID.load(Ordering::Relaxed) == tid {
        CURRENT_UID.store(uid as usize, Ordering::Relaxed);
        CURRENT_EUID.store(euid as usize, Ordering::Relaxed);
        CURRENT_SUID.store(suid as usize, Ordering::Relaxed);
        CURRENT_GID.store(gid as usize, Ordering::Relaxed);
        CURRENT_EGID.store(egid as usize, Ordering::Relaxed);
        CURRENT_SGID.store(sgid as usize, Ordering::Relaxed);
    }
}

#[inline(always)]
pub(super) fn refresh_current_process_group_hints(pid: usize, pgid: usize, sid: usize) {
    if CURRENT_PID.load(Ordering::Relaxed) == pid {
        CURRENT_PGID.store(pgid, Ordering::Relaxed);
        CURRENT_SID.store(sid, Ordering::Relaxed);
    }
}

pub fn refresh_current_user_token_for_process(pid: usize, token: usize) {
    if CURRENT_PID.load(Ordering::Relaxed) == pid {
        CURRENT_USER_TOKEN.store(token, Ordering::Relaxed);
    }
}

/// 获取当前系统调用名称（用于 OOM 诊断）。
pub fn current_syscall_name() -> &'static str {
    match CURRENT_SYSCALL_ID.load(Ordering::Relaxed) {
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
        CURRENT_SYSCALL_ID.store(id.map(|id| id + 1).unwrap_or(0), Ordering::Relaxed);
    }
}

/// 获取当前任务的用户态页表 token。
#[inline(always)]
pub fn try_current_user_token() -> Option<usize> {
    let token = CURRENT_USER_TOKEN.load(Ordering::Relaxed);
    if token != 0 {
        Some(token)
    } else {
        current_task_ref().map(|task| task.get_user_token())
    }
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
    current_task_ref()
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
    // 获取空闲任务的上下文指针
    let idle_task_cx_ptr = PROCESSOR.lock().get_idle_task_cx_ptr();
    if sched_profile_enabled() {
        SCHED_SWITCHES.fetch_add(1, SchedOrdering::Relaxed);
    }
    // Safety: `switched_task_cx_ptr` is provided by the currently running TCB
    // and `idle_task_cx_ptr` points into `PROCESSOR.idle_task_cx`. The
    // `PROCESSOR` lock is not held across the assembly context switch.
    unsafe {
        crate::task::perf::record_context_switch();
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
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
