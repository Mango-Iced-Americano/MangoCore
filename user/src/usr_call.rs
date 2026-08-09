use crate::syscall::*;
pub fn dup(fd: usize) -> isize {
    sys_dup(fd)
}
pub fn open(path: &str, flags: crate::OpenFlags) -> isize {
    sys_open(path, flags.bits)
}
pub fn close(fd: usize) -> isize {
    sys_close(fd)
}

pub fn mount(
    source: *const u8,
    target: *const u8,
    fstype: *const u8,
    flags: usize,
    data: usize,
) -> isize {
    sys_mount(source, target, fstype, flags, data)
}
pub fn pipe(pipe_fd: &mut [i32]) -> isize {
    sys_pipe(pipe_fd)
}
pub fn read(fd: usize, buf: &mut [u8]) -> isize {
    sys_read(fd, buf)
}
pub fn write(fd: usize, buf: &[u8]) -> isize {
    sys_write(fd, buf)
}
pub fn getchar() -> u8 {
    let mut buf: [u8; 1] = [0u8];
    sys_read(0, &mut buf);
    buf[0]
}
pub fn exit(exit_code: i32) -> ! {
    sys_exit(exit_code);
}
pub fn yield_() -> isize {
    sys_yield()
}
pub fn get_time() -> isize {
    sys_get_time()
}
pub fn getpid() -> isize {
    sys_getpid()
}
pub fn getppid() -> isize {
    sys_getppid()
}
pub fn fork() -> isize {
    sys_fork()
}
pub fn vfork() -> isize {
    sys_vfork()
}
pub fn exec(path: &str, args: &[*const u8], envp: &[*const u8]) -> isize {
    sys_exec(path, args, envp)
}
pub fn chdir(path: &str) -> isize {
    sys_chdir(path)
}

pub fn chroot(path: &str) -> isize {
    sys_chroot(path)
}

pub fn wait(exit_code: &mut i32) -> isize {
    sys_waitpid(-1, exit_code as *mut _)
}

pub fn waitpid(pid: usize, exit_code: &mut i32) -> isize {
    sys_waitpid(pid as isize, exit_code as *mut _)
}

pub fn waitpid_wnohang(pid: isize, exit_code: &mut i32) -> isize {
    const WNOHANG: usize = 1;
    sys_waitpid_flags(pid, exit_code as *mut _, WNOHANG)
}

pub fn sleep(period_ms: usize) {
    let start = sys_get_time();
    while sys_get_time() < start + period_ms as isize {
        sys_yield();
    }
}
pub fn kill(pid: usize, sig: usize) -> isize {
    sys_kill(pid, sig)
}
pub const SIGKILL: usize = 9;
pub const SIGTERM: usize = 15;
pub const SIGINT: usize = 2;
pub const SIGCHLD: usize = 17;

#[repr(C)]
pub struct SigAction {
    pub handler: usize,
    pub flags: usize,
    pub restorer: usize,
    pub mask: u64,
}

pub fn sigaction(signum: usize, action: &SigAction) -> isize {
    sys_rt_sigaction(
        signum,
        action as *const SigAction as usize,
        0,
        core::mem::size_of::<u64>(),
    )
}

pub fn setpgid(pid: usize, pgid: usize) -> isize {
    sys_setpgid(pid, pgid)
}

pub fn getpgid(pid: usize) -> isize {
    sys_getpgid(pid)
}

pub fn shutdown() -> isize {
    sys_shutdown()
}

// === Socket API ===
// AF_UNIX (Linux 值)
pub const AF_UNIX: usize = 1;
pub const SOCK_STREAM: usize = 1;
pub const SOCK_DGRAM: usize = 2;
pub const SOCK_SEQPACKET: usize = 5;
pub const SOCK_CLOEXEC: usize = 0o2000000;
pub const SOCK_NONBLOCK: usize = 0o4000;

pub const SHUT_RD: usize = 0;
pub const SHUT_WR: usize = 1;
pub const SHUT_RDWR: usize = 2;

pub fn socket(domain: usize, type_: usize, protocol: usize) -> isize {
    sys_socket(domain, type_, protocol)
}

pub fn socketpair(domain: usize, type_: usize, protocol: usize, sv: &mut [i32]) -> isize {
    sys_socketpair(domain, type_, protocol, sv.as_mut_ptr())
}

pub fn bind(sockfd: usize, addr: *const u8, addrlen: usize) -> isize {
    sys_bind(sockfd, addr, addrlen)
}

pub fn listen(sockfd: usize, backlog: usize) -> isize {
    sys_listen(sockfd, backlog)
}

pub fn accept(sockfd: usize, addr: *mut u8, addrlen: *mut usize) -> isize {
    sys_accept(sockfd, addr, addrlen)
}

pub fn connect(sockfd: usize, addr: *const u8, addrlen: usize) -> isize {
    sys_connect(sockfd, addr, addrlen)
}

pub fn sendto(
    sockfd: usize,
    buf: *const u8,
    len: usize,
    flags: usize,
    dest_addr: *const u8,
    addrlen: usize,
) -> isize {
    sys_sendto(sockfd, buf, len, flags, dest_addr, addrlen)
}

pub fn recvfrom(
    sockfd: usize,
    buf: *mut u8,
    len: usize,
    flags: usize,
    src_addr: *mut u8,
    addrlen: *mut usize,
) -> isize {
    sys_recvfrom(sockfd, buf, len, flags, src_addr, addrlen)
}

pub fn getsockname(sockfd: usize, addr: *mut u8, addrlen: *mut usize) -> isize {
    sys_getsockname(sockfd, addr, addrlen)
}

pub fn getpeername(sockfd: usize, addr: *mut u8, addrlen: *mut usize) -> isize {
    sys_getpeername(sockfd, addr, addrlen)
}

pub fn sock_shutdown(sockfd: usize, how: usize) -> isize {
    sys_sock_shutdown(sockfd, how)
}

pub fn getdents64(fd: usize, buf: &mut [u8]) -> isize {
    sys_getdents64(fd, buf)
}

pub fn chmod(path: &str, mode: u32) -> isize {
    sys_fchmodat(crate::syscall::AT_FDCWD, path.as_ptr() as *const u8, mode)
}
