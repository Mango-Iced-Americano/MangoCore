use super::common::*;
use super::sys_writev::sys_writev;
use super::sys_pwritev::sys_pwritev;

pub fn sys_pwritev2(
    fd: usize,
    iov: usize,
    iovcnt: usize,
    offset_low: usize,
    offset_high: usize,
    flags: usize,
) -> isize {
    if flags != 0 {
        return EOPNOTSUPP;
    }
    let offset = split_offset64(offset_low, offset_high);
    if offset == usize::MAX {
        sys_writev(fd, iov, iovcnt)
    } else {
        sys_pwritev(fd, iov, iovcnt, offset)
    }
}
