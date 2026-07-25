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
