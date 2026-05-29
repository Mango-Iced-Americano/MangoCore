//! /proc/net/dev — 网络接口统计

use alloc::fmt::Write;
use alloc::string::String;
use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

/// 生成 /proc/net/dev 内容
pub fn net_dev_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut content = String::new();

    content
        .push_str("Inter-|   Receive                                                |  Transmit\n");
    content
        .push_str(
            " face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n",
        );

    let ifaces = crate::net::net_core::IFACES.lock();
    for dev in ifaces.iter() {
        // 统计字段暂填 0，格式与 Linux /proc/net/dev 一致
        let _ = write!(
            content,
            "{:>6}:{:>8}{:>8}{:>5}{:>5}{:>5}{:>6}{:>11}{:>10}\
             {:>8}{:>8}{:>5}{:>5}{:>5}{:>6}{:>8}{:>10}\n",
            dev.name,
            0u64, 0u64, 0u32, 0u32, 0u32, 0u32, 0u32, 0u32,
            0u64, 0u64, 0u32, 0u32, 0u32, 0u32, 0u32, 0u32,
        );
    }

    proc_read_str(offset, len, buf, &content)
}
