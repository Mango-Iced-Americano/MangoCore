//! /proc/meminfo — 系统内存信息

use crate::utils::error::SyscallErr;
use crate::fs::procfs::proc_read_str;

pub fn meminfo_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let page_size = crate::config::PAGE_SIZE;
    let free_frames = crate::mm::unallocated_frames();
    let total_pages = crate::config::MEMORY_SIZE / page_size;
    let used_pages = total_pages.saturating_sub(free_frames);

    let (heap_free, heap_total) = crate::mm::heap_stats();

    let mut s = alloc::string::String::with_capacity(512);
    use core::fmt::Write;
    let _ = write!(s, "MemTotal:       {} kB\n", total_pages * page_size / 1024);
    let _ = write!(s, "MemFree:        {} kB\n", free_frames * page_size / 1024);
    let _ = write!(s, "MemAvailable:   {} kB\n", free_frames * page_size / 1024);
    let _ = write!(s, "Buffers:              0 kB\n");
    let _ = write!(s, "Cached:               0 kB\n");
    let _ = write!(s, "SwapTotal:            0 kB\n");
    let _ = write!(s, "SwapFree:             0 kB\n");
    let _ = write!(s, "Dirty:                0 kB\n");
    let _ = write!(s, "Writeback:            0 kB\n");
    let _ = write!(s, "Mapped:          {} kB\n", used_pages * page_size / 1024);
    let _ = write!(s, "Shmem:                0 kB\n");
    let _ = write!(s, "KernelHeap:      {} kB\n", heap_total / 1024);
    let _ = write!(s, "KernelHeapFree:  {} kB\n", heap_free / 1024);

    proc_read_str(offset, len, buf, &s)
}
