use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::TryFrom;
use log::info;

use crate::fs::vfs::{self, FileFlags};
use crate::mm::UserSlice;
use crate::net::posix::PosixArgsSocketType;
use crate::net::{make_unix_socket_pair, SocketFile, AF_UNIX, AF_UNSPEC, PSOCK};
use crate::task::current_task;
use crate::utils::error::SyscallErr;

/// 创建一对相互连接的 Unix 域 socket。
///
/// # Semantics
///
/// 通过 `make_unix_socket_pair()` 分配一对匿名 `UnixStreamSocket` 或
/// `UnixDatagramSocket`（取决于 `psock` 类型），分配两个新的 fd，
/// 并将 `[fd1, fd2]` 写入用户空间的 `sv` 数组。
///
/// # Errors
///
/// - `-EINVAL`：`socket_type` 的纯类型位无效。
/// - `-EPROTONOSUPPORT`：`domain` 不是 `AF_UNIX`/`AF_LOCAL`/`AF_UNSPEC`
///   （Linux `socketpair()` 仅支持 AF_UNIX，非 AF_UNIX → `EPROTONOSUPPORT`，非 `EAFNOSUPPORT`）。
/// - `-ESOCKTNOSUPPORT`：`psock` 不是 `Stream` 或 `Datagram`。
/// - `-EFAULT`：`sv` 地址写入失败。
///
/// # Linux Compatibility
///
/// Linux 6.6：非 `AF_UNIX`/`AF_LOCAL` → `-EPROTONOSUPPORT`（与 `Socket::alloc`
/// 的 `EAFNOSUPPORT` 不同）。`AF_UNSPEC` 被视为 `AF_UNIX`。
pub fn sys_socketpair(domain: u32, socket_type: u32, protocol: u32, sv: usize) -> isize {
    info!(
        "[sys_socketpair] domain {}, type {}, protocol {}, sv {}",
        domain, socket_type, protocol, sv
    );

    // 在 syscall 入口处解析 raw u32 → PSOCK + bool flags
    let type_arg = PosixArgsSocketType::from_bits_truncate(socket_type);
    let psock = match PSOCK::try_from(type_arg) {
        Ok(s) => s,
        Err(e) => return -(e as isize),
    };

    // socket_type 中不允许出现未声明的位（Linux 只识别纯类型位 +
    // SOCK_NONBLOCK + SOCK_CLOEXEC）。`from_bits_truncate` 会静默丢弃未知
    // 位，必须显式复核，否则 `SOCK_STREAM | 任意高位` 会被当作合法请求。
    if socket_type
        & !(crate::net::socket::SOCK_TYPE_MASK
            | PosixArgsSocketType::NONBLOCK.bits()
            | PosixArgsSocketType::CLOEXEC.bits())
        != 0
    {
        return -(SyscallErr::EINVAL as isize);
    }

    let is_nonblock = type_arg.is_nonblock();
    let is_cloexec = type_arg.is_cloexec();

    // Linux socketpair() only supports AF_UNIX/AF_LOCAL
    match domain as u16 {
        AF_UNIX | AF_UNSPEC => {
            // AF_UNIX 和 AF_UNSPEC 都走 unix socket pair
        }
        _ => {
            info!(
                "[sys_socketpair] domain {} not supported, only AF_UNIX is allowed",
                domain
            );
            return -(SyscallErr::EPROTONOSUPPORT as isize);
        }
    }

    // Rust's process-spawn implementation uses SOCK_SEQPACKET for its
    // close-on-exec error channel.  The Unix implementation provides the
    // same reliable byte-stream transport for SeqPacket, so accept it here
    // instead of rejecting a valid socketpair request.
    let (socket1, socket2): (Arc<dyn crate::net::Socket>, Arc<dyn crate::net::Socket>) = match psock
    {
        PSOCK::Stream | PSOCK::SeqPacket | PSOCK::Datagram => {
            let (s1, s2) = make_unix_socket_pair(is_nonblock, psock);
            (s1, s2)
        }
        _ => {
            return -(SyscallErr::ESOCKTNOSUPPORT as isize);
        }
    };

    let socket_file1 = Arc::new(SocketFile::new(socket1));
    let socket_file2 = Arc::new(SocketFile::new(socket2));

    let mut vfs_flags = FileFlags::O_RDWR;
    if is_nonblock {
        vfs_flags.insert(FileFlags::O_NONBLOCK);
    }

    let task = current_task().unwrap();
    let vf1 = vfs::File::new_without_open(socket_file1, vfs_flags, vfs::FileType::Socket);
    let vf2 = vfs::File::new_without_open(socket_file2, vfs_flags, vfs::FileType::Socket);

    // 两个 fd 必须在同一个 fd_table 临界区提交：fd2 分配失败时回滚 fd1。
    // 回滚产生的 Arc<File> 在锁外 drop，避免持锁触发 socket 析构。
    let files_ref = task.process.files();
    let mut to_release: Vec<Arc<vfs::File>> = Vec::new();
    let (fd1, fd2) = {
        let mut fd_table = files_ref.lock();
        let fd1 = match fd_table.alloc_fd(vf1, is_cloexec) {
            Ok(fd) => fd,
            Err(e) => return -(e as isize),
        };
        let fd2 = match fd_table.alloc_fd(vf2, is_cloexec) {
            Ok(fd) => fd,
            Err(e) => {
                if let Ok(file) = fd_table.drop_fd(fd1) {
                    to_release.push(file);
                }
                return -(e as isize);
            }
        };
        (fd1, fd2)
    };

    // 将两个 fd 写入用户空间的 sv 数组（sv[0] = fd1, sv[1] = fd2）
    let token = task.get_user_token();
    let fds = [fd1 as u32, fd2 as u32];
    if UserSlice::new(sv as *const u32, 2)
        .write_array_from(token, &fds)
        .is_err()
    {
        // Linux 在 sv 写入失败时不泄漏已分配的 fd。先在 fd_table 锁内摘除
        // 两个 socket fd，锁外再 drop Arc<File>，避免持锁隐式 drop 触发
        // SocketFile/UnixStreamSocket 析构死锁。
        {
            let mut fd_table = files_ref.lock();
            if let Ok(file) = fd_table.drop_fd(fd1) {
                to_release.push(file);
            }
            if let Ok(file) = fd_table.drop_fd(fd2) {
                to_release.push(file);
            }
        }
        drop(to_release);
        return -(SyscallErr::EFAULT as isize);
    }

    info!("[sys_socketpair] new sv: [{}, {}]", fd1, fd2);
    0
}
