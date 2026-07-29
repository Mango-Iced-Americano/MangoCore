//! SMP 启动阶段的 focused ktest。

use alloc::{sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::kernel_tests::runner::KernelTest;

const IRQ_PROBE_NOT_RUN: usize = 0;
const IRQ_PROBE_DISABLED: usize = 1;
const IRQ_PROBE_ENABLED: usize = 2;
static IDLE_TO_TASK_IRQ_PROBE: AtomicUsize = AtomicUsize::new(IRQ_PROBE_NOT_RUN);
static SCHED_STATE_HELPER_RUNS: AtomicUsize = AtomicUsize::new(0);
static AP_TASK_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_TASK_RUNS: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
static AP_BLOCKED_WAKE_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_BLOCKED_WAKE_PHASE: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
static AP_KSTACK_RECLAIM_RUNS: AtomicUsize = AtomicUsize::new(0);
static AP_KSTACK_RECLAIM_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_USER_TLB_RETIRE_PHASE: AtomicUsize = AtomicUsize::new(0);
static AP_USER_TLB_FREE_DURING_WAIT: AtomicUsize = AtomicUsize::new(usize::MAX);
static AP_USER_TLB_REQUEST_BEFORE: AtomicUsize = AtomicUsize::new(0);

lazy_static! {
    static ref SCHED_STATE_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    static ref AP_BLOCKED_WAKE_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    static ref USER_TLB_RETIRE_VM: Mutex<Option<Arc<crate::mm::AddressSpace<crate::hal::PageTableImpl>>>> =
        Mutex::new(None);
}

/// 返回只依赖 Phase 1 启动不变量的测试集合。
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "smp::configured_cpus_are_online",
            configured_cpus_are_online,
        ),
        KernelTest::new(
            "smp::ktest_runner_stays_on_boot_cpu",
            ktest_runner_stays_on_boot_cpu,
        ),
        KernelTest::new(
            "smp::configured_cpus_enter_scheduler",
            configured_cpus_enter_scheduler,
        ),
        KernelTest::new(
            "smp::secondary_cpus_enter_idle_context",
            secondary_cpus_enter_idle_context,
        ),
        KernelTest::new("smp::bsp_to_ap_ipi_ping", bsp_to_ap_ipi_ping),
        KernelTest::new(
            "smp::bsp_broadcasts_ipi_to_all_aps",
            bsp_broadcasts_ipi_to_all_aps,
        ),
        KernelTest::new(
            "smp::kernel_timer_irq_is_deferred",
            kernel_timer_irq_is_deferred,
        ),
        KernelTest::new("smp::ap_to_bsp_ipi_round_trip", ap_to_bsp_ipi_round_trip),
        KernelTest::new(
            "smp::syscall_irq_window_survives_schedule",
            syscall_irq_window_survives_schedule,
        ),
        KernelTest::new(
            "smp::scheduler_state_has_unique_owner",
            scheduler_state_has_unique_owner,
        ),
        KernelTest::new(
            "smp::remote_kernel_tasks_run_on_target_cpus",
            remote_kernel_tasks_run_on_target_cpus,
        ),
        KernelTest::new(
            "smp::blocked_kernel_tasks_wake_on_last_cpu",
            blocked_kernel_tasks_wake_on_last_cpu,
        ),
        KernelTest::new(
            "smp::user_tlb_full_flush_reaches_online_cpus",
            user_tlb_full_flush_reaches_online_cpus,
        ),
        KernelTest::new(
            "smp::user_tlb_page_sync_uses_arch_backend",
            user_tlb_page_sync_uses_arch_backend,
        ),
        KernelTest::new(
            "smp::user_tlb_retirement_waits_for_ack",
            user_tlb_retirement_waits_for_ack,
        ),
        KernelTest::new(
            "smp::kernel_stack_reclaim_waits_for_shootdown",
            kernel_stack_reclaim_waits_for_shootdown,
        ),
        KernelTest::terminal(
            "smp::secondary_cpus_stop_and_ack",
            secondary_cpus_stop_and_ack,
        ),
    ]
}

/// 启动函数返回后，配置拓扑中的每个 CPU 都必须已经发布 online。
fn configured_cpus_are_online() -> Result<(), &'static str> {
    let configured = crate::smp::configured_cpu_count();
    let expected = (1usize << configured) - 1;
    let online = crate::smp::online_cpu_mask();

    if online != expected {
        crate::println!(
            "# SMP topology mismatch: configured={} expected={:#x} online={:#x}",
            configured,
            expected,
            online
        );
        return Err("configured CPU set is not fully online");
    }
    Ok(())
}

