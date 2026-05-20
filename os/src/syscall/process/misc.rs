use crate::hal::shutdown;
use crate::mm::copy_to_user_string;
use crate::syscall::errno::*;
use crate::task::{current_user_token, suspend_current_and_run_next};
use num_enum::FromPrimitive;

pub fn sys_shutdown() -> isize {
    shutdown()
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq, FromPrimitive)]
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

pub fn sys_syslog(type_: u32, buf: *mut u8, len: u32) -> isize {
    const LOG_BUF_LEN: usize = 4096;
    const LOG: &str = "<5>[    0.000000] Linux version 5.10.102.1-microsoft-standard-WSL2 (rtrt@TEAM-NPUCORE) (gcc (Ubuntu 9.4.0-1ubuntu1~20.04) 9.4.0, GNU ld (GNU Binutils for Ubuntu) 2.34) #1 SMP Thu Mar 10 13:31:47 CST 2022";
    let token = current_user_token();
    let type_ = SyslogAction::from(type_);
    let len = LOG.len().min(len as usize);
    match type_ {
        SyslogAction::CLOSE | SyslogAction::OPEN => SUCCESS,
        SyslogAction::READ => {
            copy_to_user_string(token, &LOG[..len], buf).unwrap();
            len as isize
        }
        SyslogAction::READ_ALL => {
            copy_to_user_string(token, &LOG[LOG.len() - len..], buf).unwrap();
            len as isize
        }
        SyslogAction::READ_CLEAR => todo!(),
        SyslogAction::CLEAR => todo!(),
        SyslogAction::CONSOLE_OFF => todo!(),
        SyslogAction::CONSOLE_ON => todo!(),
        SyslogAction::CONSOLE_LEVEL => todo!(),
        SyslogAction::SIZE_UNREAD => todo!(),
        SyslogAction::SIZE_BUFFER => LOG_BUF_LEN as isize,
        SyslogAction::ILLEAGAL => EINVAL,
    }
}

pub fn sys_yield() -> isize {
    suspend_current_and_run_next();
    SUCCESS
}
