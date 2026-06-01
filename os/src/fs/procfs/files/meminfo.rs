//! /proc/meminfo — 系统内存信息

use crate::utils::error::SyscallErr;
use crate::fs::procfs::proc_read_str;

pub fn meminfo_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let total_kb = crate::mm::total_memory_kbytes();
    let free_available_kb = crate::mm::free_memory_kbytes();
    let free_kb = free_available_kb.min(total_kb);
    let used_kb = total_kb.saturating_sub(free_kb);

    let (heap_free, heap_total, _au, _aa, _w) = crate::mm::heap_stats();

    let mut s = alloc::string::String::with_capacity(512);
    use core::fmt::Write;
    let _ = write!(s, "MemTotal:       {} kB\n", total_kb);
    let _ = write!(s, "MemFree:        {} kB\n", free_kb);
    let _ = write!(s, "MemAvailable:   {} kB\n", free_available_kb);
    let _ = write!(s, "Buffers:              0 kB\n");
    let _ = write!(s, "Cached:               0 kB\n");
    let _ = write!(s, "SwapTotal:            0 kB\n");
    let _ = write!(s, "SwapFree:             0 kB\n");
    let _ = write!(s, "CommitLimit:    {} kB\n", crate::mm::commit_limit_kbytes());
    let _ = write!(s, "Committed_AS:   {} kB\n", crate::mm::committed_as_kbytes());
    let _ = write!(s, "Dirty:                0 kB\n");
    let _ = write!(s, "Writeback:            0 kB\n");
    let _ = write!(s, "Mapped:          {} kB\n", used_kb);
    let _ = write!(s, "Shmem:                0 kB\n");
    let _ = write!(s, "KernelHeap:      {} kB\n", heap_total / 1024);
    let _ = write!(s, "KernelHeapFree:  {} kB\n", heap_free / 1024);

    proc_read_str(offset, len, buf, &s)
}