/// CPU0 逐个唤醒 AP，并等待目标 CPU 在硬中断上下文发布 ack。
fn bsp_to_ap_ipi_ping() -> Result<(), &'static str> {
    let timeout_ticks = crate::hal::get_clock_freq();
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        let expected = match crate::smp::send_ipi_ping(cpu_id) {
            Ok(expected) => expected,
            Err(error) => {
                crate::println!("# SMP IPI send failed: cpu={} error={}", cpu_id, error);
                return Err("failed to send BSP-to-AP IPI");
            }
        };
        let deadline = crate::hal::get_time().saturating_add(timeout_ticks);
        while crate::smp::ipi_ping_ack(cpu_id) != expected {
            if crate::hal::get_time() >= deadline {
                crate::println!(
                    "# SMP IPI ack timeout: cpu={} expected={} observed={}",
                    cpu_id,
                    expected,
                    crate::smp::ipi_ping_ack(cpu_id)
                );
                return Err("AP did not acknowledge IPI");
            }
            core::hint::spin_loop();
        }
    }
    Ok(())
}

/// CPU0 先发布全部 AP 的 mailbox，再连续敲响 doorbell，最后逐项等待 ack。
fn bsp_broadcasts_ipi_to_all_aps() -> Result<(), &'static str> {
    let targets = crate::smp::online_cpu_mask() & !(1usize << crate::smp::BOOT_CPU_ID);
    let mut expected = [0usize; crate::smp::MAX_CPUS];
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        expected[cpu_id] = crate::smp::ipi_ping_ack(cpu_id).wrapping_add(1);
    }

    if let Err(error) = crate::smp::send_ipi_mask(targets, crate::smp::IpiReason::PING) {
        crate::println!(
            "# SMP IPI broadcast failed: targets={:#x} error={}",
            targets,
            error
        );
        return Err("failed to broadcast BSP-to-AP IPI");
    }

    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        while crate::smp::ipi_ping_ack(cpu_id) != expected[cpu_id] {
            if crate::hal::get_time() >= deadline {
                crate::println!(
                    "# SMP IPI broadcast ack timeout: cpu={} expected={} observed={}",
                    cpu_id,
                    expected[cpu_id],
                    crate::smp::ipi_ping_ack(cpu_id)
                );
                return Err("AP did not acknowledge broadcast IPI");
            }
            core::hint::spin_loop();
        }
    }
    Ok(())
}

/// 验证 timer 硬中断只发布 pending，完整工作只能由显式安全点消费。
fn kernel_timer_irq_is_deferred() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("timer deferred test ran on an AP");
    }

    // ktest kernel task 默认关中断运行。保存原状态后连续做两轮，第二轮同时证明
    // 第一轮安全点已经把 one-shot timer 精确地重新编程。
    let original_irq_state = crate::hal::local_irq_save();
    let tid = crate::task::current_tid();
    let result = deferred_timer_round(tid).and_then(|_| deferred_timer_round(tid));
    crate::hal::local_irq_restore(original_irq_state);
    result
}

/// 在“入口关中断、出口仍关中断”的约束下完成一轮真实 timer IRQ 测试。
fn deferred_timer_round(expected_tid: usize) -> Result<(), &'static str> {
    let cpu_id = crate::smp::cpu_id();
    let irq_before = crate::smp::timer_irq_count(cpu_id);
    let deferred_before = crate::smp::timer_deferred_count(cpu_id);

    // 只在受控窗口打开全局中断；硬 IRQ 返回后仍停在本函数中，不会自动经过
    // trap_return，因此可以直接检查 deferred work 尚未执行。
    crate::hal::local_irq_restore(true);
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while crate::smp::timer_irq_count(cpu_id) == irq_before {
        if crate::hal::get_time() >= deadline {
            let _ = crate::hal::local_irq_save();
            return Err("kernel timer interrupt did not arrive");
        }
        core::hint::spin_loop();
    }
    let was_enabled = crate::hal::local_irq_save();
    if !was_enabled {
        return Err("timer test lost its controlled interrupt window");
    }
    if crate::smp::timer_deferred_count(cpu_id) != deferred_before {
        return Err("timer hard IRQ executed deferred work");
    }
    if !crate::smp::local_timer_pending() {
        return Err("timer hard IRQ did not publish pending state");
    }
    if crate::task::current_tid() != expected_tid {
        return Err("timer hard IRQ switched the current task");
    }

    // 生产安全点可能因为 quantum 到期主动调度；恢复运行后必须仍是同一测试
    // 任务，且 pending 已被完整消费。
    crate::task::run_deferred_timer_at_task_safe_point();
    if crate::smp::local_timer_pending() {
        return Err("timer safe point left pending work behind");
    }
    if crate::smp::timer_deferred_count(cpu_id) != deferred_before.wrapping_add(1) {
        return Err("timer safe point did not complete exactly one batch");
    }
    if crate::task::current_tid() != expected_tid {
        return Err("timer safe point resumed a different task");
    }
    Ok(())
}

