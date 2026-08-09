//! WP1 网络 SMP focused ktest。
// allow: SIZE_OK - 这九个测试共享同一组零盘 fixture、原子 phase 和清理纪律；协调方只注册本模块。

use alloc::{sync::Arc, vec, vec::Vec};
use smoltcp::wire::{IpAddress, IpEndpoint, IpVersion};

use crate::{
    kernel_tests::{
        probe::{
            attach_probe_to_runner, build_epoll_edge_reader, build_eventfd_writer,
            build_udp_bind_probe, deadline_after, probe_quiesced, reap_probe, stop_probe,
        },
        runner::KernelTest,
    },
    net::{
        config::NET_INTERFACE,
        routing::InetProtocol,
        socket::{
            inet::{common::port::PortManager, datagram::udp::UdpSocket, stream::TcpSocket},
            packet::{PacketSocket, ETH_P_ALL},
            Endpoint, Socket,
        },
        PacketEndpoint,
    },
};

const IRQ_POLL_WAIT_TICKS_DIVISOR: usize = 8;
const PORT_RACE: u16 = 61_105;
const PORT_REUSE: u16 = 61_102;
const PORT_PROTO: u16 = 61_103;
const PORT_NETNS: u16 = 61_104;
const SOCKET_RETIRE_CYCLES: usize = 8;

/// 返回 WP1 的九个独立测试。注册由 Wave 2 协调方完成。
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "net_smp::irq_poll_is_publish_only",
            irq_poll_is_publish_only,
        ),
        KernelTest::new(
            "net_smp::port_reserve_exactly_once",
            port_reserve_exactly_once,
        ),
        KernelTest::new(
            "net_smp::udp_reuse_release_exact_owner",
            udp_reuse_release_exact_owner,
        ),
        KernelTest::new(
            "net_smp::tcp_udp_same_numeric_port",
            tcp_udp_same_numeric_port,
        ),
        KernelTest::new(
            "net_smp::namespace_port_isolation",
            namespace_port_isolation,
        ),
        KernelTest::new(
            "net_smp::route_handle_reuse_rejected",
            route_handle_reuse_rejected,
        ),
        KernelTest::new("net_smp::per_stack_poll_progress", per_stack_poll_progress),
        KernelTest::new(
            "net_smp::poll_worker_no_lost_wake",
            poll_worker_no_lost_wake,
        ),
        KernelTest::new(
            "net_smp::dropped_udp_buffers_reclaim_without_traffic",
            dropped_udp_buffers_reclaim_without_traffic,
        ),
        KernelTest::new("net_smp::epollet_concurrent_edge", epollet_concurrent_edge),
    ]
}

fn loopback_endpoint(port: u16) -> Endpoint {
    Endpoint::Ip(IpEndpoint::new(IpAddress::v4(127, 0, 0, 1), port))
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

/// IRQ 入口必须有界返回且只发布请求，不能在中断上下文执行 smoltcp。
fn irq_poll_is_publish_only() -> Result<(), &'static str> {
    let started = crate::hal::get_time();
    let _ = NET_INTERFACE.try_poll_irq();
    let elapsed = crate::hal::get_time().wrapping_sub(started);

    if elapsed >= (crate::hal::get_clock_freq() / IRQ_POLL_WAIT_TICKS_DIVISOR).max(1) {
        return Err("IRQ poll did not return within the publish-only bound");
    }

    // 直接调用 try_poll_irq() 不经过硬 IRQ 收尾，因此由 runner 模拟安全点领取
    // deferred wake。worker 的真实推进能力由下面的 veth 投递用例验证。
    NET_INTERFACE.run_deferred_net_wake();
    Ok(())
}

/// 两个完整用户 TCB 在 CPU1/CPU2 经过 socket()+bind()；runner 不占用 probe 的 current 槽。
fn port_reserve_exactly_once() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
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

