#[macro_use]
mod syscall_macro;

pub mod errno;
pub mod fs;
mod process;
mod syscall_id;
pub mod utils;

use crate::fs::eventfd::sys_eventfd2;
use crate::fs::eventpoll::{sys_epoll_create1, sys_epoll_ctl, sys_epoll_pwait};
use crate::fs::iov::IOVec;
use crate::net::syscall::*;
use core::convert::TryFrom;
use fs::*;
use log::{error, info};
pub use process::CloneFlags;
use process::*;
use syscall_id::*;
pub fn syscall_name(id: usize) -> &'static str {
    match id {
        SYSCALL_DUP => "dup",
        SYSCALL_DUP3 => "dup3",
        SYSCALL_EVENTFD2 => "eventfd2",
        SYSCALL_EPOLL_CREATE1 => "epoll_create1",
        SYSCALL_EPOLL_CTL => "epoll_ctl",
        SYSCALL_EPOLL_PWAIT => "epoll_pwait",
        SYSCALL_OPEN => "open",
        SYSCALL_GET_TIME => "get_time",
        SYSCALL_GETCWD => "getcwd",
        SYSCALL_FCNTL => "fcntl",
        SYSCALL_IOCTL => "ioctl",
        SYSCALL_IOPRIO_SET => "ioprio_set",
        SYSCALL_IOPRIO_GET => "ioprio_get",
        SYSCALL_MKDIRAT => "mkdirat",
        SYSCALL_FLOCK => "flock",
        SYSCALL_MKNODAT => "mknodat",
        SYSCALL_UNLINKAT => "unlinkat",
        SYSCALL_LINKAT => "linkat",
        SYSCALL_SYMLINKAT => "symlinkat",
        SYSCALL_UMOUNT2 => "umount2",
        SYSCALL_MOUNT => "mount",
        SYSCALL_FACCESSAT => "faccessat",
        SYSCALL_CHDIR => "chdir",
        SYSCALL_FCHDIR => "fchdir",
        SYSCALL_CHMOD => "chmod",
        SYSCALL_FCHMOD => "fchmod",
        SYSCALL_FCHMODAT => "fchmodat",
        SYSCALL_FCHOWNAT => "fchownat",
        SYSCALL_OPENAT => "openat",
        SYSCALL_CLOSE => "close",
        SYSCALL_CLOSE_RANGE => "close_range",
        SYSCALL_PIPE2 => "pipe2",
        SYSCALL_GETDENTS64 => "getdents64",
        SYSCALL_LSEEK => "lseek",
        SYSCALL_READ => "read",
        SYSCALL_WRITE => "write",
        SYSCALL_READV => "readv",
        SYSCALL_WRITEV => "writev",
        SYSCALL_PREAD => "pread",
        SYSCALL_PWRITE => "pwrite",
        SYSCALL_SENDFILE => "sendfile",
        SYSCALL_COPY_FILE_RANGE => "copy_file_range",
        SYSCALL_PSELECT6 => "pselect6",
        SYSCALL_PPOLL => "ppoll",
        SYSCALL_SIGNALFD4 => "signalfd4",
        SYSCALL_READLINKAT => "readlinkat",
        SYSCALL_FSTATAT => "fstatat",
        SYSCALL_FSTAT => "fstat",
        SYSCALL_SYNC => "sync",
        SYSCALL_STATFS => "statfs",
        SYSCALL_TRUNCATE => "truncate",
        SYSCALL_FTRUNCATE => "ftruncate",
        SYSCALL_FALLOCATE => "fallocate",
        SYSCALL_FSYNC => "fsync",
        SYSCALL_UTIMENSAT => "utimensat",
        SYSCALL_CAPGET => "capget",
        SYSCALL_CAPSET => "capset",
        SYSCALL_PERSONALITY => "personality",
        SYSCALL_EXIT => "exit",
        SYSCALL_EXIT_GROUP => "exit_GROUP",
        SYSCALL_WAITID => "waitid",
        SYSCALL_SET_TID_ADDRESS => "set_tid_address",
        SYSCALL_UNSHARE => "unshare",
        SYSCALL_FUTEX => "futex",
        SYSCALL_SET_ROBUST_LIST => "set_robust_list",
        SYSCALL_GET_ROBUST_LIST => "get_robust_list",
        SYSCALL_NANOSLEEP => "nanosleep",
        SYSCALL_DELETE_MODULE => "delete_module",
        SYSCALL_GETITIMER => "getitimer",
        SYSCALL_SETITIMER => "setitimer",
        SYSCALL_TIMER_CREATE => "timer_create",
        SYSCALL_TIMER_GETTIME => "timer_gettime",
        SYSCALL_TIMER_GETOVERRUN => "timer_getoverrun",
        SYSCALL_TIMER_SETTIME => "timer_settime",
        SYSCALL_TIMER_DELETE => "timer_delete",
        SYSCALL_CLOCK_SETTIME => "clock_settime",
        SYSCALL_CLOCK_GETTIME => "clock_gettime",
        SYSCALL_CLOCK_GETRES => "clock_getres",
        SYSCALL_CLOCK_NANOSLEEP => "clock_nanosleep",
        SYSCALL_SYSLOG => "syslog",
        SYSCALL_YIELD => "yield",
        SYSCALL_KILL => "kill",
        SYSCALL_TKILL => "tkill",
        SYSCALL_TGKILL => "tgkill",
        SYSCALL_SIGALTSTACK => "sigaltstack",
        SYSCALL_RT_SIGSUSPEND => "rt_sigsuspend",
        SYSCALL_SIGACTION => "sigaction",
        SYSCALL_SIGPROCMASK => "sigprocmask",
        SYSCALL_RT_SIGPENDING => "rt_sigpending",
        SYSCALL_SIGTIMEDWAIT => "sigtimedwait",
        SYSCALL_RT_SIGQUEUEINFO => "rt_sigqueueinfo",
        SYSCALL_KCMP => "kcmp",
        SYSCALL_REBOOT => "reboot",
        SYSCALL_PIDFD_SEND_SIGNAL => "pidfd_send_signal",
        SYSCALL_PIDFD_OPEN => "pidfd_open",
        SYSCALL_PIDFD_GETFD => "pidfd_getfd",
        SYSCALL_SIGRETURN => "sigreturn",
        SYSCALL_SETREGID => "setregid",
        SYSCALL_SETGID => "setgid",
        SYSCALL_SETREUID => "setreuid",
        SYSCALL_SETUID => "setuid",
        SYSCALL_SETRESUID => "setresuid",
        SYSCALL_GETRESUID => "getresuid",
        SYSCALL_SETRESGID => "setresgid",
        SYSCALL_GETRESGID => "getresgid",
        SYSCALL_SETFSUID => "setfsuid",
        SYSCALL_SETFSGID => "setfsgid",
        SYSCALL_TIMES => "times",
        SYSCALL_SETPGID => "setpgid",
        SYSCALL_GETPGID => "getpgid",
        SYSCALL_GETSID => "getsid",
        SYSCALL_SETSID => "setsid",
        SYSCALL_GETGROUPS => "getgroups",
        SYSCALL_SETGROUPS => "setgroups",
        SYSCALL_UNAME => "uname",
        SYSCALL_SETHOSTNAME => "sethostname",
        SYSCALL_SETDOMAINNAME => "setdomainname",
        SYSCALL_GETRUSAGE => "getrusage",
        SYSCALL_UMASK => "umask",
        SYSCALL_PRCTL => "prctl",
        SYSCALL_GETCPU => "getcpu",
        SYSCALL_GET_TIME_OF_DAY => "get_time_of_day",
        SYSCALL_SET_TIME_OF_DAY => "set_time_of_day",
        SYSCALL_ADJTIMEX => "adjtimex",
        SYSCALL_GETPID => "getpid",
        SYSCALL_GETPPID => "getppid",
        SYSCALL_GETUID => "getuid",
        SYSCALL_GETEUID => "geteuid",
        SYSCALL_GETGID => "getgid",
        SYSCALL_GETEGID => "getegid",
        SYSCALL_GETTID => "gettid",
        SYSCALL_SETPRIORITY => "setpriority",
        SYSCALL_GETPRIORITY => "getpriority",
        SYSCALL_SYSINFO => "sysinfo",
        SYSCALL_MSGGET => "msgget",
        SYSCALL_MSGCTL => "msgctl",
        SYSCALL_MSGRCV => "msgrcv",
        SYSCALL_MSGSND => "msgsnd",
        SYSCALL_SEMGET => "semget",
        SYSCALL_SEMCTL => "semctl",
        SYSCALL_SEMTIMEDOP => "semtimedop",
        SYSCALL_SEMOP => "semop",
        SYSCALL_SHMGET => "shmget",
        SYSCALL_SHMCTL => "shmctl",
        SYSCALL_SHMAT => "shmat",
        SYSCALL_SHMDT => "shmdt",
        SYSCALL_SOCKET => "socket",
        SYSCALL_SOCKETPAIR => "socketpair",
        SYSCALL_SOCK_SHUTDOWN => "sock_shutdown",
        SYSCALL_BIND => "bind",
        SYSCALL_LISTEN => "listen",
        SYSCALL_ACCEPT => "accept",
        SYSCALL_CONNECT => "connect",
        SYSCALL_ACCEPT4 => "accept4",
        SYSCALL_GETSOCKNAME => "getsockname",
        SYSCALL_GETPEERNAME => "getpeername",
        SYSCALL_SENDTO => "sendto",
        SYSCALL_RECVFROM => "recvfrom",
        SYSCALL_SETSOCKOPT => "setsockopt",
        SYSCALL_GETSOCKOPT => "getsockopt",
        SYSCALL_SBRK => "sbrk",
        SYSCALL_BRK => "brk",
        SYSCALL_MUNMAP => "munmap",
        SYSCALL_MREMAP => "mremap",
        SYSCALL_CLONE => "clone",
        SYSCALL_EXECVE => "execve",
        SYSCALL_MMAP => "mmap",
        SYSCALL_MPROTECT => "mprotect",
        SYSCALL_MSYNC => "msync",
        SYSCALL_MLOCK => "mlock",
        SYSCALL_MUNLOCK => "munlock",
        SYSCALL_MLOCKALL => "mlockall",
        SYSCALL_MUNLOCKALL => "munlockall",
        SYSCALL_MINCORE => "mincore",
        SYSCALL_MLOCK2 => "mlock2",
        SYSCALL_GETRLIMIT => "getrlimit",
        SYSCALL_SETRLIMIT => "setrlimit",
        SYSCALL_WAIT4 => "wait4",
        SYSCALL_PRLIMIT => "prlimit",
        SYSCALL_PROCESS_VM_READV => "process_vm_readv",
        SYSCALL_PROCESS_VM_WRITEV => "process_vm_writev",
        SYSCALL_RENAMEAT2 => "renameat2",
        SYSCALL_FACCESSAT2 => "faccessat2",
        SYSCALL_MEMBARRIER => "membarrier",
        SYSCALL_STATX => "statx",
        SYSCALL_SYNCFS => "syncfs",
        SYSCALL_GETRANDOM => "getrandom",
        SYSCALL_MADVISE => "madvise",
        SYSCALL_CLONE3 => "clone3",
        SYSCALL_GET_MEMPOLICY => "get_mempolicy",
        SYSCALL_CLOCK_ADJTIME => "clock_adjtime",
        SYSCALL_SCHED_SETPARAM => "sched_setparam",
        SYSCALL_SCHED_SETSCHEDULER => "sched_setscheduler",
        SYSCALL_SCHED_GETSCHEDULER => "sched_getscheduler",
        SYSCALL_SCHED_GETPARAM => "sched_getparam",
        SYSCALL_SCHED_SETAFFINITY => "sched_setaffinity",
        SYSCALL_SCHED_GETAFFINITY => "sched_getaffinity",
        SYSCALL_SCHED_GET_PRIORITY_MAX => "sched_get_priority_max",
        SYSCALL_SCHED_GET_PRIORITY_MIN => "sched_get_priority_min",
        SYSCALL_SCHED_RR_GET_INTERVAL => "sched_rr_get_interval",
        SYSCALL_SCHED_SETATTR => "sched_setattr",
        SYSCALL_SCHED_GETATTR => "sched_getattr",
        // non-standard
        SYSCALL_LS => "ls",
        SYSCALL_SHUTDOWN => "shutdown",
        SYSCALL_CLEAR => "clear",
        SYSCALL_EXT4_COUNTERS => "ext4_counters",
        _ => "unknown",
    }
}
use crate::{
    fs::poll::FdSet,
    mm::{translated_byte_buffer, UserAccess, UserBuffer},
    syscall::errno::Errno,
    task::{current_user_token, Rusage},
    timer::{ITimerVal, TimeSpec, TimeVal, Times},
};

