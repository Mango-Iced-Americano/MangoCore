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
    if let Err(errno) = vm_ref
        .lock()
        .validate_msync_range(addr, length, flags.contains(MsyncFlags::MS_INVALIDATE))
    {
        return errno;
    }
    info!(
        "[sys_msync] addr: {:X}, length: {:X}, flags: {:?}",
        addr, flags, flags
    );
    SUCCESS
}