/// 反复验证 AP hard IRQ → idle deferred reply → CPU0 kernel trap 的完整闭环。
fn ap_to_bsp_ipi_round_trip() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("round-trip test ran on an AP");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    // CPU0 的 kernel task 默认关中断。请求先送到 AP，随后只在受控窗口打开
    // 本地全局中断接收 reply；每轮结束仍保持关中断。
    let original_irq_state = crate::hal::local_irq_save();
    let result = round_trip_all_aps();
    // 受控窗口内可能同时收到 timer hard IRQ；用 B11 的生产安全点收尾，
    // 避免把 quiesced one-shot 留给后续测试或 shutdown。
    crate::task::run_deferred_timer_at_task_safe_point();
    crate::hal::local_irq_restore(original_irq_state);
    result
}

fn round_trip_all_aps() -> Result<(), &'static str> {
    const ROUNDS_PER_AP: usize = 64;

    for cpu_id in 1..crate::smp::configured_cpu_count() {
        let failures_before = crate::smp::ipi_send_failures(cpu_id);
        for round in 0..ROUNDS_PER_AP {
            let expected = match crate::smp::send_ipi_round_trip(cpu_id) {
                Ok(expected) => expected,
                Err(error) => {
                    crate::println!(
                        "# SMP round-trip request failed: cpu={} round={} error={}",
                        cpu_id,
                        round,
                        error
                    );
                    return Err("failed to send round-trip request");
                }
            };

            crate::hal::local_irq_restore(true);
            let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
            while crate::smp::round_trip_reply_ack() != expected {
                if crate::hal::get_time() >= deadline {
                    let _ = crate::hal::local_irq_save();
                    crate::println!(
                        "# SMP round-trip timeout: cpu={} round={} expected={} observed={} send_failures={}",
                        cpu_id,
                        round,
                        expected,
                        crate::smp::round_trip_reply_ack(),
                        crate::smp::ipi_send_failures(cpu_id)
                    );
                    return Err("AP-to-BSP IPI reply timed out");
                }
                core::hint::spin_loop();
            }
            if !crate::hal::local_irq_save() {
                return Err("round-trip test lost its controlled interrupt window");
            }
        }

        if crate::smp::ipi_send_failures(cpu_id) != failures_before {
            return Err("AP failed to send a deferred IPI reply");
        }
    }
    Ok(())
}

/// 读取并原样恢复本 CPU 的全局中断状态。
fn local_interrupts_enabled() -> bool {
    let enabled = crate::hal::local_irq_save();
    crate::hal::local_irq_restore(enabled);
    enabled
}

/// 这个新任务由 idle scheduler 首次切入，因此可以直接观测 idle
/// 传给任务的硬件中断状态。检查后保持关中断并走正常 ktest exit。
fn probe_idle_to_task_irq_state() {
    let enabled = crate::hal::local_irq_save();
    IDLE_TO_TASK_IRQ_PROBE.store(
        if enabled {
            IRQ_PROBE_ENABLED
        } else {
            IRQ_PROBE_DISABLED
        },
        Ordering::Release,
    );
}

/// 验证 syscall 窗口跨 yield 切换后恢复，idle 不继承开中断状态。
fn syscall_irq_window_survives_schedule() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("syscall IRQ-window test ran on an AP");
    }

    IDLE_TO_TASK_IRQ_PROBE.store(IRQ_PROBE_NOT_RUN, Ordering::Release);
    let original_irq_state = crate::hal::local_irq_save();
    let result = crate::hal::with_local_interrupts_enabled(|| {
        if !local_interrupts_enabled() {
            return Err("controlled syscall window did not enable interrupts");
        }

        // helper 先入队，当前 runner 后入队；FIFO fast path 会先切入
        // helper，使它观测到的正是 runner -> idle -> helper 的状态。
        crate::task::spawn_ktest_task(probe_idle_to_task_irq_state);
        crate::task::suspend_current_and_run_next();

        match IDLE_TO_TASK_IRQ_PROBE.load(Ordering::Acquire) {
            IRQ_PROBE_DISABLED => {}
            IRQ_PROBE_ENABLED => return Err("idle scheduler leaked enabled IRQs into a new task"),
            _ => return Err("idle IRQ-state probe task did not run"),
        }
        if !local_interrupts_enabled() {
            return Err("resumed task did not recover its IRQ window");
        }

        // 窗口恢复后再接收一次真实 AP reply，证明不只是 CSR 位看起来
        // 开启，而是 kernel trap 确实能在该任务上下文中往返。
        receive_one_ap_reply_while_irqs_enabled()
    });

    // helper 正常返回后必须恢复入口快照。先关中断再消费窗口内
    // 可能发布的 timer pending，避免把 one-shot 状态泄漏给下一用例。
    let restored_irq_state = crate::hal::local_irq_save();
    crate::task::run_deferred_timer_at_task_safe_point();
    crate::hal::local_irq_restore(original_irq_state);

    result?;
    if restored_irq_state != original_irq_state {
        return Err("controlled syscall window did not restore entry IRQ state");
    }
    Ok(())
}

