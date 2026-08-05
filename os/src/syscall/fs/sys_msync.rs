use super::common::*;

pub fn sys_msync(addr: usize, length: usize, flags: u32) -> isize {
    if !VirtAddr::from(addr).aligned() {
        return EINVAL;
    }
    let flags = match MsyncFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return EINVAL,
    };
    if flags.contains(MsyncFlags::MS_ASYNC) && flags.contains(MsyncFlags::MS_SYNC) {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let vm_ref = task.process.vm();
    let ranges = match vm_ref.read(|vm| {
        vm.validate_msync_range(addr, length, flags.contains(MsyncFlags::MS_INVALIDATE))?;
        vm.msync_page_ranges(addr, length)
    }) {
        Ok(ranges) => ranges,
        Err(errno) => return errno,
    };
    // VM 锁已释放。MS_SYNC 的后端 I/O 绝不能与 VM/PTE 锁或 TLB ack 等待重叠。
    for (inode, start_page, end_page) in ranges {
        let Some(page_cache) = inode.page_cache() else {
            continue;
        };
        if flags.contains(MsyncFlags::MS_SYNC) {
            if page_cache.writeback_range(start_page, end_page).is_err() {
                return EIO;
            }
        } else {
            page_cache.queue_writeback();
        }
    }
    info!(
        "[sys_msync] addr: {:X}, length: {:X}, flags: {:?}",
        addr, flags, flags
    );
    SUCCESS
}
