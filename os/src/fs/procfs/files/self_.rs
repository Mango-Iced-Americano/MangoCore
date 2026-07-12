//! /proc/self — 指向当前进程 PID 的符号链接
//!
//! 动态解析：每次读取时通过 `current_task()` 获取当前 PID。

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::format;

/// /proc/self 的内容 = 当前 PID 的数字字符串
pub fn self_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    // /proc/self is a symlink, but it's implemented as a file with dynamic content.
    // The VFS symlink resolution path will call read_at to get the target.
    let pid = crate::task::current_task().map(|t| t.pid()).unwrap_or(0);
    let s = format!("{}", pid);
    proc_read_str(offset, len, buf, &s)
}
