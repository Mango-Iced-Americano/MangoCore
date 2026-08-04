//! WP1 网络 SMP focused ktest。
// allow: SIZE_OK - 这十个测试共享同一组零盘 fixture、原子 phase 和清理纪律；协调方只注册本模块。

use alloc::{
    sync::Arc,
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use smoltcp::wire::{IpAddress, IpEndpoint, IpVersion};
use spin::Mutex;

use crate::{
    kernel_tests::runner::KernelTest,
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
const PORT_RACE: u16 = 61_101;
const PORT_REUSE: u16 = 61_102;
const PORT_PROTO: u16 = 61_103;
const PORT_NETNS: u16 = 61_104;

static IRQ_HOOK_FIRED: AtomicUsize = AtomicUsize::new(0);
static IRQ_HOOK_ERRORS: AtomicUsize = AtomicUsize::new(0);
static PORT_RACE_START: AtomicUsize = AtomicUsize::new(0);
static PORT_RACE_HELPER_SUCCESS: AtomicUsize = AtomicUsize::new(0);

lazy_static! {
    /// CPU1 只在 port race 的短窗口内取得这个 Arc；runner 清理前必须 take 回。
    static ref PORT_RACE_HELPER_SOCKET: Mutex<Option<Arc<dyn Socket>>> = Mutex::new(None);
}

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
        KernelTest::new(
            "net_smp::route_handle_reuse_rejected",
            route_handle_reuse_rejected,
        ),
        KernelTest::new("net_smp::per_stack_poll_progress", per_stack_poll_progress),
        KernelTest::new("net_smp::poll_worker_no_lost_wake", poll_worker_no_lost_wake),
        KernelTest::new(
            "net_smp::tcp_dual_sender_exact_bytes",
            tcp_dual_sender_exact_bytes,
        ),
        KernelTest::new("net_smp::epollet_concurrent_edge", epollet_concurrent_edge),
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

fn bind_race_helper() {
    while PORT_RACE_START.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    let Some(socket) = PORT_RACE_HELPER_SOCKET.lock().as_ref().cloned() else {
        return;
    };
    let Some(task) = crate::task::current_task() else {
        return;
    };
    if PortManager::bind_port(&task, &socket, &loopback_endpoint(PORT_RACE)).is_ok() {
        PORT_RACE_HELPER_SUCCESS.store(1, Ordering::Release);
    }
}

/// 两个 CPU 同时经过生产 bind_port；成功数大于一说明 check/register 不是同一 reservation。
fn port_reserve_exactly_once() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 2 {
        return Ok(());
    }
    PortManager::unregister_udp_bind(PORT_RACE);
    PORT_RACE_START.store(0, Ordering::Relaxed);
    PORT_RACE_HELPER_SUCCESS.store(0, Ordering::Relaxed);
    let local = new_udp_socket();
    let remote = new_udp_socket();
    *PORT_RACE_HELPER_SOCKET.lock() = Some(remote);
    let helper = crate::task::spawn_ktest_task_on(1, bind_race_helper);
    PORT_RACE_START.store(1, Ordering::Release);
    let task = crate::task::current_task().ok_or("port race has no current task")?;
    let local_success = PortManager::bind_port(&task, &local, &loopback_endpoint(PORT_RACE)).is_ok();
    drop(task);
    let cleanup = wait_for_zombie(&helper, 1, "port reservation helper did not reach Zombie");
    let remote = PORT_RACE_HELPER_SOCKET.lock().take();
    PortManager::unregister_udp_bind(PORT_RACE);
    drop(remote);
    drop(local);
    cleanup?;
    if local_success as usize + PORT_RACE_HELPER_SUCCESS.load(Ordering::Acquire) > 1 {
        return Err("concurrent bind reserved one UDP endpoint more than once");
    }
    Ok(())
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

/// WP5 才引入 stack-local route directory 与 generation revalidation；WP1 仅保留测试位。
fn route_handle_reuse_rejected() -> Result<(), &'static str> {
    // protective: requires WP5 route directory and reusable-slot test hook.
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

/// WP6 才有 requested/completed generation worker；现阶段只证明 loopback poll 路径可调用。
fn poll_worker_no_lost_wake() -> Result<(), &'static str> {
    // protective: requires WP6 poll-worker generation state and window hooks.
    let _ = NET_INTERFACE.try_poll_irq();
    NET_INTERFACE.poll();
    Ok(())
}

/// 当前 TCP accept 只能产出 fd，缺少把 accepted SocketFile 取回为内核 Socket 的公共入口。
/// WP6 增加 socket/worker fixture 后替换为 CPU1/CPU2 同 socket 的编号 frame 精确校验。
fn tcp_dual_sender_exact_bytes() -> Result<(), &'static str> {
    // protective: requires WP6 accepted-socket kernel entry for frame verification.
    Ok(())
}

/// EventPoll 的 add/modify/wait 仍是私有 API；WP6 之前不能在无用户页 ktest 中诚实驱动 EPOLLET。
fn epollet_concurrent_edge() -> Result<(), &'static str> {
    // protective: requires WP6 eventpoll kernel entry or mapped-user probe fixture.
    Ok(())
}
