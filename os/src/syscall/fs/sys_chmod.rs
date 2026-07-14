use super::common::*;
use super::sys_fchmodat::sys_fchmodat;

pub fn sys_chmod(path: *const u8, mode: u32) -> isize {
    sys_fchmodat(crate::syscall::AT_FDCWD, path, mode, 0)
}
