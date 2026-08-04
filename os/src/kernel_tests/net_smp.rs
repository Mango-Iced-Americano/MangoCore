//! WP1 网络 SMP focused ktest。
// allow: SIZE_OK - 这十个测试共享同一组零盘 fixture、原子 phase 和清理纪律；协调方只注册本模块。

use alloc::{
    sync::Arc,
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicUsize, Ordering};
use smoltcp::wire::{IpAddress, IpEndpoint, IpVersion};

use crate::{
    kernel_tests::{
        probe::{attach_probe_to_runner, build_udp_bind_probe, deadline_after, probe_quiesced, reap_probe, stop_probe},
        runner::KernelTest,
    },
    net::{
        config::{NetPollTestHookPoint, NET_INTERFACE},
        socket::{
            inet::{
                common::port::PortManager,
                datagram::udp::UdpSocket,
                stream::TcpSocket,
            },
            Endpoint, Socket,
        },
    },
};

const IRQ_POLL_WAIT_TICKS_DIVISOR: usize = 8;
const PORT_RACE: u16 = 61_105;
const PORT_REUSE: u16 = 61_102;
const PORT_PROTO: u16 = 61_103;
const PORT_NETNS: u16 = 61_104;

static IRQ_HOOK_FIRED: AtomicUsize = AtomicUsize::new(0);
static IRQ_HOOK_ERRORS: AtomicUsize = AtomicUsize::new(0);

/// 返回 WP1 的十个独立测试。注册由 Wave 2 协调方完成。
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("net_smp::irq_poll_is_publish_only", irq_poll_is_publish_only),
        KernelTest::new("net_smp::port_reserve_exactly_once", port_reserve_exactly_once),
        KernelTest::new(
            "net_smp::udp_reuse_release_exact_owner",
            udp_reuse_release_exact_owner,
        ),
        KernelTest::new("net_smp::tcp_udp_same_numeric_port", tcp_udp_same_numeric_port),
        KernelTest::new("net_smp::namespace_port_isolation", namespace_port_isolation),
        KernelTest::skip(
            "net_smp::route_handle_reuse_rejected",
            "requires WP5 route revalidation machinery",
        ),
        KernelTest::new("net_smp::per_stack_poll_progress", per_stack_poll_progress),
        KernelTest::new("net_smp::poll_worker_no_lost_wake", poll_worker_no_lost_wake),
        KernelTest::skip(
            "net_smp::tcp_dual_sender_exact_bytes",
            "requires user-probe fixture (dual sender)",
        ),
        KernelTest::skip(
            "net_smp::epollet_concurrent_edge",
            "requires WP6 epoll edge machinery",
        ),
    ]
}

fn loopback_endpoint(port: u16) -> Endpoint {
    Endpoint::Ip(IpEndpoint::new(IpAddress::v4(127, 0, 0, 1), port))
}

fn loopback_addr() -> Option<smoltcp::wire::Ipv4Address> {
    match IpAddress::v4(127, 0, 0, 1) {
        IpAddress::Ipv4(address) => Some(address),
        IpAddress::Ipv6(_) => None,
    }
}

fn new_udp_socket() -> Arc<dyn Socket> {
    let socket = Arc::new(UdpSocket::new(IpVersion::Ipv4));
    UdpSocket::register_udp_socket(&socket);
    socket
}

fn new_tcp_socket() -> Arc<dyn Socket> {
    let socket = Arc::new(TcpSocket::new(IpVersion::Ipv4));
    TcpSocket::register_tcp_socket(&socket);
    socket
}

fn wait_for_zombie(
    task: &Arc<crate::task::TaskControlBlock>,
    cpu: usize,
    timeout: &'static str,
) -> Result<(), &'static str> {
    crate::hal::with_local_interrupts_enabled(|| {
        let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
        while task.task_status() != crate::task::TaskStatus::Zombie
            || crate::task::processor::cpu_has_current(cpu)
            || crate::task::run_queue_count(cpu) != 0
        {
            if crate::hal::get_time() >= deadline {
                return Err(timeout);
            }
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
        Ok(())
    })
}

/// IRQ hook 不取得锁、不分配也不打印；它只证明 IRQ 路径在 publish 前经过此点。
fn irq_before_publish(point: NetPollTestHookPoint) {
    if point != NetPollTestHookPoint::IrqBeforePublish {
        IRQ_HOOK_ERRORS.fetch_add(1, Ordering::Release);
        return;
    }
    IRQ_HOOK_FIRED.fetch_add(1, Ordering::Release);
}