fn receive_one_ap_reply_while_irqs_enabled() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    let expected = crate::smp::send_ipi_round_trip(1)
        .map_err(|_| "failed to request AP reply inside syscall IRQ window")?;
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while crate::smp::round_trip_reply_ack() != expected {
        if crate::hal::get_time() >= deadline {
            return Err("AP reply did not interrupt the syscall IRQ window");
        }
        core::hint::spin_loop();
    }
    if !local_interrupts_enabled() {
        return Err("kernel IPI trap returned with syscall IRQ window closed");
    }
    Ok(())
}

/// helper 被选中后确认 Queued -> Running，并通过 Completion 唤醒 blocked runner。
fn complete_scheduler_state_probe() {
    let task = crate::task::current_task().expect("scheduler-state helper has no current task");
    if task.task_status() == crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        SCHED_STATE_HELPER_RUNS.fetch_add(1, Ordering::Release);
    }
    let completion = SCHED_STATE_COMPLETION
        .lock()
        .as_ref()
        .expect("scheduler-state completion missing")
        .clone();
    completion.complete();
}

/// 覆盖任务发布、提前取消阻塞、完整睡眠/唤醒和退出回收，并验证重复 wake
/// 不会改变队列 owner。测试只调用生产入口，不直接伪造原子状态。
fn scheduler_state_has_unique_owner() -> Result<(), &'static str> {
    let runner = crate::task::current_task().ok_or("scheduler-state runner is missing")?;
    if runner.task_status() != crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        return Err("runner does not own CPU0 before scheduler-state test");
    }

    // checked block 会先登记 Blocking，再复查条件。这里故意返回 false，验证
    // 早到 wake 只撤销阻塞意图，任务必须等切回 idle 后才能重新进入 runqueue。
    let mut saw_blocking = false;
    crate::task::block_current_and_run_next_checked(|task| {
        saw_blocking =
            task.task_status() == crate::task::TaskStatus::Blocking(crate::smp::BOOT_CPU_ID);
        false
    });
    if !saw_blocking {
        return Err("checked block did not expose Blocking ownership window");
    }
    if runner.task_status() != crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        return Err("cancelled block did not return runner to CPU0");
    }

    SCHED_STATE_HELPER_RUNS.store(0, Ordering::Release);
    let completion = Arc::new(crate::task::Completion::new());
    *SCHED_STATE_COMPLETION.lock() = Some(completion.clone());
    let cpu0_queued_before = crate::task::run_queue_count(crate::smp::BOOT_CPU_ID);
    let helper = crate::task::spawn_ktest_task(complete_scheduler_state_probe);
    if helper.task_status() != crate::task::TaskStatus::Queued(crate::smp::BOOT_CPU_ID) {
        return Err("new helper did not acquire the CPU0 ready queue");
    }
    if crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != cpu0_queued_before + 1 {
        return Err("CPU0 runqueue did not gain exactly one helper");
    }
    for cpu in 1..crate::smp::configured_cpu_count() {
        if crate::task::run_queue_count(cpu) != 0 {
            return Err("parked AP unexpectedly owns a runnable task");
        }
    }

    completion.wait_uninterruptible();
    *SCHED_STATE_COMPLETION.lock() = None;
    if SCHED_STATE_HELPER_RUNS.load(Ordering::Acquire) != 1 {
        return Err("scheduler-state helper did not run exactly once");
    }
    if helper.task_status() != crate::task::TaskStatus::Zombie {
        return Err("scheduler-state helper did not reach Zombie");
    }
    if runner.task_status() != crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        return Err("woken runner did not reacquire CPU0 ownership");
    }
    if crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != cpu0_queued_before {
        return Err("helper lifecycle changed the baseline CPU0 runqueue length");
    }

    // 对已经 Running 的 runner 再发两次 wake；统一入口必须把它们识别为
    // 已唤醒，且不能改变 ready/interruptible 容器。
    let counts_before = crate::task::task_manager_counts();
    crate::task::wake_interruptible(runner.clone());
    crate::task::wake_interruptible(runner.clone());
    if crate::task::task_manager_counts() != counts_before {
        return Err("duplicate wake changed scheduler queue membership");
    }
    if runner.task_status() != crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        return Err("duplicate wake changed the running owner");
    }
    Ok(())
}