/// IRQ 发布请求后，CPU0 worker 必须把真实 veth 帧投递到 AF_PACKET，不能丢失唤醒。
fn poll_worker_no_lost_wake() -> Result<(), &'static str> {
    let (left, right) = crate::drivers::net::veth::veth_pair_new("npwveth0", "npwveth1");
    let result = (|| {
        let packet = Arc::new(PacketSocket::new(ETH_P_ALL));
        PacketSocket::register_packet_socket(&packet);
        packet
            .bind(&Endpoint::Packet(PacketEndpoint {
                ifindex: right,
                protocol: ETH_P_ALL,
                hatype: 1,
                pkttype: 0,
                halen: 6,
                addr: [0; 8],
            }))
            .map_err(|_| "AF_PACKET bind for poll worker probe failed")?;
        let frame = [0x02, 0, 0, 0, 0, 4, 0x02, 0, 0, 0, 0, 3, 0x08, 0x00];
        NET_INTERFACE
            .transmit_on_stack(left, &frame)
            .map_err(|_| "poll worker veth transmit failed")?;
        let _ = NET_INTERFACE.try_poll_irq();
        NET_INTERFACE.run_deferred_net_wake();

        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
        crate::hal::with_local_interrupts_enabled(|| loop {
            let mut received = [0u8; 32];
            if matches!(
                packet.try_recv(&mut received),
                Ok(length) if length as usize == frame.len() && received[..frame.len()] == frame
            ) {
                break Ok(());
            }
            if crate::hal::get_time() >= deadline {
                break Err("CPU0 poll worker lost the IRQ-published veth request");
            }
            // worker 与 runner 都固定在 CPU0，必须经过安全点交出执行权。
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        })
    })();
    NET_INTERFACE.remove_veth_stack(left);
    NET_INTERFACE.remove_veth_stack(right);
    crate::net::net_core::remove_device(left as usize);
    crate::net::net_core::remove_device(right as usize);
    result
}

/// 先让创建 socket 时的 poll 请求被消费，再在无后续 IRQ/syscall 的窗口释放最后
/// Arc。每轮都要求 SocketSet UDP 数回到基线，覆盖“大缓冲连续 close 不累积”。
fn dropped_udp_buffers_reclaim_without_traffic() -> Result<(), &'static str> {
    let (_, baseline_udp, _, _) = NET_INTERFACE.socket_stats();
    let timeout = crate::hal::get_time()
        .saturating_add(crate::hal::get_clock_freq().saturating_mul(3));

    crate::hal::with_local_interrupts_enabled(|| {
        for _ in 0..SOCKET_RETIRE_CYCLES {
            let socket = Arc::new(UdpSocket::new(IpVersion::Ipv4));
            UdpSocket::register_udp_socket(&socket);

            while NET_INTERFACE.poll_request_pending() {
                if crate::hal::get_time() >= timeout {
                    return Err("UDP create poll request was not consumed by worker");
                }
                crate::task::run_task_safe_point();
                core::hint::spin_loop();
            }

            drop(socket);
            loop {
                let (_, udp_count, _, pending_removals) = NET_INTERFACE.socket_stats();
                if udp_count == baseline_udp && pending_removals == 0 {
                    break;
                }
                if crate::hal::get_time() >= timeout {
                    return Err("dropped UDP SocketSet payloads were not reclaimed without traffic");
                }
                crate::task::run_task_safe_point();
                core::hint::spin_loop();
            }
        }
        Ok(())
    })
}

/// reservation 的 token 与 socket Weak 身份必须共同决定释放目标。
fn udp_reuse_release_exact_owner() -> Result<(), &'static str> {
    let first = new_udp_socket();
    let second = new_udp_socket();
    first
        .set_reuse_addr(true)
        .map_err(|_| "UDP socket rejected SO_REUSEADDR")?;
    second
        .set_reuse_addr(true)
        .map_err(|_| "UDP socket rejected SO_REUSEADDR")?;
    let namespace = crate::task::NetNamespace::new_isolated();
    let endpoint = loopback_endpoint(PORT_REUSE);
    let first_intent = first
        .snapshot_bind_intent(&endpoint)
        .map_err(|_| "first UDP bind intent failed")?;
    let second_intent = second
        .snapshot_bind_intent(&endpoint)
        .map_err(|_| "second UDP bind intent failed")?;
    let first_reservation = namespace
        .ports
        .lock()
        .reserve(&namespace, first_intent, &first)
        .map_err(|_| "first UDP reservation failed")?;
    let second_reservation = namespace
        .ports
        .lock()
        .reserve(&namespace, second_intent, &second)
        .map_err(|_| "second UDP reuse reservation failed")?;
    namespace
        .ports
        .lock()
        .commit(&first_reservation, &first)
        .map_err(|_| "first UDP reservation commit failed")?;
    namespace
        .ports
        .lock()
        .commit(&second_reservation, &second)
        .map_err(|_| "second UDP reservation commit failed")?;

    first_reservation.release();
    if namespace
        .ports
        .lock()
        .commit(&first_reservation, &first)
        .is_ok()
    {
        return Err("released UDP reservation remained addressable");
    }
    if namespace
        .ports
        .lock()
        .commit(&second_reservation, &second)
        .is_err()
    {
        return Err("releasing one UDP owner removed its reuse peer");
    }
    second_reservation.release();
    drop(first);
    drop(second);
    Ok(())
}

