use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn pid_io_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(
        offset,
        len,
        buf,
        "rchar: 0\nwchar: 0\nread_bytes: 0\nwrite_bytes: 0\ncancelled_write_bytes: 0\n",
    )
}