/// 在所有可重复测试结束后永久停止 AP，并验证每个目标都发布了 ack。
fn secondary_cpus_stop_and_ack() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("STOP test ran on an AP");
    }

    let targets = crate::smp::online_cpu_mask() & !(1usize << crate::smp::BOOT_CPU_ID);
    if let Err(error) = crate::smp::stop_secondary_cpus() {
        crate::println!(
            "# SMP STOP failed: targets={:#x} error={:?}",
            targets,
            error
        );
        return Err("secondary CPUs did not stop");
    }

    let stopped = crate::smp::stopped_cpu_mask();
    if stopped & targets != targets {
        crate::println!(
            "# SMP STOP ack mismatch: targets={:#x} stopped={:#x}",
            targets,
            stopped
        );
        return Err("STOP returned before every AP acknowledged");
    }

    // 验证生产 shutdown 再次调用同一协议时走幂等快路径。
    crate::smp::stop_secondary_cpus().map_err(|_| "repeated STOP was not idempotent")
}

/// AP 只有在切换到独立 idle stack 后才允许发布 online。
fn secondary_cpus_enter_idle_context() -> Result<(), &'static str> {
    let configured = crate::smp::configured_cpu_count();
    let expected = ((1usize << configured) - 1) & !(1usize << crate::smp::BOOT_CPU_ID);
    let idle = crate::smp::idle_cpu_mask();

    if idle != expected {
        crate::println!(
            "# SMP idle mismatch: configured={} expected={:#x} idle={:#x}",
            configured,
            expected,
            idle
        );
        return Err("secondary CPU did not enter its idle context");
    }
    Ok(())
}

/// 测试 runner 本身仍固定 CPU0，避免 focused test 意外进入用户迁移路径。
fn ktest_runner_stays_on_boot_cpu() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("SMP ktest runner executed on an AP");
    }
    Ok(())
}

/// BSP 返回 scheduler-ready 发布函数前，所有配置 CPU 都必须进入调度循环。
fn configured_cpus_enter_scheduler() -> Result<(), &'static str> {
    let expected = (1usize << crate::smp::configured_cpu_count()) - 1;
    let entered = crate::smp::scheduler_cpu_mask();
    if entered != expected {
        crate::println!(
            "# SMP scheduler mask mismatch: expected={:#x} entered={:#x}",
            expected,
            entered
        );
        return Err("configured CPU set did not enter per-CPU schedulers");
    }
    Ok(())
}

fn record_remote_kernel_task_cpu() {
    let cpu = crate::smp::cpu_id();
    let status_ok = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(cpu))
        .unwrap_or(false);
    if cpu == crate::smp::BOOT_CPU_ID || !status_ok {
        AP_TASK_ERRORS.fetch_or(1usize << cpu, Ordering::Release);
    }
    AP_TASK_RUNS[cpu].fetch_add(1, Ordering::Release);
}

/// 向每个 AP 的真实 runqueue 发布一个 kernel-only 任务，并验证 target/current 唯一归属。
fn remote_kernel_tasks_run_on_target_cpus() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("remote enqueue test did not run on CPU0");
    }
    AP_TASK_ERRORS.store(0, Ordering::Release);
    for runs in &AP_TASK_RUNS {
        runs.store(0, Ordering::Release);
    }

    let mut tasks = Vec::new();
    for cpu in 1..crate::smp::configured_cpu_count() {
        tasks.push((
            cpu,
            crate::task::spawn_ktest_task_on(cpu, record_remote_kernel_task_cpu),
        ));
    }

    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    loop {
        let finished = tasks.iter().all(|(cpu, task)| {
            AP_TASK_RUNS[*cpu].load(Ordering::Acquire) == 1
                && task.task_status() == crate::task::TaskStatus::Zombie
                && !crate::task::processor::cpu_has_current(*cpu)
        });
        if finished {
            break;
        }
        if crate::hal::get_time() >= deadline {
            return Err("remote kernel task did not finish before timeout");
        }
        core::hint::spin_loop();
    }

    if AP_TASK_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("remote kernel task observed wrong CPU/current owner");
    }
    for cpu in 1..crate::smp::configured_cpu_count() {
        if AP_TASK_RUNS[cpu].load(Ordering::Acquire) != 1 {
            return Err("remote kernel task ran more or less than once");
        }
        if crate::task::run_queue_count(cpu) != 0 {
            return Err("AP runqueue retained a completed kernel task");
        }
    }
    Ok(())
}

/// AP 任务走真实 Completion/WaitQueue 阻塞路径；恢复后必须仍由原 CPU 唯一拥有。
fn wait_for_remote_completion() {
    let origin = crate::smp::cpu_id();
    let completion = AP_BLOCKED_WAKE_COMPLETION
        .lock()
        .as_ref()
        .expect("AP blocked-wake completion missing")
        .clone();
    AP_BLOCKED_WAKE_PHASE[origin].store(1, Ordering::Release);
    completion.wait_uninterruptible();

    let resumed = crate::smp::cpu_id();
    let owner_is_origin = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(origin))
        .unwrap_or(false);
    if resumed != origin || !owner_is_origin {
        AP_BLOCKED_WAKE_ERRORS.fetch_or(1usize << origin, Ordering::Release);
    }
    AP_BLOCKED_WAKE_PHASE[origin].store(2, Ordering::Release);
}