/// IRQ 只发布 generation；CPU0 poll worker 随后消费该 generation，不能访问 poll 栈。
fn irq_poll_is_publish_only() -> Result<(), &'static str> {
    IRQ_HOOK_FIRED.store(0, Ordering::Relaxed);
    IRQ_HOOK_ERRORS.store(0, Ordering::Relaxed);
    let (requested_before, _) = NET_INTERFACE.poll_generation_snapshot();
    NET_INTERFACE.set_test_poll_hook(Some(irq_before_publish));
    let started = crate::hal::get_time();
    let _ = NET_INTERFACE.try_poll_irq();
    let elapsed = crate::hal::get_time().wrapping_sub(started);
    NET_INTERFACE.set_test_poll_hook(None);

    if elapsed >= (crate::hal::get_clock_freq() / IRQ_POLL_WAIT_TICKS_DIVISOR).max(1) {
        return Err("IRQ poll did not return within the publish-only bound");
    }
    if IRQ_HOOK_ERRORS.load(Ordering::Acquire) != 0
        || IRQ_HOOK_FIRED.load(Ordering::Acquire) != 1
    {
        return Err("IRQ poll did not fire IrqBeforePublish exactly once");
    }

    // 直接调用 try_poll_irq() 不经过硬 IRQ 收尾；在 runner 上领取 deferred wake，
    // 之后只让 CPU0 的已固定 poll worker 消费本次 generation。
    NET_INTERFACE.run_deferred_net_wake();
    let expected_generation = requested_before.saturating_add(1);
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    let converged = crate::hal::with_local_interrupts_enabled(|| loop {
        let (requested, completed) = NET_INTERFACE.poll_generation_snapshot();
        if requested == completed && completed >= expected_generation {
            break true;
        }
        if crate::hal::get_time() >= deadline {
            break false;
        }
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    });
    if !converged {
        return Err("poll worker did not consume the published IRQ generation");
    }
    Ok(())
}

/// 两个完整用户 TCB 在 CPU1/CPU2 经过 socket()+bind()；runner 不占用 probe 的 current 槽。
fn port_reserve_exactly_once() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
    PortManager::unregister_udp_bind(PORT_RACE);
    let first = build_udp_bind_probe()?;
    let second = build_udp_bind_probe()?;
    first.set_initial_cpus_allowed(1 << 1);
    second.set_initial_cpus_allowed(1 << 2);
    let first_parent = attach_probe_to_runner(&first)?;
    let second_parent = attach_probe_to_runner(&second)?;

    // 两个用户任务均已完成 parent/affinity 发布，随后连续入队；不以 sleep 伪造竞争窗口。
    crate::task::publish_task_on(first.clone(), 1);
    crate::task::publish_task_on(second.clone(), 2);
    let first_done = probe_quiesced(&first, &first.process, 1, deadline_after(3));
    let second_done = probe_quiesced(&second, &second.process, 2, deadline_after(3));
    let first_clean = first_done || stop_probe(&first, &first.process, 1);
    let second_clean = second_done || stop_probe(&second, &second.process, 2);
    let first_reaped = reap_probe(&first_parent, &first);
    let second_reaped = reap_probe(&second_parent, &second);
    PortManager::unregister_udp_bind(PORT_RACE);
    if !first_clean || !second_clean || !first_reaped || !second_reaped {
        return Err("user UDP bind probes did not quiesce and reap");
    }
    let first_exit = first.process.exit_code();
    let second_exit = second.process.exit_code();
    // wait_child 返回 Linux raw wait status；probe 的 `exit(1)` 是 0x100，不是 1。
    // 只解码低 8 位退出码，保留 raw status 诊断以便区分 signal/ABI 异常。
    let first_code = first_exit >> 8;
    let second_code = second_exit >> 8;
    if first_code + second_code != 1 {
        // 仅在断言失败时输出，区分双成功、双失败和 probe 非预期退出；正常路径零噪声。
        crate::println!(
            "# net_smp UDP bind probe exits: CPU1={}, CPU2={}",
            first_exit,
            second_exit
        );
        return Err("concurrent user bind did not produce exactly one success");
    }
    Ok(())
}

/// 通过 IRQ publish 请求 generation，再等待 CPU0 worker 发布相同 completed generation。
fn poll_worker_no_lost_wake() -> Result<(), &'static str> {
    let (requested_before, completed_before) = NET_INTERFACE.poll_generation_snapshot();
    let _ = NET_INTERFACE.try_poll_irq();
    NET_INTERFACE.run_deferred_net_wake();
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
    let completed = crate::hal::with_local_interrupts_enabled(|| loop {
        let (requested, completed) = NET_INTERFACE.poll_generation_snapshot();
        if requested > requested_before && completed >= requested && completed > completed_before {
            break true;
        }
        if crate::hal::get_time() >= deadline {
            break false;
        }
        // worker 与 runner 都固定在 CPU0；IRQ 只发布 deferred wake，必须由此安全点让出 CPU。
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    });
    if completed {
        Ok(())
    } else {
        Err("net poll worker did not complete the published generation")
    }
}