/// TCP 与 UDP 使用不同 reservation 表，同一数字端口不得构成假冲突。
fn tcp_udp_same_numeric_port() -> Result<(), &'static str> {
    let tcp = new_tcp_socket();
    let udp = new_udp_socket();
    let task = crate::task::current_task().ok_or("protocol port test has no current task")?;
    let tcp_result = PortManager::bind_port(&task, &tcp, &loopback_endpoint(PORT_PROTO));
    let udp_result = PortManager::bind_port(&task, &udp, &loopback_endpoint(PORT_PROTO));
    drop(task);
    drop(tcp);
    drop(udp);
    if tcp_result.is_err() || udp_result.is_err() {
        return Err("TCP and UDP falsely conflicted on one numeric port");
    }
    Ok(())
}

/// 同 endpoint 在不同 netns 的 reservation 必须隔离；恢复 runner netns 后才报告结果。
fn namespace_port_isolation() -> Result<(), &'static str> {
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
    drop(first);
    drop(second);
    if first_result.is_err() || second_result.is_err() {
        return Err("network namespaces falsely conflicted on one UDP endpoint");
    }
    Ok(())
}

/// veth 左端发送的真实以太网帧必须经右端 DeviceStack poll 投递到 AF_PACKET。
fn per_stack_poll_progress() -> Result<(), &'static str> {
    let (left, right) = crate::drivers::net::veth::veth_pair_new("wp1veth0", "wp1veth1");
    let registered = NET_INTERFACE.stack_ifindexes();
    // 测试主体放进闭包，使 `?` 只退出闭包；无论哪一步失败，下面都能回收 veth。
    let result = (|| {
        if !registered.contains(&left) || !registered.contains(&right) {
            return Err("veth stacks were not registered in NET_INTERFACE");
        }
        let packet = Arc::new(PacketSocket::new(ETH_P_ALL));
        PacketSocket::register_packet_socket(&packet);
        packet
            .bind(&Endpoint::Packet(PacketEndpoint {
                ifindex: right,
                protocol: ETH_P_ALL,
                hatype: 1,
                pkttype: 0,
                halen: 6,
                addr: [0; 8],
            }))
            .map_err(|_| "AF_PACKET bind to right veth failed")?;
        let frame = [
            0x02, 0, 0, 0, 0, 2, // destination MAC
            0x02, 0, 0, 0, 0, 1, // source MAC
            0x08, 0x00, // IPv4 ethertype; payload may be empty for this delivery probe
        ];
        NET_INTERFACE
            .transmit_on_stack(left, &frame)
            .map_err(|_| "left veth transmit failed")?;
        let _ = NET_INTERFACE.try_poll_stack(right);
        let mut received = [0u8; 32];
        let delivery = match packet.try_recv(&mut received) {
            Ok(length) if length as usize == frame.len() && received[..frame.len()] == frame => {
                Ok(())
            }
            _ => Err("right DeviceStack poll did not deliver the veth frame"),
        };
        drop(packet);
        delivery
    })();
    // 网络设备会进入全局注册表；清理不能依赖测试主体成功，否则重复运行会互相污染。
    NET_INTERFACE.remove_veth_stack(left);
    NET_INTERFACE.remove_veth_stack(right);
    crate::net::net_core::remove_device(left as usize);
    crate::net::net_core::remove_device(right as usize);
    result
}