/// 一次 `Completion::complete()` 批量唤醒所有 AP，覆盖生产 batch wake、
/// `Blocked -> Queued(last_cpu)` 和释放调度锁后广播 RESCHEDULE 的完整链路。
fn blocked_kernel_tasks_wake_on_last_cpu() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("blocked-wake test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    AP_BLOCKED_WAKE_ERRORS.store(0, Ordering::Release);
    for phase in &AP_BLOCKED_WAKE_PHASE {
        phase.store(0, Ordering::Release);
    }
    let completion = Arc::new(crate::task::Completion::new());
    *AP_BLOCKED_WAKE_COMPLETION.lock() = Some(completion.clone());

    let mut tasks = Vec::new();
    for cpu in 1..crate::smp::configured_cpu_count() {
        tasks.push((
            cpu,
            crate::task::spawn_ktest_task_on(cpu, wait_for_remote_completion),
        ));
    }

    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while !tasks.iter().all(|(cpu, task)| {
        AP_BLOCKED_WAKE_PHASE[*cpu].load(Ordering::Acquire) == 1
            && task.task_status() == crate::task::TaskStatus::Blocked
            && !crate::task::processor::cpu_has_current(*cpu)
            && crate::task::run_queue_count(*cpu) == 0
    }) {
        if crate::hal::get_time() >= deadline {
            return Err("AP tasks did not fully leave their CPUs before wake");
        }
        core::hint::spin_loop();
    }

    if !completion.complete() {
        return Err("first completion did not publish wakeup");
    }
    let wake_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while !tasks.iter().all(|(cpu, task)| {
        AP_BLOCKED_WAKE_PHASE[*cpu].load(Ordering::Acquire) == 2
            && task.task_status() == crate::task::TaskStatus::Zombie
            && !crate::task::processor::cpu_has_current(*cpu)
    }) {
        if crate::hal::get_time() >= wake_deadline {
            return Err("remotely woken AP tasks did not finish before timeout");
        }
        core::hint::spin_loop();
    }

    if AP_BLOCKED_WAKE_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("blocked AP task resumed on the wrong CPU or owner");
    }
    if completion.complete() {
        return Err("duplicate completion attempted a second wakeup");
    }
    *AP_BLOCKED_WAKE_COMPLETION.lock() = None;
    Ok(())
}

/// 直接调用生产 user-TLB 同步原语，验证独立 sequence、IPI handler 与 ack 闭环。
///
/// 本用例尚未让用户任务迁移，也不伪装 stale-PTE 证明；它只验收 B22 已完成的
/// 基础设施。真正的 generation race 与 ack 前 frame 生命周期留给锁外 batch 节点。
fn user_tlb_full_flush_reaches_online_cpus() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("user TLB sync test did not run on CPU0");
    }
    let targets = crate::smp::online_cpu_mask() & !crate::smp::stopped_cpu_mask();
    let mut ack_before = [0usize; crate::smp::MAX_CPUS];
    for cpu in 1..crate::smp::configured_cpu_count() {
        ack_before[cpu] = crate::smp::user_tlb_ack(cpu);
    }

    crate::smp::synchronize_user_tlb(targets, None).map_err(|error| {
        crate::println!("# user TLB full-flush sync failed: {:?}", error);
        "user TLB full-flush sync failed"
    })?;
    for cpu in 1..crate::smp::configured_cpu_count() {
        if crate::smp::user_tlb_ack(cpu) <= ack_before[cpu] {
            return Err("an online AP did not acknowledge the user TLB flush");
        }
    }

    // 同步等待临时开放过本地 IRQ；ktest 不经过用户 trap-return，因此显式走
    // 已有任务安全点，避免把恰好到达的 one-shot timer pending 留给下一用例。
    crate::task::run_deferred_timer_at_task_safe_point();
    Ok(())
}

/// 页级同步在 RV64 应由 RFENCE 直接完成；LA64 则使用既有全量 IPI fallback。
fn user_tlb_page_sync_uses_arch_backend() -> Result<(), &'static str> {
    let mut targets = 1usize << crate::smp::BOOT_CPU_ID;
    if crate::smp::configured_cpu_count() > 1 {
        // 只选择逻辑 CPU0/1；当 cold-boot hart 非 0 时，物理 mask 不再碰巧等于
        // 逻辑 mask，从而让 focused 运行真正经过逆映射分支。
        targets |= 1usize << 1;
    }
    targets &= crate::smp::online_cpu_mask() & !crate::smp::stopped_cpu_mask();
    let request_before = if crate::smp::configured_cpu_count() > 1 {
        crate::smp::user_tlb_request(1)
    } else {
        0
    };

    crate::smp::synchronize_user_tlb(targets, Some(crate::mm::VirtAddr::from(0x51_0000).floor()))
        .map_err(|error| {
        crate::println!("# user TLB page sync failed: {:?}", error);
        "user TLB page sync failed"
    })?;

    #[cfg(feature = "riscv")]
    if crate::smp::configured_cpu_count() > 1 && crate::smp::user_tlb_request(1) != request_before {
        return Err("RV64 page sync unexpectedly used the IPI fallback");
    }
    #[cfg(feature = "loongarch64")]
    if crate::smp::configured_cpu_count() > 1 && crate::smp::user_tlb_request(1) <= request_before {
        return Err("LA64 page sync did not use the IPI fallback");
    }
    crate::task::run_deferred_timer_at_task_safe_point();
    Ok(())
}

