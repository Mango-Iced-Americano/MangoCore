use alloc::sync::Arc;
use core::convert::TryFrom;
use log::info;

use crate::fs::vfs::{self, FileFlags};
use crate::mm::UserBufferWriter;
use crate::net::posix::PosixArgsSocketType;
use crate::net::{make_unix_socket_pair, SocketFile, AF_UNIX, AF_UNSPEC, PSOCK};
use crate::task::current_task;
use crate::utils::error::SyscallErr;

/// 创建一对相互连接的 Unix 域 socket。
///
/// # Semantics
///
/// 通过 `make_unix_socket_pair()` 分配一对匿名 Unix socket，分配两个新的 fd，
/// 并将 `[fd1, fd2]` 写入用户空间的 `sv` 数组。
///
/// # Errors
///
/// - `-EINVAL`：`socket_type` 包含无效类型或控制标志位。
/// - `-EPROTONOSUPPORT`：`domain` 不是 `AF_UNIX`/`AF_LOCAL`/`AF_UNSPEC`
///   （Linux `socketpair()` 仅支持 AF_UNIX，非 AF_UNIX → `EPROTONOSUPPORT`，非 `EAFNOSUPPORT`）。
/// - `-ESOCKTNOSUPPORT`：`psock` 不是 `Stream`、`Datagram` 或 `SeqPacket`。
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

    // Parse all bits strictly: only the low four type bits and the two
    // socketpair control flags are accepted.
    let type_arg = match PosixArgsSocketType::from_bits(socket_type) {
        Some(type_arg) => type_arg,
        None => return -(SyscallErr::EINVAL as isize),
    };
    let psock = match PSOCK::try_from(type_arg) {
        Ok(s) => s,
        Err(e) => return -(e as isize),
    };
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

    let task = match current_task() {
        Some(task) => task,
        None => return -(SyscallErr::ESRCH as isize),
    };
    let token = task.get_user_token();
    if sv == 0 {
        return -(SyscallErr::EFAULT as isize);
    }

    // Fault in the entire output array before acquiring resources, so a user
    // copy failure cannot leave half of the fd transaction visible.
    let mut sv_writer = match UserBufferWriter::new(
        token,
        sv as *mut u8,
        core::mem::size_of::<[u32; 2]>(),
    ) {
        Ok(writer) => writer,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };

    let (socket1, socket2): (Arc<dyn crate::net::Socket>, Arc<dyn crate::net::Socket>) =
        match make_unix_socket_pair(is_nonblock, psock) {
            Ok(sockets) => sockets,
            Err(err) => return -(err as isize),
        };

    let socket_file1 = Arc::new(SocketFile::new(socket1));
    let socket_file2 = Arc::new(SocketFile::new(socket2));

    let mut vfs_flags = FileFlags::O_RDWR;
    if is_nonblock {
        vfs_flags.insert(FileFlags::O_NONBLOCK);
    }

    let vf1 = vfs::File::new_without_open(socket_file1, vfs_flags, vfs::FileType::Socket);
    let vf2 = vfs::File::new_without_open(socket_file2, vfs_flags, vfs::FileType::Socket);

    let files_ref = task.process.files();
    let (fd1, fd2) = {
        let mut fd_table = files_ref.lock();
        let fd1 = match fd_table.alloc_fd(vf1, is_cloexec) {
            Ok(fd) => fd,
            Err(err) => return -(err as isize),
        };
        let fd2 = match fd_table.alloc_fd(vf2, is_cloexec) {
            Ok(fd) => fd,
            Err(err) => {
                let removed_fd = fd_table.drop_fd(fd1);
                drop(fd_table);
                drop(removed_fd);
                return -(err as isize);
            }
        };
        (fd1, fd2)
    };

    // Write the two native-endian u32 file descriptors only after both fd
    // allocations have succeeded.  UserBufferWriter was fully faulted in above.
    let fd1_bytes = (fd1 as u32).to_ne_bytes();
    let fd2_bytes = (fd2 as u32).to_ne_bytes();
    let mut fd_bytes = [0_u8; core::mem::size_of::<[u32; 2]>()];
    fd_bytes[..core::mem::size_of::<u32>()].copy_from_slice(&fd1_bytes);
    fd_bytes[core::mem::size_of::<u32>()..].copy_from_slice(&fd2_bytes);
    if !matches!(sv_writer.write_from(&fd_bytes), Ok(copied) if copied == fd_bytes.len()) {
        let removed_fds = {
            let mut fd_table = files_ref.lock();
            (fd_table.drop_fd(fd1), fd_table.drop_fd(fd2))
        };
        drop(removed_fds);
        return -(SyscallErr::EFAULT as isize);
    }

    info!("[sys_socketpair] new sv: [{}, {}]", fd1, fd2);
    0
}
