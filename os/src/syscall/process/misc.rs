use crate::hal::shutdown;
use crate::mm::{copy_to_user_array, translated_str};
use crate::syscall::errno::*;
use crate::task::{
    current_euid, current_task, current_user_token, has_ready_task,
    suspend_current_and_run_next,
};
use core::sync::atomic::{AtomicBool, Ordering};
use log::info;
use num_enum::FromPrimitive;

const SYSLOG_BUF_LEN: usize = 4096;
const SYSLOG_BOOT_LOG: &[u8] = b"<5>[    0.000000] Linux version 5.10.102.1-MangoCore (mango@kernel.org) (gcc (Ubuntu 9.4.0-1ubuntu1~20.04) 9.4.0, GNU ld (GNU Binutils for Ubuntu) 2.34) #1 SMP Thu Mar 10 13:31:47 CST 2022";
const CAP_SYS_ADMIN: usize = 21;
const CAP_SYSLOG: usize = 34;
static SYSLOG_READ_ALL_CLEARED: AtomicBool = AtomicBool::new(false);

pub fn sys_shutdown() -> isize {
    info!("[sys_shutdown] committing and detaching filesystem backends...");
    if let Err(error) = crate::fs::vfs::mount::shutdown_all_backends() {
        log::error!(
            "[sys_shutdown] one or more filesystem backends failed to shut down: {:?}",
            error
        );
    }
    info!("[sys_shutdown] halting");
    shutdown()
}

pub fn sys_reboot(magic: usize, magic2: usize, cmd: usize, _arg: usize) -> isize {
    const LINUX_REBOOT_MAGIC1: u32 = 0xfee1dead;
    const LINUX_REBOOT_MAGIC2: u32 = 0x28121969;
    const LINUX_REBOOT_MAGIC2A: u32 = 0x05121996;
    const LINUX_REBOOT_MAGIC2B: u32 = 0x16041998;
    const LINUX_REBOOT_MAGIC2C: u32 = 0x20112000;

    const LINUX_REBOOT_CMD_CAD_OFF: u32 = 0x00000000;
    const LINUX_REBOOT_CMD_RESTART: u32 = 0x01234567;
    const LINUX_REBOOT_CMD_HALT: u32 = 0xcdef0123;
    const LINUX_REBOOT_CMD_CAD_ON: u32 = 0x89abcdef;
    const LINUX_REBOOT_CMD_POWER_OFF: u32 = 0x4321fedc;
    const LINUX_REBOOT_CMD_RESTART2: u32 = 0xa1b2c3d4;
    const LINUX_REBOOT_CMD_SW_SUSPEND: u32 = 0xd000fce2;
    const LINUX_REBOOT_CMD_KEXEC: u32 = 0x45584543;

    let magic = magic as u32;
    let magic2 = magic2 as u32;
    let cmd = cmd as u32;

    if magic != LINUX_REBOOT_MAGIC1
        || !matches!(
            magic2,
            LINUX_REBOOT_MAGIC2
                | LINUX_REBOOT_MAGIC2A
                | LINUX_REBOOT_MAGIC2B
                | LINUX_REBOOT_MAGIC2C
        )
    {
        return EINVAL;
    }

    let known_cmd = matches!(
        cmd,
        LINUX_REBOOT_CMD_CAD_OFF
            | LINUX_REBOOT_CMD_RESTART
            | LINUX_REBOOT_CMD_HALT
            | LINUX_REBOOT_CMD_CAD_ON
            | LINUX_REBOOT_CMD_POWER_OFF
            | LINUX_REBOOT_CMD_RESTART2
            | LINUX_REBOOT_CMD_SW_SUSPEND
            | LINUX_REBOOT_CMD_KEXEC
    );
    if !known_cmd {
        return EINVAL;
    }

    if current_euid() != 0 {
        return EPERM;
    }

    SUCCESS
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
    inner.euid == 0 || (inner.cap_effective & ((1u64 << CAP_SYS_ADMIN) | (1u64 << CAP_SYSLOG))) != 0
}

pub fn sys_yield() -> isize {
    if has_ready_task() {
        suspend_current_and_run_next();
    }
    SUCCESS
}

pub fn sys_delete_module(name: *const u8, _flags: u32) -> isize {
    if current_euid() != 0 {
        return EPERM;
    }

    match translated_str(current_user_token(), name) {
        Ok(_) => ENOENT,
        Err(errno) => errno,
    }
}
