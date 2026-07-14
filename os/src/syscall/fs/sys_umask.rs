use super::common::*;

pub fn sys_umask(mask: u32) -> isize {
    info!("[sys_umask] mask: {:o}", mask);
    let task = current_task().unwrap();
    let fs_ref = task.process.fs();
    let mut fs = fs_ref.lock();
    let old_mask = fs.umask & 0o777;
    fs.umask = mask & 0o777;
    old_mask as isize
}
