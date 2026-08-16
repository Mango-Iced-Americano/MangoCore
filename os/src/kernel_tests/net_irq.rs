use alloc::{sync::Arc, vec, vec::Vec};

use crate::{
    kernel_tests::{
        probe::{
            attach_probe_to_runner, build_udp_recvfrom_probe, deadline_after, reap_probe,
            stop_probe,
        },
        runner::KernelTest,
    },
    net::config::NET_INTERFACE,
    task::{TaskControlBlock, TaskStatus},
};

const NEGATIVE_CONTROL_SECS: usize = 1;
const RECEIVE_TIMEOUT_SECS: usize = 5;

pub fn tests() -> Vec<KernelTest> {
    vec![KernelTest::with_timeout(
        "net_irq::recvfrom_requires_external_irq_without_tick_fallback",
        recvfrom_requires_external_irq_without_tick_fallback,
        12_000,
    )]
}

fn wait_until_blocked(task: &Arc<TaskControlBlock>, deadline: usize) -> bool {
    crate::hal::with_local_interrupts_enabled(|| loop {
        if matches!(task.task_status(), TaskStatus::Blocked) {
            return true;
        }
        if crate::hal::get_time() >= deadline {
            return false;
        }
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    })
}

fn wait_until_boot_poll_is_quiescent(deadline: usize) -> bool {
    crate::hal::with_local_interrupts_enabled(|| loop {
        if !NET_INTERFACE.poll_request_pending() {
            return true;
        }
        if crate::hal::get_time() >= deadline {
            return false;
        }
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    })
}

fn wait_for_probe_exit(task: &Arc<TaskControlBlock>, deadline: usize) -> bool {
    crate::hal::with_local_interrupts_enabled(|| loop {
        if task.process.is_zombie() && matches!(task.task_status(), TaskStatus::Zombie) {
            return true;
        }
        if crate::hal::get_time() >= deadline {
            return false;
        }
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    })
}

fn recvfrom_remains_blocked_without_seie() -> Result<(), &'static str> {
    let probe = build_udp_recvfrom_probe()?;
    let parent = attach_probe_to_runner(&probe)?;
    crate::task::publish_task_on(probe.clone(), crate::smp::BOOT_CPU_ID);

    let blocked = wait_until_blocked(&probe, deadline_after(NEGATIVE_CONTROL_SECS));
    let stayed_blocked = crate::hal::with_local_interrupts_enabled(|| {
        let deadline = deadline_after(NEGATIVE_CONTROL_SECS);
        while crate::hal::get_time() < deadline {
            if matches!(probe.task_status(), TaskStatus::Zombie) {
                return false;
            }
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
        matches!(probe.task_status(), TaskStatus::Blocked)
    });
    crate::hal::arch::riscv::trap::enable_external_interrupt();
    let resumed = wait_for_probe_exit(&probe, deadline_after(RECEIVE_TIMEOUT_SECS));
    let reaped = reap_probe(&parent, &probe);
    crate::println!(
        "[net_irq] SEIE control: blocked={}, stayed_blocked={}, resumed={}, reaped={}, irq_count={}",
        blocked,
        stayed_blocked,
        resumed,
        reaped,
        crate::drivers::net::virtio_net::virtio_net_irq_count()
    );
    if !blocked {
        return Err("recvfrom did not enter Blocked while SEIE was disabled");
    }
    if !stayed_blocked {
        return Err("recvfrom completed while SEIE and tick fallback were disabled");
    }
    if !resumed || !reaped {
        return Err("SEIE negative-control recvfrom probe did not resume and reap after re-enable");
    }
    Ok(())
}

fn recvfrom_requires_external_irq_without_tick_fallback() -> Result<(), &'static str> {
    if crate::hal::arch::riscv::plic::boot_cpu_context().is_none()
        || !crate::drivers::net::virtio_net::virtio_net_irq_available()
    {
        return Err("SKIP: requires an initialized boot-CPU PLIC and virtio-net device");
    }
    if !wait_until_boot_poll_is_quiescent(deadline_after(NEGATIVE_CONTROL_SECS)) {
        return Err("boot network poll did not quiesce before the IRQ negative control");
    }

    let previous_fallback = NET_INTERFACE.set_scheduler_tick_net_fallback_enabled_for_test(false);
    crate::hal::arch::riscv::trap::disable_external_interrupt();
    let negative_control = recvfrom_remains_blocked_without_seie();
    crate::hal::arch::riscv::trap::enable_external_interrupt();
    if negative_control.is_err() {
        NET_INTERFACE.set_scheduler_tick_net_fallback_enabled_for_test(previous_fallback);
        return negative_control;
    }

    let result = (|| {
        let irq_before = crate::drivers::net::virtio_net::virtio_net_irq_count();
        let probe = build_udp_recvfrom_probe()?;
        let parent = attach_probe_to_runner(&probe)?;
        crate::task::publish_task_on(probe.clone(), crate::smp::BOOT_CPU_ID);
        let received = wait_for_probe_exit(&probe, deadline_after(RECEIVE_TIMEOUT_SECS));
        let cleaned = received || stop_probe(&probe, &probe.process, crate::smp::BOOT_CPU_ID);
        let exit_status = probe.process.exit_code();
        let reaped = reap_probe(&parent, &probe);
        let irq_after = crate::drivers::net::virtio_net::virtio_net_irq_count();
        if !received || !cleaned || !reaped || exit_status != 0 {
            return Err("blocking recvfrom did not receive the host UDP datagram");
        }
        if irq_after <= irq_before {
            return Err("virtio-net receive completed without a new hardware IRQ");
        }
        crate::println!(
            "[net_irq] recvfrom woke via virtio IRQ: before={}, after={}",
            irq_before,
            irq_after
        );
        Ok(())
    })();
    NET_INTERFACE.set_scheduler_tick_net_fallback_enabled_for_test(previous_fallback);
    result
}
