#![allow(unused)]

use core::arch::global_asm;

const SYSCALL_GETCWD: usize = 17;
const SYSCALL_DUP: usize = 23;
const SYSCALL_DUP3: usize = 24;
const SYSCALL_FCNTL: usize = 25;
const SYSCALL_IOCTL: usize = 29;
const SYSCALL_MKDIRAT: usize = 34;
const SYSCALL_UNLINKAT: usize = 35;
const SYSCALL_SYMLINKAT: usize = 36;
const SYSCALL_LINKAT: usize = 37;
const SYSCALL_UMOUNT2: usize = 39;
const SYSCALL_MOUNT: usize = 40;
const SYSCALL_FTRUNCATE: usize = 46;
const SYSCALL_FACCESSAT: usize = 48;
const SYSCALL_FACCESSAT2: usize = 439;
const SYSCALL_CHDIR: usize = 49;
const SYSCALL_OPENAT: usize = 56;
const SYSCALL_CLOSE: usize = 57;
const SYSCALL_PIPE: usize = 59;
const SYSCALL_GETDENTS64: usize = 61;
const SYSCALL_LSEEK: usize = 62;
const SYSCALL_READ: usize = 63;
const SYSCALL_WRITE: usize = 64;
const SYSCALL_READV: usize = 65;
const SYSCALL_WRITEV: usize = 66;
const SYSCALL_SENDFILE: usize = 71;
const SYSCALL_PSELECT6: usize = 72;
const SYSCALL_PPOLL: usize = 73;
const SYSCALL_READLINKAT: usize = 78;
const SYSCALL_NEW_FSTATAT: usize = 79;
const SYSCALL_FSTAT: usize = 80;
const SYSCALL_SYNC: usize = 81;
const SYSCALL_FSYNC: usize = 82;
const SYSCALL_TIMERFD_CREATE: usize = 85;
const SYSCALL_TIMERFD_SETTIME: usize = 86;
const SYSCALL_TIMERFD_GETTIME: usize = 87;
const SYSCALL_UTIMENSAT: usize = 88;
const SYSCALL_EXIT: usize = 93;
const SYSCALL_EXIT_GRUOP: usize = 94;
const SYSCALL_SET_TID_ADDRESS: usize = 96;
const SYSCALL_NANOSLEEP: usize = 101;
const SYSCALL_GETITIMER: usize = 102;
const SYSCALL_SETITIMER: usize = 103;
const SYSCALL_TIMER_CREATE: usize = 107;
const SYSCALL_TIMER_GETTIME: usize = 108;
const SYSCALL_TIMER_SETTIME: usize = 110;
const SYSCALL_TIMER_DELETE: usize = 111;
const SYSCALL_CLOCK_GETTIME: usize = 113;
const SYSCALL_CLOCK_SETTIME: usize = 112;
const SYSCALL_CLOCK_NANOSLEEP: usize = 115;
const SYSCALL_YIELD: usize = 124;
const SYSCALL_KILL: usize = 129;
const SYSCALL_SIGACTION: usize = 134;
const SYSCALL_SIGPROCMASK: usize = 135;
const SYSCALL_SIGRETURN: usize = 139;
const SYSCALL_TIMES: usize = 153;
const SYSCALL_SETPGID: usize = 154;
const SYSCALL_GETPGID: usize = 155;
const SYSCALL_UNAME: usize = 160;
const SYSCALL_GETRUSAGE: usize = 165;
const SYSCALL_GET_TIME_OF_DAY: usize = 169;
const SYSCALL_GETPID: usize = 172;
const SYSCALL_GETPPID: usize = 173;
const SYSCALL_GETUID: usize = 174;
const SYSCALL_GETEUID: usize = 175;
const SYSCALL_GETGID: usize = 176;
const SYSCALL_GETEGID: usize = 177;
const SYSCALL_GETTID: usize = 178;
const SYSCALL_SBRK: usize = 213;
const SYSCALL_BRK: usize = 214;
const SYSCALL_MUNMAP: usize = 215;
const SYSCALL_CLONE: usize = 220;
const SYSCALL_EXECVE: usize = 221;
const SYSCALL_MMAP: usize = 222;
const SYSCALL_MPROTECT: usize = 226;
const SYSCALL_WAIT4: usize = 260;
const SYSCALL_PRLIMIT: usize = 261;
const SYSCALL_RENAMEAT2: usize = 276;
const SYSCALL_STATX: usize = 291;
// Not standard POSIX sys_call
const SYSCALL_LS: usize = 500;
const SYSCALL_SHUTDOWN: usize = 501;
const SYSCALL_CLEAR: usize = 502;
const SYSCALL_EXT4_COUNTERS: usize = 503;
const SYSCALL_OPEN: usize = 506; //where?
const SYSCALL_GET_TIME: usize = 1690; //you mean get time of day by 169?