fn observe_user_tlb_retirement_window() {
    let cpu = crate::smp::cpu_id();
    if cpu != 1 {
        AP_USER_TLB_RETIRE_PHASE.store(usize::MAX, Ordering::Release);
        return;
    }
    let vm = USER_TLB_RETIRE_VM
        .lock()
        .as_ref()
        .expect("user TLB retirement VM missing")
        .clone();
    vm.activate_on(cpu);
    let request_before = crate::smp::user_tlb_request(cpu);
    AP_USER_TLB_REQUEST_BEFORE.store(request_before, Ordering::Release);
    AP_USER_TLB_RETIRE_PHASE.store(1, Ordering::Release);

    // ktest kernel task 默认关中断运行：request 增加后 handler 尚不可能 ack，
    // 因而这里正好位于“PTE 已清除、远端 flush 未完成”的窗口。
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while crate::smp::user_tlb_request(cpu) == request_before {
        if crate::hal::get_time() >= deadline {
            AP_USER_TLB_RETIRE_PHASE.store(usize::MAX, Ordering::Release);
            return;
        }
        core::hint::spin_loop();
    }
    AP_USER_TLB_FREE_DURING_WAIT.store(crate::mm::unallocated_frames(), Ordering::Release);
    AP_USER_TLB_RETIRE_PHASE.store(2, Ordering::Release);

    crate::hal::with_local_interrupts_enabled(|| {
        while AP_USER_TLB_RETIRE_PHASE.load(Ordering::Acquire) != 3 {
            core::hint::spin_loop();
        }
    });
    AP_USER_TLB_RETIRE_PHASE.store(4, Ordering::Release);
}

/// 用真实共享地址空间撤映射证明：request 已发布但 AP 尚未 ack 时，
/// 数据 frame 仍未回到分配器；`write()` 返回后它才完成退休。
fn user_tlb_retirement_waits_for_ack() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    const TEST_BASE: usize = 0x52_0000;

    AP_USER_TLB_RETIRE_PHASE.store(0, Ordering::Release);
    AP_USER_TLB_FREE_DURING_WAIT.store(usize::MAX, Ordering::Release);
    let mut space = crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare();
    // 两个不同 VPN 会让 MmuGather 升级为 Full，确保本用例仍专门验证软件
    // IPI 的可观测 ack 窗口；单页 RFENCE 由前一用例独立覆盖。
    space.insert_framed_area(
        crate::mm::VirtAddr::from(TEST_BASE),
        crate::mm::VirtAddr::from(TEST_BASE + 2 * crate::config::PAGE_SIZE),
        crate::mm::MapPermission::R | crate::mm::MapPermission::W | crate::mm::MapPermission::U,
    );
    let vm = Arc::new(crate::mm::AddressSpace::new(space));
    vm.activate_on(crate::smp::BOOT_CPU_ID);
    *USER_TLB_RETIRE_VM.lock() = Some(vm.clone());
    let task = crate::task::spawn_ktest_task_on(1, observe_user_tlb_retirement_window);

    let ready_deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while AP_USER_TLB_RETIRE_PHASE.load(Ordering::Acquire) != 1 {
        if AP_USER_TLB_RETIRE_PHASE.load(Ordering::Acquire) == usize::MAX
            || crate::hal::get_time() >= ready_deadline
        {
            return Err("AP did not enter user TLB retirement window");
        }
        core::hint::spin_loop();
    }

    let free_before = crate::mm::unallocated_frames();
    let ack_before = crate::smp::user_tlb_ack(1);
    vm.write(|space| {
        space
            .remove_area_with_start_vpn(crate::mm::VirtAddr::from(TEST_BASE).floor())
            .expect("user TLB retirement test unmap failed");
    });

    let free_during = AP_USER_TLB_FREE_DURING_WAIT.load(Ordering::Acquire);
    let free_after = crate::mm::unallocated_frames();
    let validation_error = if free_during != free_before {
        Some("user frame was released before remote TLB ack")
    } else if free_after != free_before.saturating_add(2) {
        Some("user frames were not released after remote TLB ack")
    } else if crate::smp::user_tlb_ack(1) <= ack_before
        || crate::smp::user_tlb_request(1) <= AP_USER_TLB_REQUEST_BEFORE.load(Ordering::Acquire)
    {
        Some("user TLB retirement did not complete a new request/ack")
    } else {
        None
    };

    AP_USER_TLB_RETIRE_PHASE.store(3, Ordering::Release);
    let done_deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while task.task_status() != crate::task::TaskStatus::Zombie
        || AP_USER_TLB_RETIRE_PHASE.load(Ordering::Acquire) != 4
    {
        if crate::hal::get_time() >= done_deadline {
            return Err("AP retirement observer did not finish");
        }
        core::hint::spin_loop();
    }
    *USER_TLB_RETIRE_VM.lock() = None;
    crate::task::run_deferred_timer_at_task_safe_point();
    validation_error.map_or(Ok(()), Err)
}

