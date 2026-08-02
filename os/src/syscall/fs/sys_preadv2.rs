use super::common::*;
use super::sys_preadv::sys_preadv;
use super::sys_readv::sys_readv;

pub fn sys_preadv2(
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
        sys_readv(fd, iov, iovcnt)
    } else {
        sys_preadv(fd, iov, iovcnt, offset)
    }
}