#[cfg(target_arch = "loongarch64")]
global_asm!(include_str!("syscall.S"));
#[cfg(target_arch = "loongarch64")]
extern "C" {
    pub fn __syscall(id: usize, args0: usize, args1: usize, args2: usize, args3: usize, args4: usize, args5: usize) -> isize;
}

fn syscall(id: usize, args: [usize; 3]) -> isize {
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        __syscall(id, args[0], args[1], args[2], 0, 0, 0)
    }
    #[cfg(target_arch = "riscv64")]
    {
        let mut ret: isize;
        unsafe {
            core::arch::asm!(
                "ecall",
                inlateout("x10") args[0] => ret,
                in("x11") args[1],
                in("x12") args[2],
                in("x17") id
            );
        }
        ret
    }
}

/// 4-argument syscall (用于 socketpair 等需要4个参数的调用)
fn syscall4(id: usize, args: [usize; 4]) -> isize {
    #[cfg(target_arch = "riscv64")]
    {
        let mut ret: isize;
        unsafe {
            core::arch::asm!(
                "ecall",
                inlateout("x10") args[0] => ret,
                in("x11") args[1],
                in("x12") args[2],
                in("x13") args[3],
                in("x17") id
            );
        }
        ret
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        __syscall(id, args[0], args[1], args[2], args[3], 0, 0)
    }
}

/// 6-argument syscall (用于 sendto/recvfrom/setsockopt 等需要超过3个参数的 socket 调用)
fn syscall6(id: usize, args: [usize; 6]) -> isize {
    #[cfg(target_arch = "riscv64")]
    {
        let mut ret: isize;
        unsafe {
            core::arch::asm!(
                "ecall",
                inlateout("x10") args[0] => ret,
                in("x11") args[1],
                in("x12") args[2],
                in("x13") args[3],
                in("x14") args[4],
                in("x15") args[5],
                in("x17") id
            );
        }
        ret
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        __syscall(id, args[0], args[1], args[2], args[3], args[4], args[5])
    }
}

pub fn sys_dup(fd: usize) -> isize {
    syscall(SYSCALL_DUP, [fd, 0, 0])
}

pub fn sys_open(path: &str, flags: u32) -> isize {
    syscall(SYSCALL_OPEN, [path.as_ptr() as usize, flags as usize, 0])
}

pub fn sys_close(fd: usize) -> isize {
    syscall(SYSCALL_CLOSE, [fd, 0, 0])
}

pub fn sys_mount(
    source: *const u8,
    target: *const u8,
    fstype: *const u8,
    flags: usize,
    data: usize,
) -> isize {
    syscall6(
        SYSCALL_MOUNT,
        [source as usize, target as usize, fstype as usize, flags, data, 0],
    )
}

pub fn sys_umount2(target: *const u8, flags: u32) -> isize {
    syscall(SYSCALL_UMOUNT2, [target as usize, flags as usize, 0])
}

pub fn sys_pipe(pipe: &mut [i32]) -> isize {
    syscall(SYSCALL_PIPE, [pipe.as_mut_ptr() as usize, 0, 0])
}

pub fn sys_read(fd: usize, buffer: &mut [u8]) -> isize {
    syscall(
        SYSCALL_READ,
        [fd, buffer.as_mut_ptr() as usize, buffer.len()],
    )
}

pub fn sys_write(fd: usize, buffer: &[u8]) -> isize {
    syscall(SYSCALL_WRITE, [fd, buffer.as_ptr() as usize, buffer.len()])
}

pub fn sys_ioctl(fd: usize, cmd: u32, arg: usize) -> isize {
    syscall(SYSCALL_IOCTL, [fd, cmd as usize, arg])
}

pub fn sys_exit(exit_code: i32) -> ! {
    syscall(SYSCALL_EXIT, [exit_code as usize, 0, 0]);
    panic!("sys_exit never returns!");
}

pub fn sys_yield() -> isize {
    syscall(SYSCALL_YIELD, [0, 0, 0])
}

pub fn sys_get_time() -> isize {
    syscall(SYSCALL_GET_TIME, [0, 0, 0])
}