/// WP5 路由重验：route handle 单调不复用，移除后旧 handle 不得 alias 新 socket。
///
/// FAIL-before：旧 route 修改新 socket；PASS-after：stack 内 route ID 重验失败。
fn route_handle_reuse_rejected() -> Result<(), &'static str> {
    let (left, right) = crate::drivers::net::veth::veth_pair_new("rhrveth0", "rhrveth1");
    let cleanup = |left: u32, right: u32| {
        NET_INTERFACE.remove_veth_stack(left);
        NET_INTERFACE.remove_veth_stack(right);
        crate::net::net_core::remove_device(left as usize);
        crate::net::net_core::remove_device(right as usize);
    };

    // 在 left 栈上注册一个真实 smoltcp UDP socket，得到单调 route handle。
    let first = {
        let rx = smoltcp::socket::udp::PacketBuffer::new(
            vec![smoltcp::socket::udp::PacketMetadata::EMPTY; 4],
            vec![0u8; 2048],
        );
        let tx = smoltcp::socket::udp::PacketBuffer::new(
            vec![smoltcp::socket::udp::PacketMetadata::EMPTY; 4],
            vec![0u8; 2048],
        );
        let socket = smoltcp::socket::udp::Socket::new(rx, tx);
        match NET_INTERFACE.add_routed_socket_on(InetProtocol::Udp, socket, left) {
            Some(handle) => handle,
            None => {
                cleanup(left, right);
                return Err("failed to add first routed socket");
            }
        }
    };
    if NET_INTERFACE.routed_ifindex(first) != Some(left) {
        cleanup(left, right);
        return Err("first route handle did not resolve to left stack");
    }

    // 移除该 route；此后旧 handle 必须立即失效。
    NET_INTERFACE.remove_routed(first);
    if NET_INTERFACE.routed_ifindex(first).is_some() {
        cleanup(left, right);
        return Err("removed route handle still resolved");
    }
    if NET_INTERFACE.udp_routed_socket(first, |_| ()).is_some() {
        cleanup(left, right);
        return Err("removed route handle still reached a socket");
    }

    // 在 right 栈注册新 socket：新 handle 必须不同，且旧 handle 不得 alias 它。
    let second = {
        let rx = smoltcp::socket::udp::PacketBuffer::new(
            vec![smoltcp::socket::udp::PacketMetadata::EMPTY; 4],
            vec![0u8; 2048],
        );
        let tx = smoltcp::socket::udp::PacketBuffer::new(
            vec![smoltcp::socket::udp::PacketMetadata::EMPTY; 4],
            vec![0u8; 2048],
        );
        let socket = smoltcp::socket::udp::Socket::new(rx, tx);
        match NET_INTERFACE.add_routed_socket_on(InetProtocol::Udp, socket, right) {
            Some(handle) => handle,
            None => {
                cleanup(left, right);
                return Err("failed to add second routed socket");
            }
        }
    };
    if second == first {
        cleanup(left, right);
        return Err("route handle was reused");
    }
    if NET_INTERFACE.routed_ifindex(first).is_some()
        || NET_INTERFACE.routed_ifindex(second) != Some(right)
    {
        cleanup(left, right);
        return Err("stale route handle aliased the new socket");
    }
    NET_INTERFACE.remove_routed(second);
    cleanup(left, right);
    Ok(())
}

/// EPOLLET edge 语义 + eventfd 跨 CPU 通知：writer 写一次，reader 只收到一次
/// edge；数据未 drain 时第二次非阻塞 pwait 必须返回 0（edge 不 level）。
fn epollet_concurrent_edge() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
    // runner 创建 eventfd，并把同一 File 安装进两个用户探针的 fd 表。
    let runner_task = crate::task::current_task().ok_or("epollet test has no current task")?;
    let eventfd = crate::fs::eventfd::sys_eventfd2(0, 0);
    if eventfd < 0 {
        drop(runner_task);
        return Err("failed to create eventfd for epollet probe");
    }
    let eventfd_file = {
        // 先克隆 FdTable Arc，再在锁内取 File；guard 不跨表达式存活。
        let files = runner_task.process.files();
        let mut guard = files.lock();
        match guard.get_file(eventfd as usize) {
            Ok(file) => file,
            Err(_) => {
                drop(guard);
                drop(runner_task);
                return Err("failed to retrieve eventfd file");
            }
        }
    };
    drop(runner_task);

    let reader = build_epoll_edge_reader(eventfd_file.clone())?;
    let writer = build_eventfd_writer(eventfd_file)?;
    reader.set_initial_cpus_allowed(1usize << 1);
    writer.set_initial_cpus_allowed(1usize << 2);
    let reader_parent = attach_probe_to_runner(&reader)?;
    let writer_parent = attach_probe_to_runner(&writer)?;
    // reader 先入队并阻塞在 epoll_pwait；writer 随后写入触发一次 edge。
    crate::task::publish_task_on(reader.clone(), 1);
    crate::task::publish_task_on(writer.clone(), 2);
    let reader_done = probe_quiesced(&reader, &reader.process, 1, deadline_after(3));
    let writer_done = probe_quiesced(&writer, &writer.process, 2, deadline_after(3));
    let reader_clean = reader_done || stop_probe(&reader, &reader.process, 1);
    let writer_clean = writer_done || stop_probe(&writer, &writer.process, 2);
    let reader_reaped = reap_probe(&reader_parent, &reader);
    let writer_reaped = reap_probe(&writer_parent, &writer);
    if !reader_clean || !writer_clean || !reader_reaped || !writer_reaped {
        return Err("epollet probes did not quiesce and reap");
    }
    let reader_status = reader.process.exit_code();
    let writer_status = writer.process.exit_code();
    if reader_status != 0 || writer_status != 0 {
        crate::println!(
            "# net_smp epollet probe status: reader={}, writer={}",
            reader_status,
            writer_status
        );
        return Err("epollet probe syscalls failed");
    }
    Ok(())
}
