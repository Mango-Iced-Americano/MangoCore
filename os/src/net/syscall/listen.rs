use crate::net::socket::inet::common::port::{AutoBindPurpose, PortManager};
use crate::net::socket::inet::stream::inner::MAX_LISTEN_BACKLOG;
use crate::task::current_task;

/// 将 socket 标记为被动模式，准备接受连接。
///
/// # Semantics
///
/// 委托 `socket.listen(backlog)`。backlog 决定监听 socket 可容纳的并发
/// pending/established 连接槽数；`0` 按 1 处理，上限
/// [`MAX_LISTEN_BACKLOG`]。官方 CAgent 的 LLM server 以 backlog=10 同时服务
/// 10 个并发 agent 客户端，旧实现忽略 backlog 并固定 8 槽，导致第 9/10 个
/// 客户端 connect 被拒绝而整个 testcase 失败。
///
/// # Errors
///
/// - 由 `socket.listen()` 产生（如 `-EOPNOTSUPP`：UDP socket 不支持 listen）。
///
/// # Linux Compatibility
///
/// backlog 上限远小于 Linux `somaxconn`（4096），但语义方向一致；
/// `SO_RCVBUF` 的 backlog 关联语义（Linux 3.x+ `somaxconn`）未实现。
pub fn sys_listen(sockfd: u32, backlog: u32) -> isize {
    let socket = crate::get_socket!(sockfd);
    let task = current_task().unwrap();
    if let Err(error) = PortManager::ensure_auto_bound(
        &task,
        &socket,
        None,
        AutoBindPurpose::Listen,
    ) {
        return -(error as isize);
    }
    let backlog = backlog.min(MAX_LISTEN_BACKLOG as u32);
    match socket.listen(backlog) {
        Ok(s) => s as isize,
        Err(err) => -(err as isize),
    }
}
