use crate::hal::shutdown;
use crate::mm::copy_to_user_array;
use crate::syscall::errno::*;
use crate::task::{current_task, current_user_token, suspend_current_and_run_next};
use core::sync::atomic::{AtomicBool, Ordering};
use num_enum::FromPrimitive;

const SYSLOG_BUF_LEN: usize = 4096;
const SYSLOG_BOOT_LOG: &[u8] = b"<5>[    0.000000] Linux version 5.10.102.1-microsoft-standard-WSL2 (rtrt@TEAM-NPUCORE) (gcc (Ubuntu 9.4.0-1ubuntu1~20.04) 9.4.0, GNU ld (GNU Binutils for Ubuntu) 2.34) #1 SMP Thu Mar 10 13:31:47 CST 2022";
const CAP_SYS_ADMIN: usize = 21;
const CAP_SYSLOG: usize = 34;
static SYSLOG_READ_ALL_CLEARED: AtomicBool = AtomicBool::new(false);

pub fn sys_shutdown() -> isize {
    shutdown()
}

#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u32)]
pub enum SyslogAction {
    CLOSE = 0,
    OPEN = 1,
    READ = 2,
    READ_ALL = 3,
    READ_CLEAR = 4,
    CLEAR = 5,
    CONSOLE_OFF = 6,
    CONSOLE_ON = 7,
    CONSOLE_LEVEL = 8,
    SIZE_UNREAD = 9,
    SIZE_BUFFER = 10,
    #[default]
    ILLEAGAL,
}

fn copy_syslog_to_user(buf: *mut u8, len: isize, respect_clear: bool) -> isize {
    if buf.is_null() || len < 0 {
        return EINVAL;
    }
    let len = len as usize;
    if len == 0 || (respect_clear && SYSLOG_READ_ALL_CLEARED.load(Ordering::Relaxed)) {
        return 0;
    }
    let token = current_user_token();
    let copied = SYSLOG_BOOT_LOG.len().min(len);
    let start = SYSLOG_BOOT_LOG.len() - copied;
    match copy_to_user_array(token, SYSLOG_BOOT_LOG[start..].as_ptr(), buf, copied) {
        Ok(_) => copied as isize,
        Err(errno) => errno,
    }
}

pub fn sys_syslog(type_: u32, buf: *mut u8, len: isize) -> isize {
    let type_ = SyslogAction::from(type_);
    if type_ == SyslogAction::ILLEAGAL {
        return EINVAL;
    }
    if syslog_requires_privilege(type_) && !has_syslog_privilege() {
        return EPERM;
    }
    match type_ {
        SyslogAction::CLOSE | SyslogAction::OPEN => SUCCESS,
        SyslogAction::READ => copy_syslog_to_user(buf, len, false),
        SyslogAction::READ_ALL => copy_syslog_to_user(buf, len, true),
        SyslogAction::READ_CLEAR => {
            let ret = copy_syslog_to_user(buf, len, true);
            if ret >= 0 {
                SYSLOG_READ_ALL_CLEARED.store(true, Ordering::Relaxed);
            }
            ret
        }
        SyslogAction::CLEAR => {
            SYSLOG_READ_ALL_CLEARED.store(true, Ordering::Relaxed);
            SUCCESS
        }
        SyslogAction::CONSOLE_OFF | SyslogAction::CONSOLE_ON => SUCCESS,
        SyslogAction::CONSOLE_LEVEL => {
            if (1..=8).contains(&len) {
                SUCCESS
            } else {
                EINVAL
            }
        }
        SyslogAction::SIZE_UNREAD => SYSLOG_BOOT_LOG.len() as isize,
        SyslogAction::SIZE_BUFFER => SYSLOG_BUF_LEN as isize,
        SyslogAction::ILLEAGAL => EINVAL,
    }
}

fn syslog_requires_privilege(action: SyslogAction) -> bool {
    !matches!(
        action,
        SyslogAction::READ_ALL | SyslogAction::SIZE_BUFFER | SyslogAction::ILLEAGAL
    )
}

fn has_syslog_privilege() -> bool {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    inner.euid == 0
        || (inner.cap_effective & ((1u64 << CAP_SYS_ADMIN) | (1u64 << CAP_SYSLOG))) != 0
}

pub fn sys_yield() -> isize {
    suspend_current_and_run_next();
    SUCCESS
}