pub fn syscall(syscall_id: usize, args: [usize; 6]) -> isize {
    crate::trace_event!(syscall_id, args[0], args[1], args[2], args[3], args[4], args[5]);
    // 记录当前系统调用 ID，供 OOM 诊断使用
    crate::task::set_current_syscall_id(Some(syscall_id));
    let mut show_info = true;
    if option_env!("LOG").is_some()
        && ![
            //black list
            SYSCALL_YIELD,
            // SYSCALL_READ,
            SYSCALL_WRITE,
            SYSCALL_GETDENTS64,
            SYSCALL_READV,
            SYSCALL_WRITEV,
            SYSCALL_PSELECT6,
            SYSCALL_SIGACTION,
            SYSCALL_SIGPROCMASK,
            // SYSCALL_WAIT4,
            // SYSCALL_GETPPID,
            SYSCALL_CLOCK_GETTIME,
        ]
        .contains(&syscall_id)
    {
        show_info = false;
        log::info!(
            "[syscall] {}({}) args: [{:X}, {:X}, {:X}, {:X}, {:X}, {:X}]",
            syscall_name(syscall_id),
            syscall_id,
            args[0],
            args[1],
            args[2],
            args[3],
            args[4],
            args[5],
        );
    }
    let ret = match syscall_id {
        SYSCALL_GETCWD => sys_getcwd(args[0], args[1]),
        SYSCALL_DUP => sys_dup(args[0]),
        SYSCALL_DUP3 => sys_dup3(args[0], args[1], args[2] as u32),
        SYSCALL_EVENTFD2 => sys_eventfd2(args[0] as u32, args[1] as u32),
        SYSCALL_EPOLL_CREATE1 => sys_epoll_create1(args[0]),
        SYSCALL_EPOLL_CTL => sys_epoll_ctl(
            args[0],
            args[1],
            args[2],
            args[3] as *const crate::fs::eventpoll::EpollUserEvent,
        ),
        SYSCALL_EPOLL_PWAIT => sys_epoll_pwait(
            args[0],
            args[1] as *mut crate::fs::eventpoll::EpollUserEvent,
            args[2] as isize,
            args[3] as isize,
            args[4] as *const crate::task::signal::Signals,
        ),
        SYSCALL_FCNTL => sys_fcntl(args[0], args[1] as u32, args[2]),
        SYSCALL_IOCTL => sys_ioctl(args[0], args[1] as u32, args[2]),
        SYSCALL_IOPRIO_SET => sys_ioprio_set(args[0], args[1], args[2]),
        SYSCALL_IOPRIO_GET => sys_ioprio_get(args[0], args[1]),
        SYSCALL_MKDIRAT => sys_mkdirat(args[0], args[1] as *const u8, args[2] as u32),
        SYSCALL_FLOCK => sys_flock(args[0], args[1] as u32),
        SYSCALL_MKNODAT => sys_mknodat(args[0], args[1] as *const u8, args[2] as u32, args[3]),
        SYSCALL_UNLINKAT => sys_unlinkat(args[0], args[1] as *const u8, args[2] as u32),
        SYSCALL_SYMLINKAT => sys_symlinkat(args[0] as *const u8, args[1], args[2] as *const u8),
        SYSCALL_LINKAT => sys_linkat(args[0], args[1] as *const u8, args[2], args[3] as *const u8, args[4] as u32),
        SYSCALL_UMOUNT2 => sys_umount2(args[0] as *const u8, args[1] as u32),
        SYSCALL_MOUNT => sys_mount(
            args[0] as *const u8,
            args[1] as *const u8,
            args[2] as *const u8,
            args[3],
            args[4] as *const u8,
        ),
        SYSCALL_FACCESSAT => sys_faccessat2(args[0], args[1] as *const u8, args[2] as u32, 0u32),
        SYSCALL_CHDIR => sys_chdir(args[0] as *const u8),
        SYSCALL_FCHDIR => sys_fchdir(args[0]),
        SYSCALL_CHMOD => sys_chmod(args[0] as *const u8, args[1] as u32),
        SYSCALL_FCHMOD => sys_fchmod(args[0], args[1] as u32),
        SYSCALL_FCHMODAT => sys_fchmodat(args[0], args[1] as *const u8, args[2] as u32, args[3] as u32),
        SYSCALL_FCHOWNAT => sys_fchownat(
            args[0],
            args[1] as *const u8,
            args[2] as u32,
            args[3] as u32,
            args[4] as u32,
        ),
        SYSCALL_OPEN => sys_openat(AT_FDCWD, args[0] as *const u8, args[1] as u32, 0o777u32),
        SYSCALL_OPENAT => sys_openat(
            args[0],
            args[1] as *const u8,
            args[2] as u32,
            args[3] as u32,
        ),
        SYSCALL_CLOSE => sys_close(args[0]),
        SYSCALL_CLOSE_RANGE => sys_close_range(args[0], args[1], args[2] as u32),
        SYSCALL_PIPE2 => sys_pipe2(args[0], args[1] as u32),
        SYSCALL_GETDENTS64 => sys_getdents64(args[0], args[1] as *mut u8, args[2]),
        SYSCALL_READ => sys_read(args[0], args[1], args[2]),
        SYSCALL_READV => sys_readv(args[0], args[1], args[2]),
        SYSCALL_PREAD => sys_pread(args[0], args[1], args[2], args[3]),
        SYSCALL_WRITE => sys_write(args[0], args[1], args[2]),
        SYSCALL_WRITEV => sys_writev(args[0], args[1], args[2]),
        SYSCALL_PWRITE => sys_pwrite(args[0], args[1], args[2], args[3]),
        SYSCALL_LSEEK => sys_lseek(args[0], args[1] as isize, args[2] as u32),
        SYSCALL_SENDFILE => sys_sendfile(args[0], args[1], args[2] as *mut usize, args[3]),
        SYSCALL_COPY_FILE_RANGE => sys_copy_file_range(
            args[0],
            args[1] as *mut usize,
            args[2],
            args[3] as *mut usize,
            args[4],
            args[5] as u32,
        ),
        SYSCALL_SPLICE => sys_splice(
            args[0],
            args[1] as *mut usize,
            args[2],
            args[3] as *mut usize,
            args[4],
            args[5] as u32,
        ),
        SYSCALL_READLINKAT => {
            sys_readlinkat(args[0], args[1] as *const u8, args[2] as *mut u8, args[3])
        }
        SYSCALL_FSTATAT => sys_fstatat(
            args[0],
            args[1] as *const u8,
            args[2] as *mut u8,
            args[3] as u32,
        ),
        SYSCALL_FSTAT => sys_fstat(args[0], args[1] as *mut u8),
        SYSCALL_TRUNCATE => sys_truncate(args[0] as *const u8, args[1] as isize),
        SYSCALL_FTRUNCATE => sys_ftruncate(args[0], args[1] as isize),
        SYSCALL_FALLOCATE => {
            sys_fallocate(args[0], args[1] as u32, args[2] as isize, args[3] as isize)
        }
        SYSCALL_SYNC => sys_sync(),
        SYSCALL_FSYNC => sys_fsync(args[0]),
        SYSCALL_UTIMENSAT => sys_utimensat(
            args[0],
            args[1] as *const u8,
            args[2] as *const [TimeSpec; 2],
            args[3] as u32,
        ),
        SYSCALL_CAPGET => sys_capget(args[0] as *mut CapUserHeader, args[1] as *mut CapUserData),
        SYSCALL_CAPSET => {
            sys_capset(args[0] as *mut CapUserHeader, args[1] as *const CapUserData)
        }
        SYSCALL_PERSONALITY => sys_personality(args[0]),
        SYSCALL_EXIT => sys_exit(args[0] as u32),
        SYSCALL_EXIT_GROUP => sys_exit_group(args[0] as u32),
        SYSCALL_WAITID => sys_waitid(
            args[0],
            args[1],
            args[2],
            args[3] as u32,
            args[4] as *mut Rusage,
        ),
        SYSCALL_CLOCK_SETTIME => sys_clock_settime(args[0], args[1] as *const TimeSpec),
        SYSCALL_CLOCK_GETTIME => sys_clock_gettime(args[0], args[1] as *mut TimeSpec),
        SYSCALL_CLOCK_GETRES => sys_clock_getres(args[0], args[1] as *mut TimeSpec),
        SYSCALL_CLOCK_NANOSLEEP => sys_clock_nanosleep(
            args[0] as usize,
            args[1] as u32,
            args[2] as *const TimeSpec,
            args[3] as *mut TimeSpec,
        ),
        SYSCALL_KILL => sys_kill(args[0], args[1]),
        SYSCALL_TKILL => sys_tkill(args[0], args[1]),
        SYSCALL_TGKILL => sys_tgkill(args[0], args[1], args[2]),
        SYSCALL_SYSLOG => sys_syslog(args[0] as u32, args[1] as *mut u8, args[2] as isize),
        SYSCALL_YIELD => sys_yield(),
        SYSCALL_SIGALTSTACK => sys_sigaltstack(args[0], args[1]),
        SYSCALL_SIGACTION => sys_sigaction(args[0], args[1], args[2], args[3]),
        SYSCALL_SIGPROCMASK => sys_sigprocmask(args[0] as u32, args[1], args[2], args[3]),
        SYSCALL_RT_SIGPENDING => sys_rt_sigpending(args[0], args[1]),
        SYSCALL_RT_SIGSUSPEND => sys_rt_sigsuspend(args[0], args[1]),
        SYSCALL_SIGTIMEDWAIT => sys_sigtimedwait(args[0], args[1], args[2], args[3]),
        SYSCALL_RT_SIGQUEUEINFO => sys_rt_sigqueueinfo(args[0], args[1], args[2]),
        SYSCALL_SIGNALFD4 => sys_signalfd4(args[0], args[1], args[2], args[3]),
        SYSCALL_KCMP => sys_kcmp(args[0], args[1], args[2], args[3], args[4]),
        SYSCALL_REBOOT => sys_reboot(args[0], args[1], args[2], args[3]),
        SYSCALL_PIDFD_SEND_SIGNAL => sys_pidfd_send_signal(args[0], args[1], args[2], args[3]),
        SYSCALL_PIDFD_OPEN => sys_pidfd_open(args[0], args[1]),
        SYSCALL_PIDFD_GETFD => sys_pidfd_getfd(args[0], args[1], args[2]),
        SYSCALL_SIGRETURN => sys_sigreturn(),
        SYSCALL_TIMES => sys_times(args[0] as *mut Times),
        SYSCALL_NANOSLEEP => sys_nanosleep(
            args[0] as *const crate::timer::TimeSpec,
            args[1] as *mut crate::timer::TimeSpec,
        ),
        SYSCALL_GETITIMER => sys_getitimer(args[0], args[1] as *mut ITimerVal),
        SYSCALL_SETITIMER => sys_setitimer(
            args[0],
            args[1] as *const ITimerVal,
            args[2] as *mut ITimerVal,
        ),
        SYSCALL_TIMER_CREATE => {
            sys_timer_create(args[0], args[1] as *const SigeventHeader, args[2] as *mut i32)
        }
        SYSCALL_TIMER_GETTIME => sys_timer_gettime(args[0], args[1] as *mut ITimerSpec),
        SYSCALL_TIMER_GETOVERRUN => sys_timer_getoverrun(args[0]),
        SYSCALL_TIMER_SETTIME => sys_timer_settime(
            args[0],
            args[1] as u32,
            args[2] as *const ITimerSpec,
            args[3] as *mut ITimerSpec,
        ),
        SYSCALL_TIMER_DELETE => sys_timer_delete(args[0]),
        SYSCALL_GET_TIME => sys_get_time(),
        SYSCALL_UNSHARE => sys_unshare(args[0] as u32),
        SYSCALL_GETRUSAGE => sys_getrusage(args[0] as isize, args[1] as *mut Rusage),
        SYSCALL_UMASK => sys_umask(args[0] as u32),
        SYSCALL_PRCTL => sys_prctl(args[0], args[1], args[2], args[3], args[4]),
        SYSCALL_GETCPU => sys_getcpu(args[0] as *mut u32, args[1] as *mut u32, args[2]),
        SYSCALL_GET_TIME_OF_DAY => sys_gettimeofday(
            args[0] as *mut crate::timer::TimeVal,
            args[1] as *mut crate::timer::TimeZone,
        ),
        SYSCALL_SET_TIME_OF_DAY => {
            sys_settimeofday(args[0] as *const TimeVal, args[1] as *const crate::timer::TimeZone)
        }
        SYSCALL_ADJTIMEX => sys_adjtimex(args[0] as *mut Timex),
        SYSCALL_CLOCK_ADJTIME => sys_clock_adjtime(args[0], args[1] as *mut Timex),
        SYSCALL_SETPGID => sys_setpgid(args[0], args[1]),
        SYSCALL_GETPGID => sys_getpgid(args[0]),
        SYSCALL_GETSID => sys_getsid(args[0]),
        SYSCALL_SETSID => sys_setsid(),
        SYSCALL_UNAME => sys_uname(args[0] as *mut u8),
        SYSCALL_SETHOSTNAME => sys_sethostname(args[0] as *const u8, args[1]),
        SYSCALL_SETDOMAINNAME => sys_setdomainname(args[0] as *const u8, args[1]),
        SYSCALL_GETPID => sys_getpid(),
        SYSCALL_GETPPID => sys_getppid(),
        #[cfg(not(feature = "loongarch64"))]
        SYSCALL_CLONE => sys_clone(
            args[0] as u32,
            args[1] as *const u8,
            args[2] as *mut u32,
            args[3],
            args[4] as *mut u32,
        ),
        #[cfg(feature = "loongarch64")]
        SYSCALL_CLONE => sys_clone(
            args[0] as u32,
            args[1] as *const u8,
            args[2] as *mut u32,
            // LoongArch raw clone ABI 为 flags, stack, ptid, ctid, tls。
            args[4],
            args[3] as *mut u32,
        ),
        SYSCALL_EXECVE => sys_execve(
            args[0] as *const u8,
            args[1] as *const *const u8,
            args[2] as *const *const u8,
        ),
        SYSCALL_WAIT4 => sys_wait4(
            args[0] as isize,
            args[1] as *mut u32,
            args[2] as u32,
            args[3] as *mut Rusage,
        ),
        SYSCALL_GETRLIMIT => sys_getrlimit(args[0] as u32, args[1] as *mut RLimit),
        SYSCALL_SETRLIMIT => sys_setrlimit(args[0] as u32, args[1] as *const RLimit),
        SYSCALL_PRLIMIT => sys_prlimit(
            args[0],
            args[1] as u32,
            args[2] as *const RLimit,
            args[3] as *mut RLimit,
        ),
        SYSCALL_PROCESS_VM_READV => sys_process_vm_readv(
            args[0],
            args[1] as *const IOVec,
            args[2],
            args[3] as *const IOVec,
            args[4],
            args[5],
        ),
        SYSCALL_PROCESS_VM_WRITEV => sys_process_vm_writev(
            args[0],
            args[1] as *const IOVec,
            args[2],
            args[3] as *const IOVec,
            args[4],
            args[5],
        ),
        SYSCALL_SET_TID_ADDRESS => sys_set_tid_address(args[0]),
        SYSCALL_CLONE3 => sys_clone3(args[0] as *const u8, args[1]),
        SYSCALL_FUTEX => sys_futex(
            args[0] as *mut u32,
            args[1] as u32,
            args[2] as u32,
            args[3] as *const TimeSpec,
            args[4] as *mut u32,
            args[5] as u32,
        ),
        SYSCALL_SET_ROBUST_LIST => sys_set_robust_list(args[0], args[1]),
        SYSCALL_GET_ROBUST_LIST => {
            sys_get_robust_list(args[0] as u32, args[1] as *mut usize, args[2] as *mut usize)
        }
        SYSCALL_GETUID => sys_getuid(),
        SYSCALL_GETEUID => sys_geteuid(),
        SYSCALL_SETUID => sys_setuid(args[0]),
        SYSCALL_SETREUID => sys_setreuid(args[0], args[1]),
        SYSCALL_SETRESUID => sys_setresuid(args[0], args[1], args[2]),
        SYSCALL_GETRESUID => {
            sys_getresuid(args[0] as *mut u32, args[1] as *mut u32, args[2] as *mut u32)
        }
        SYSCALL_GETGID => sys_getgid(),
        SYSCALL_GETEGID => sys_getegid(),
        SYSCALL_SETGID => sys_setgid(args[0]),
        SYSCALL_SETREGID => sys_setregid(args[0], args[1]),
        SYSCALL_SETRESGID => sys_setresgid(args[0], args[1], args[2]),
        SYSCALL_GETRESGID => {
            sys_getresgid(args[0] as *mut u32, args[1] as *mut u32, args[2] as *mut u32)
        }
        SYSCALL_SETFSUID => sys_setfsuid(args[0]),
        SYSCALL_SETFSGID => sys_setfsgid(args[0]),
        SYSCALL_GETGROUPS => sys_getgroups(args[0], args[1] as *mut u32),
        SYSCALL_SETGROUPS => sys_setgroups(args[0], args[1] as *const u32),
        SYSCALL_SETPRIORITY => sys_setpriority(args[0], args[1], args[2]),
        SYSCALL_GETPRIORITY => sys_getpriority(args[0], args[1]),
        SYSCALL_GETTID => sys_gettid(),
        SYSCALL_SYSINFO => sys_sysinfo(args[0] as *mut Sysinfo),
        SYSCALL_MSGGET => sys_msgget(args[0] as isize, args[1]),
        SYSCALL_MSGCTL => sys_msgctl(args[0] as i32, args[1], args[2]),
        SYSCALL_MSGRCV => sys_msgrcv(args[0] as i32, args[1], args[2], args[3] as isize, args[4]),
        SYSCALL_MSGSND => sys_msgsnd(args[0] as i32, args[1], args[2], args[3]),
        SYSCALL_SEMGET => sys_semget(args[0] as isize, args[1], args[2]),
        SYSCALL_SEMCTL => sys_semctl(args[0] as i32, args[1], args[2], args[3]),
        SYSCALL_SEMTIMEDOP => sys_semtimedop(args[0] as i32, args[1], args[2], args[3]),
        SYSCALL_SEMOP => sys_semop(args[0] as i32, args[1], args[2]),
        SYSCALL_SHMGET => sys_shmget(args[0] as isize, args[1], args[2]),
        SYSCALL_SHMCTL => sys_shmctl(args[0] as i32, args[1], args[2]),
        SYSCALL_SHMAT => sys_shmat(args[0] as i32, args[1], args[2]),
        SYSCALL_SHMDT => sys_shmdt(args[0]),
        SYSCALL_SBRK => sys_sbrk(args[0] as isize),
        SYSCALL_BRK => sys_brk(args[0]),
        SYSCALL_MMAP => sys_mmap(args[0], args[1], args[2], args[3], args[4], args[5]),
        SYSCALL_MUNMAP => sys_munmap(args[0], args[1]),
        SYSCALL_MREMAP => sys_mremap(args[0], args[1], args[2], args[3], args[4]),
        SYSCALL_MPROTECT => sys_mprotect(args[0], args[1], args[2]),
        SYSCALL_PSELECT6 => sys_pselect(
            args[0],
            args[1] as *mut FdSet,
            args[2] as *mut FdSet,
            args[3] as *mut FdSet,
            args[4] as *mut TimeSpec,
            args[5],
        ),
        SYSCALL_PPOLL => sys_ppoll(args[0], args[1], args[2], args[3]),
        SYSCALL_FACCESSAT2 => sys_faccessat2(
            args[0],
            args[1] as *const u8,
            args[2] as u32,
            args[3] as u32,
        ),
        SYSCALL_MEMBARRIER => sys_memorybarrier(args[0], args[1], args[2]),
        SYSCALL_STATX => sys_statx(
            args[0],
            args[1] as *const u8,
            args[2] as u32,
            args[3] as u32,
            args[4] as *mut u8,
        ),
        SYSCALL_RENAMEAT2 => sys_renameat2(
            args[0],
            args[1] as *const u8,
            args[2],
            args[3] as *const u8,
            args[4] as u32,
        ),
        SYSCALL_MSYNC => sys_msync(args[0], args[1], args[2] as u32),
        SYSCALL_MLOCK => sys_mlock(args[0], args[1]),
        SYSCALL_MUNLOCK => sys_munlock(args[0], args[1]),
        SYSCALL_MLOCKALL => sys_mlockall(args[0]),
        SYSCALL_MUNLOCKALL => sys_munlockall(),
        SYSCALL_MINCORE => sys_mincore(args[0], args[1], args[2]),
        SYSCALL_MLOCK2 => sys_mlock2(args[0], args[1], args[2]),
        SYSCALL_STATFS => sys_statfs(args[0] as *const u8, args[1] as *mut Statfs),
        SYSCALL_SYNCFS => sys_syncfs(args[0]),
        SYSCALL_SOCKET => sys_socket(args[0] as u32, args[1] as u32, args[2] as u32),
        SYSCALL_SOCKETPAIR => sys_socketpair(
            args[0] as u32,
            args[1] as u32,
            args[2] as u32,
            args[3] as usize,
        ),
        SYSCALL_BIND => sys_bind(args[0] as u32, args[1] as usize, args[2] as u32),
        SYSCALL_LISTEN => sys_listen(args[0] as u32, args[1] as u32),
        SYSCALL_ACCEPT => sys_accept(args[0] as u32, args[1] as usize, args[2] as usize),
        SYSCALL_ACCEPT4 => sys_accept4(
            args[0] as u32,
            args[1] as usize,
            args[2] as usize,
            args[3] as u32,
        ),
        SYSCALL_CONNECT => sys_connect(args[0] as u32, args[1] as usize, args[2] as u32),
        SYSCALL_GETSOCKNAME => sys_getsockname(args[0] as u32, args[1] as usize, args[2] as usize),
        SYSCALL_GETPEERNAME => sys_getpeername(args[0] as u32, args[1] as usize, args[2] as usize),
        SYSCALL_SENDTO => sys_sendto(
            args[0] as u32,
            args[1] as usize,
            args[2],
            args[3] as u32,
            args[4] as usize,
            args[5] as u32,
        ),
        SYSCALL_RECVFROM => sys_recvfrom(
            args[0] as u32,
            args[1] as usize,
            args[2] as u32,
            args[3] as u32,
            args[4] as usize,
            args[5] as usize,
        ),
        SYSCALL_SETSOCKOPT => sys_setsockopt(
            args[0] as u32,
            args[1] as u32,
            args[2] as u32,
            args[3] as usize,
            args[4] as u32,
        ),
        SYSCALL_GETSOCKOPT => sys_getsockopt(
            args[0] as u32,
            args[1] as u32,
            args[2] as u32,
            args[3] as usize,
            args[4] as usize,
        ),
        SYSCALL_SOCK_SHUTDOWN => sys_sock_shutdown(args[0] as u32, args[1] as u32),
        SYSCALL_SENDMSG => sys_sendmsg(args[0] as u32, args[1], args[2] as u32),
        SYSCALL_RECVMSG => sys_recvmsg(args[0] as u32, args[1], args[2] as u32),
        SYSCALL_GETRANDOM => sys_getrandom(args[0] as usize, args[1] as usize, args[2] as u32),
        SYSCALL_DELETE_MODULE => sys_delete_module(args[0] as *const u8, args[1] as u32),
        SYSCALL_SHUTDOWN => sys_shutdown(),
        SYSCALL_EXT4_COUNTERS => crate::fs::ext4::counters::sys_ext4_counters(args[0], args[1], args[2]),
        SYSCALL_SCHED_SETPARAM => sys_sched_setparam(args[0], args[1] as *const SchedParam),
        SYSCALL_SCHED_SETSCHEDULER => {
            sys_sched_setscheduler(args[0], args[1], args[2] as *const SchedParam)
        }
        SYSCALL_SCHED_GETSCHEDULER => sys_sched_getscheduler(args[0]),
        SYSCALL_SCHED_GETPARAM => sys_sched_getparam(args[0], args[1] as *mut SchedParam),
        SYSCALL_SCHED_SETAFFINITY => {
            sys_sched_setaffinity(args[0], args[1], args[2] as *const u8)
        }
        SYSCALL_SCHED_GETAFFINITY => sys_sched_getaffinity(args[0], args[1], args[2] as *mut u8),
        SYSCALL_SCHED_GET_PRIORITY_MAX => sys_sched_get_priority_max(args[0]),
        SYSCALL_SCHED_GET_PRIORITY_MIN => sys_sched_get_priority_min(args[0]),
        SYSCALL_SCHED_RR_GET_INTERVAL => {
            sys_sched_rr_get_interval(args[0], args[1] as *mut TimeSpec)
        }
        SYSCALL_SCHED_SETATTR => sys_sched_setattr(args[0], args[1] as *const SchedAttr, args[2]),
        SYSCALL_SCHED_GETATTR => {
            sys_sched_getattr(args[0], args[1] as *mut SchedAttr, args[2], args[3])
        }
        SYSCALL_GET_MEMPOLICY => sys_get_mempolicy(
            args[0] as *mut i32,
            args[1] as *mut usize,
            args[2],
            args[3],
            args[4],
        ),
        SYSCALL_MADVISE => sys_madvise(args[0], args[1], args[2]),
        _ => {
            if syscall_id == 242 {
                crate::trace_event!(
                    0xB042,
                    args[0] as u64,
                    args[1] as u64,
                    args[2] as u64,
                    args[3] as u64,
                    0,
                    0
                );
            }
            println!(
                "[syscall] Unsupported syscall: {} ({}), calling over arguments: {:?}",
                syscall_name(syscall_id),
                syscall_id,
                args
            );
            error!(
                "Unsupported syscall:{} ({}), calling over arguments:",
                syscall_name(syscall_id),
                syscall_id
            );
            for i in 0..args.len() {
                error!("args[{}]: {:X}", i, args[i]);
            }
            /*
            crate::task::current_task()
                .unwrap()
                .acquire_inner_lock()
                .add_signal(crate::task::Signals::SIGSYS);
            */
            errno::ENOSYS
        }
    };

    if option_env!("LOG").is_some() && show_info {
        match Errno::try_from(ret) {
            Ok(errno) => info!(
                "[syscall] {}({}) -> {:?}",
                syscall_name(syscall_id),
                syscall_id,
                errno
            ),
            Err(val) => info!(
                "[syscall] {}({}) -> {:X}",
                syscall_name(syscall_id),
                syscall_id,
                val.number
            ),
        }
    }
    ret
}

pub fn sys_getrandom(buf: usize, buflen: usize, flags: u32) -> isize {
    const GRND_NONBLOCK: u32 = 0x0001;
    const GRND_RANDOM: u32 = 0x0002;
    const GRND_INSECURE: u32 = 0x0004;
    const GRND_ALLOWED: u32 = GRND_NONBLOCK | GRND_RANDOM | GRND_INSECURE;

    if flags & !GRND_ALLOWED != 0 {
        return errno::EINVAL;
    }
    if buflen == 0 {
        return 0;
    }

    let buffers = match translated_byte_buffer(
        current_user_token(),
        buf as *const u8,
        buflen,
        UserAccess::Write,
    ) {
        Ok(buffers) => buffers,
        Err(errno) => return errno,
    };
    let mut user = UserBuffer::new(buffers);
    let mut seed = crate::hal::get_time() as u64 ^ ((buf as u64) << 17) ^ buflen as u64;
    let mut offset = 0usize;
    let mut chunk = [0u8; 64];
    while offset < buflen {
        for byte in chunk.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = seed as u8;
        }
        let copy_len = core::cmp::min(chunk.len(), buflen - offset);
        user.write_at(offset, &chunk[..copy_len]);
        offset += copy_len;
    }
    buflen as isize
}
