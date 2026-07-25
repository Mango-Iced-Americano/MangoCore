//! SMP 启动阶段的 focused ktest。

use alloc::{vec, vec::Vec};

use crate::kernel_tests::runner::KernelTest;

/// 返回只依赖 Phase 1 启动不变量的测试集合。
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "smp::configured_cpus_are_online",
            configured_cpus_are_online,
        ),
        KernelTest::new(
            "smp::legacy_scheduler_stays_on_boot_cpu",
            legacy_scheduler_stays_on_boot_cpu,
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

/// Phase 1 只允许 CPU0 进入旧调度器，避免过早暴露未审计的共享状态。
fn legacy_scheduler_stays_on_boot_cpu() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("legacy scheduler executed the SMP ktest on an AP");
    }
    Ok(())
}