pub fn sys_getpid() -> isize {
    syscall(SYSCALL_GETPID, [0, 0, 0])
}

pub fn sys_fork() -> isize {
    const SIGCHLD: usize = 17;
    syscall(SYSCALL_CLONE, [SIGCHLD, 0, 0])
}

pub fn sys_vfork() -> isize {
    const CLONE_VM: usize = 0x00000100;
    const CLONE_VFORK: usize = 0x00004000;
    const SIGCHLD: usize = 17;
    syscall(SYSCALL_CLONE, [CLONE_VM | CLONE_VFORK | SIGCHLD, 0, 0])
}

pub fn sys_exec(path: &str, args: &[*const u8], envp: &[*const u8]) -> isize {
    syscall(
        SYSCALL_EXECVE,
        [
            path.as_ptr() as usize,
            args.as_ptr() as usize,
            envp.as_ptr() as usize,
        ],
    )
}

pub fn sys_chdir(path: &str) -> isize {
    syscall(SYSCALL_CHDIR, [path.as_ptr() as usize, 0, 0])
}

pub fn sys_waitpid(pid: isize, exit_code: *mut i32) -> isize {
    syscall(SYSCALL_WAIT4, [pid as usize, exit_code as usize, 0])
}

pub fn sys_waitpid_flags(pid: isize, exit_code: *mut i32, options: usize) -> isize {
    syscall(SYSCALL_WAIT4, [pid as usize, exit_code as usize, options])
}

pub fn sys_kill(pid: usize, sig: usize) -> isize {
    syscall(SYSCALL_KILL, [pid, sig, 0])
}

pub fn sys_setpgid(pid: usize, pgid: usize) -> isize {
    syscall(SYSCALL_SETPGID, [pid, pgid, 0])
}

pub fn sys_getpgid(pid: usize) -> isize {
    syscall(SYSCALL_GETPGID, [pid, 0, 0])
}

pub fn sys_shutdown() -> isize {
    syscall(SYSCALL_SHUTDOWN, [0, 0, 0])
}

// === Socket syscall wrappers ===
// 系统调用号定义（与内核 os/src/syscall/syscall_id.rs 保持一致）
pub const SYSCALL_SOCKET: usize = 198;
pub const SYSCALL_SOCKETPAIR: usize = 199;
pub const SYSCALL_BIND: usize = 200;
pub const SYSCALL_LISTEN: usize = 201;
pub const SYSCALL_ACCEPT: usize = 202;
pub const SYSCALL_CONNECT: usize = 203;
pub const SYSCALL_GETSOCKNAME: usize = 204;
pub const SYSCALL_GETPEERNAME: usize = 205;
pub const SYSCALL_SENDTO: usize = 206;
pub const SYSCALL_RECVFROM: usize = 207;
pub const SYSCALL_SETSOCKOPT: usize = 208;
pub const SYSCALL_GETSOCKOPT: usize = 209;
pub const SYSCALL_SOCK_SHUTDOWN: usize = 210;
pub const SYSCALL_SENDMSG: usize = 211;
pub const SYSCALL_RECVMSG: usize = 212;
pub const SYSCALL_ACCEPT4: usize = 242;

pub fn sys_socket(domain: usize, type_: usize, protocol: usize) -> isize {
    syscall(SYSCALL_SOCKET, [domain, type_, protocol])
}

pub fn sys_socketpair(domain: usize, type_: usize, protocol: usize, sv: *mut i32) -> isize {
    syscall4(SYSCALL_SOCKETPAIR, [domain, type_, protocol, sv as usize])
}

pub fn sys_bind(sockfd: usize, addr: *const u8, addrlen: usize) -> isize {
    syscall(SYSCALL_BIND, [sockfd, addr as usize, addrlen])
}

pub fn sys_listen(sockfd: usize, backlog: usize) -> isize {
    syscall(SYSCALL_LISTEN, [sockfd, backlog, 0])
}

pub fn sys_accept(sockfd: usize, addr: *mut u8, addrlen: *mut usize) -> isize {
    syscall(SYSCALL_ACCEPT, [sockfd, addr as usize, addrlen as usize])
}

pub fn sys_connect(sockfd: usize, addr: *const u8, addrlen: usize) -> isize {
    syscall(SYSCALL_CONNECT, [sockfd, addr as usize, addrlen])
}

pub fn sys_getsockname(sockfd: usize, addr: *mut u8, addrlen: *mut usize) -> isize {
    syscall(
        SYSCALL_GETSOCKNAME,
        [sockfd, addr as usize, addrlen as usize],
    )
}