fn record_kstack_reclaim_task() {
    let cpu = crate::smp::cpu_id();
    let owner_ok = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(cpu))
        .unwrap_or(false);
    if cpu == crate::smp::BOOT_CPU_ID || !owner_ok {
        AP_KSTACK_RECLAIM_ERRORS.fetch_add(1, Ordering::Release);
    }
    AP_KSTACK_RECLAIM_RUNS.fetch_add(1, Ordering::Release);
}

/// 在同一 AP 上执行一轮超过内核栈缓存容量的任务；等 AP current 已清空后，
/// 由仍在 CPU0 运行的测试任务显式析构这些“其它任务”的 zombie TCB。
fn run_kstack_reclaim_wave() -> Result<(), &'static str> {
    let task_count = crate::hal::KERNEL_STACK_CACHE_LIMIT + 1;
    AP_KSTACK_RECLAIM_RUNS.store(0, Ordering::Release);
    AP_KSTACK_RECLAIM_ERRORS.store(0, Ordering::Release);

    let mut tasks = Vec::with_capacity(task_count);
    for _ in 0..task_count {
        tasks.push(crate::task::spawn_ktest_task_on(
            1,
            record_kstack_reclaim_task,
        ));
    }

    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(5));
    while AP_KSTACK_RECLAIM_RUNS.load(Ordering::Acquire) != task_count
        || tasks
            .iter()
            .any(|task| task.task_status() != crate::task::TaskStatus::Zombie)
        || crate::task::processor::cpu_has_current(1)
        || crate::task::run_queue_count(1) != 0
        || crate::task::zombie_queue_count_fast() < task_count
    {
        if crate::hal::get_time() >= deadline {
            return Err("AP kernel-stack reclaim wave did not quiesce");
        }
        core::hint::spin_loop();
    }
    if AP_KSTACK_RECLAIM_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("kernel-stack reclaim task observed wrong CPU owner");
    }

    let weak_tasks: Vec<_> = tasks.iter().map(Arc::downgrade).collect();
    drop(tasks);
    let zombies = crate::task::take_zombie_tasks(task_count);
    if zombies.len() != task_count {
        return Err("zombie queue did not transfer the complete reclaim wave");
    }
    drop(zombies);
    if crate::hal::reclaim_retired_kernel_stacks(usize::MAX) == 0 {
        return Err("kernel-stack cache overflow did not queue a retirement");
    }
    if weak_tasks.iter().any(|task| task.upgrade().is_some()) {
        return Err("reclaimed kernel task still has a strong TCB owner");
    }
    Ok(())
}

/// 第一轮强制让缓存溢出并撤销至少一个 AP 使用过的 stack mapping；第二轮
/// 随即耗尽缓存并重新映射回收 slot，验证 shootdown 后的真实复用闭环。
fn kernel_stack_reclaim_waits_for_shootdown() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("kernel-stack reclaim test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    let stale = crate::task::take_zombie_tasks(usize::MAX);
    drop(stale);
    if crate::task::zombie_queue_count_fast() != 0 {
        return Err("zombie queue was not empty before kernel-stack reclaim test");
    }

    let mut ack_before = [0usize; crate::smp::MAX_CPUS];
    for cpu in 1..crate::smp::configured_cpu_count() {
        ack_before[cpu] = crate::smp::kernel_tlb_ack(cpu);
    }
    run_kstack_reclaim_wave()?;
    for cpu in 1..crate::smp::configured_cpu_count() {
        if crate::smp::kernel_tlb_ack(cpu) <= ack_before[cpu] {
            return Err("kernel-stack retirement missed an online AP shootdown");
        }
    }
    run_kstack_reclaim_wave()?;
    // ktest runner 不会像 syscall 一样返回 trap-return；上面的 shootdown 等待
    // 会临时开中断，若期间接住 one-shot timer，必须在离开用例前通过生产
    // 安全点消费 pending 并重新编程，否则下一轮 timer 用例会继承静默状态。
    crate::task::run_deferred_timer_at_task_safe_point();
    Ok(())
}