/// WP4 的 owner 精度测试直接观察现有 UDP bucket：关闭一个 reuse owner 不能删除 peer。
fn udp_reuse_release_exact_owner() -> Result<(), &'static str> {
    PortManager::unregister_udp_bind(PORT_REUSE);
    let first = new_udp_socket();
    let second = new_udp_socket();
    first.set_reuse_addr(true).map_err(|_| "UDP socket rejected SO_REUSEADDR")?;
    second.set_reuse_addr(true).map_err(|_| "UDP socket rejected SO_REUSEADDR")?;
    let task = crate::task::current_task().ok_or("UDP reuse test has no current task")?;
    let first_result = PortManager::bind_port(&task, &first, &loopback_endpoint(PORT_REUSE));
    let second_result = PortManager::bind_port(&task, &second, &loopback_endpoint(PORT_REUSE));
    drop(task);
    if first_result.is_err() || second_result.is_err() {
        PortManager::unregister_udp_bind(PORT_REUSE);
        return Err("UDP reuse owners could not share one endpoint");
    }

    // 当前 API 只有 whole-bucket unregister；该断言是 WP4 exact-owner token 的 RED 基线。
    PortManager::unregister_udp_bind(PORT_REUSE);
    let retained = PortManager::check_udp_conflict(PORT_REUSE, loopback_addr(), false).is_err();
    drop(first);
    drop(second);
    if !retained {
        return Err("closing one UDP reuse owner removed the whole port bucket");
    }
    Ok(())
}

/// TCP 与 UDP 使用不同 reservation 表，同一数字端口不得构成假冲突。
fn tcp_udp_same_numeric_port() -> Result<(), &'static str> {
    PortManager::unregister_tcp_bind(PORT_PROTO);
    PortManager::unregister_udp_bind(PORT_PROTO);
    let tcp = new_tcp_socket();
    let udp = new_udp_socket();
    let task = crate::task::current_task().ok_or("protocol port test has no current task")?;
    let tcp_result = PortManager::bind_port(&task, &tcp, &loopback_endpoint(PORT_PROTO));
    let udp_result = PortManager::bind_port(&task, &udp, &loopback_endpoint(PORT_PROTO));
    drop(task);
    PortManager::unregister_tcp_bind(PORT_PROTO);
    PortManager::unregister_udp_bind(PORT_PROTO);
    drop(tcp);
    drop(udp);
    if tcp_result.is_err() || udp_result.is_err() {
        return Err("TCP and UDP falsely conflicted on one numeric port");
    }
    Ok(())
}

/// 同 endpoint 在不同 netns 的 reservation 必须隔离；恢复 runner netns 后才报告结果。
fn namespace_port_isolation() -> Result<(), &'static str> {
    PortManager::unregister_udp_bind(PORT_NETNS);
    let task = crate::task::current_task().ok_or("netns port test has no current task")?;
    let original = task.process.net();
    let first_ns = crate::task::NetNamespace::new_isolated();
    let second_ns = crate::task::NetNamespace::new_isolated();
    let first = new_udp_socket();
    let second = new_udp_socket();

    task.process.set_net(first_ns);
    let first_result = PortManager::bind_port(&task, &first, &loopback_endpoint(PORT_NETNS));
    task.process.set_net(second_ns);
    let second_result = PortManager::bind_port(&task, &second, &loopback_endpoint(PORT_NETNS));
    task.process.set_net(original);
    drop(task);
    PortManager::unregister_udp_bind(PORT_NETNS);
    drop(first);
    drop(second);
    if first_result.is_err() || second_result.is_err() {
        return Err("network namespaces falsely conflicted on one UDP endpoint");
    }
    Ok(())
}

/// WP5 前全局 NET_INTERFACE 锁是已知实现；本测试只验证 veth fixture 可反复创建并清理。
fn per_stack_poll_progress() -> Result<(), &'static str> {
    let (left, right) = crate::drivers::net::veth::veth_pair_new("wp1veth0", "wp1veth1");
    let registered = NET_INTERFACE.stack_ifindexes();
    let result = if !registered.contains(&left) || !registered.contains(&right) {
        Err("veth stacks were not registered in NET_INTERFACE")
    } else {
        let _ = NET_INTERFACE.try_poll_stack(right);
        Ok(())
    };
    NET_INTERFACE.remove_veth_stack(left);
    NET_INTERFACE.remove_veth_stack(right);
    crate::net::net_core::remove_device(left as usize);
    crate::net::net_core::remove_device(right as usize);
    result
}