pub fn sys_getpeername(sockfd: usize, addr: *mut u8, addrlen: *mut usize) -> isize {
    syscall(
        SYSCALL_GETPEERNAME,
        [sockfd, addr as usize, addrlen as usize],
    )
}

pub fn sys_sendto(
    sockfd: usize,
    buf: *const u8,
    len: usize,
    flags: usize,
    dest_addr: *const u8,
    addrlen: usize,
) -> isize {
    syscall6(
        SYSCALL_SENDTO,
        [
            sockfd,
            buf as usize,
            len,
            flags,
            dest_addr as usize,
            addrlen,
        ],
    )
}

pub fn sys_recvfrom(
    sockfd: usize,
    buf: *mut u8,
    len: usize,
    flags: usize,
    src_addr: *mut u8,
    addrlen: *mut usize,
) -> isize {
    syscall6(
        SYSCALL_RECVFROM,
        [
            sockfd,
            buf as usize,
            len,
            flags,
            src_addr as usize,
            addrlen as usize,
        ],
    )
}

pub fn sys_setsockopt(
    sockfd: usize,
    level: usize,
    optname: usize,
    optval: *const u8,
    optlen: usize,
) -> isize {
    syscall6(
        SYSCALL_SETSOCKOPT,
        [sockfd, level, optname, optval as usize, optlen, 0],
    )
}

pub fn sys_getsockopt(
    sockfd: usize,
    level: usize,
    optname: usize,
    optval: *mut u8,
    optlen: *mut usize,
) -> isize {
    syscall6(
        SYSCALL_GETSOCKOPT,
        [sockfd, level, optname, optval as usize, optlen as usize, 0],
    )
}

pub fn sys_sock_shutdown(sockfd: usize, how: usize) -> isize {
    syscall(SYSCALL_SOCK_SHUTDOWN, [sockfd, how, 0])
}

pub fn sys_getdents64(fd: usize, buf: &mut [u8]) -> isize {
    syscall(SYSCALL_GETDENTS64, [fd, buf.as_mut_ptr() as usize, buf.len()])
}

/// AT_FDCWD — use current working directory
pub const AT_FDCWD: isize = -100;

// ── Stat struct (must match kernel fs/layout.rs:Stat exactly) ────────────

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TimeSpec {
    pub tv_sec: usize,
    pub tv_nsec: usize,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TimerFdSpec {
    pub it_interval: TimeSpec,
    pub it_value: TimeSpec,
}

pub fn sys_timerfd_create(clock_id: usize, flags: u32) -> isize {
    syscall(SYSCALL_TIMERFD_CREATE, [clock_id, flags as usize, 0])
}

pub fn sys_timerfd_settime(
    fd: usize,
    flags: u32,
    new_value: *const TimerFdSpec,
    old_value: *mut TimerFdSpec,
) -> isize {
    syscall4(
        SYSCALL_TIMERFD_SETTIME,
        [fd, flags as usize, new_value as usize, old_value as usize],
    )
}

pub fn sys_timerfd_gettime(fd: usize, curr_value: *mut TimerFdSpec) -> isize {
    syscall(SYSCALL_TIMERFD_GETTIME, [fd, curr_value as usize, 0])
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ITimerSpec {
    pub it_interval: TimeSpec,
    pub it_value: TimeSpec,
}

pub fn sys_timer_create(clock_id: usize, sevp: *const u8, timerid: *mut i32) -> isize {
    syscall(
        SYSCALL_TIMER_CREATE,
        [clock_id, sevp as usize, timerid as usize],
    )
}

pub fn sys_timer_settime(
    timer_id: usize,
    flags: u32,
    new_value: *const ITimerSpec,
    old_value: *mut ITimerSpec,
) -> isize {
    syscall4(
        SYSCALL_TIMER_SETTIME,
        [timer_id, flags as usize, new_value as usize, old_value as usize],
    )
}

pub fn sys_timer_gettime(timer_id: usize, curr_value: *mut ITimerSpec) -> isize {
    syscall(SYSCALL_TIMER_GETTIME, [timer_id, curr_value as usize, 0])
}

pub fn sys_timer_delete(timer_id: usize) -> isize {
    syscall(SYSCALL_TIMER_DELETE, [timer_id, 0, 0])
}

pub fn sys_clock_gettime(clock_id: usize, tp: *mut TimeSpec) -> isize {
    syscall(SYSCALL_CLOCK_GETTIME, [clock_id, tp as usize, 0])
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub __pad: u64,
    pub st_size: i64,
    pub st_blksize: u32,
    pub __pad2: i32,
    pub st_blocks: u64,
    pub st_atime: TimeSpec,
    pub st_mtime: TimeSpec,
    pub st_ctime: TimeSpec,
    pub __unused: u64,
}

pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

pub const AT_EMPTY_PATH: u32 = 0x1000;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;

// ── Wrappers ────────────────────────────────────────────────────────────

pub fn sys_mkdirat(dirfd: isize, path: &str, mode: u32) -> isize {
    syscall(SYSCALL_MKDIRAT, [dirfd as usize, path.as_ptr() as usize, mode as usize])
}

pub fn sys_symlinkat(target: &str, newdirfd: isize, linkpath: &str) -> isize {
    syscall(SYSCALL_SYMLINKAT, [target.as_ptr() as usize, newdirfd as usize, linkpath.as_ptr() as usize])
}

pub fn sys_readlinkat(dirfd: isize, path: &str, buf: &mut [u8]) -> isize {
    syscall4(SYSCALL_READLINKAT, [dirfd as usize, path.as_ptr() as usize, buf.as_mut_ptr() as usize, buf.len()])
}

pub fn sys_unlinkat(dirfd: isize, path: &str, flags: u32) -> isize {
    syscall(SYSCALL_UNLINKAT, [dirfd as usize, path.as_ptr() as usize, flags as usize])
}

pub fn sys_linkat(olddirfd: isize, oldpath: &str, newdirfd: isize, newpath: &str, flags: u32) -> isize {
    syscall6(
        SYSCALL_LINKAT,
        [
            olddirfd as usize,
            oldpath.as_ptr() as usize,
            newdirfd as usize,
            newpath.as_ptr() as usize,
            flags as usize,
            0,
        ],
    )
}

pub fn sys_renameat2(
    olddirfd: isize,
    oldpath: &str,
    newdirfd: isize,
    newpath: &str,
    flags: u32,
) -> isize {
    syscall6(
        SYSCALL_RENAMEAT2,
        [
            olddirfd as usize,
            oldpath.as_ptr() as usize,
            newdirfd as usize,
            newpath.as_ptr() as usize,
            flags as usize,
            0,
        ],
    )
}

pub fn sys_lseek(fd: usize, offset: isize, whence: u32) -> isize {
    syscall(SYSCALL_LSEEK, [fd, offset as usize, whence as usize])
}

pub fn sys_fstatat(dirfd: isize, path: &str, buf: &mut Stat, flags: u32) -> isize {
    syscall4(
        SYSCALL_NEW_FSTATAT,
        [
            dirfd as usize,
            path.as_ptr() as usize,
            buf as *mut Stat as usize,
            flags as usize,
        ],
    )
}

pub fn sys_fstat(fd: usize, buf: &mut Stat) -> isize {
    syscall(SYSCALL_FSTAT, [fd, buf as *mut Stat as usize, 0])
}

pub fn sys_ftruncate(fd: usize, length: isize) -> isize {
    syscall(SYSCALL_FTRUNCATE, [fd, length as usize, 0])
}

pub fn sys_sync() -> isize {
    syscall(SYSCALL_SYNC, [0, 0, 0])
}

pub fn sys_fsync(fd: usize) -> isize {
    syscall(SYSCALL_FSYNC, [fd, 0, 0])
}

pub fn sys_ext4_counters(cmd: usize, arg1: usize, arg2: usize) -> isize {
    syscall(SYSCALL_EXT4_COUNTERS, [cmd, arg1, arg2])
}

pub fn sys_faccessat2(dirfd: isize, path: &str, mode: u32, flags: u32) -> isize {
    syscall4(
        SYSCALL_FACCESSAT2,
        [
            dirfd as usize,
            path.as_ptr() as usize,
            mode as usize,
            flags as usize,
        ],
    )
}

pub fn sys_clock_settime(clock_id: usize, tp: *const TimeSpec) -> isize {
    syscall(SYSCALL_CLOCK_SETTIME, [clock_id, tp as usize, 0])
}

pub fn sys_clock_nanosleep(
    clock_id: usize,
    flags: u32,
    req: *const TimeSpec,
    rem: *mut TimeSpec,
) -> isize {
    syscall4(
        SYSCALL_CLOCK_NANOSLEEP,
        [clock_id, flags as usize, req as usize, rem as usize],
    )
}
