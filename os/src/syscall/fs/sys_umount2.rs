use super::common::*;

pub fn sys_umount2(target: *const u8, flags: u32) -> isize {
    if target.is_null() {
        return EINVAL;
    }
    // Permission check: umount requires root (CAP_SYS_ADMIN)
    let task = current_task().unwrap();
    if task.acquire_inner_lock().euid != 0 {
        return EPERM;
    }
    let token = current_user_token();
    let target = match user_cstring(token, target) {
        Ok(target) => target,
        Err(errno) => return errno,
    };
    let flags = match UmountFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return EINVAL,
    };
    info!("[sys_umount2] target: {}, flags: {:?}", target, flags);
    let (lookup_inode, lookup_path) = {
        let task = current_task().unwrap();
        let fs_ref = task.process.fs();
        let fs = fs_ref.lock();
        if target.starts_with('/') {
            let root: Arc<dyn vfs::IndexNode> = crate::fs::vfs_root().mountpoint_root_inode();
            (root, target)
        } else {
            let cwd_inode: Arc<dyn vfs::IndexNode> = fs.working_inode.inode.clone();
            let path = normalize_cwd(&fs.working_path, &target);
            (cwd_inode, path)
        }
    };
    let inode = match vfs_lookup(&lookup_inode, &lookup_path, false) {
        Ok(inode) => inode,
        Err(errno) => {
            error!("[sys_umount2] vfs_lookup failed for path '{}': errno={}", lookup_path, errno);
            return errno;
        }
    };
    if flags.contains(UmountFlags::MNT_DETACH) {
        let target_mnt = match resolve_umount_target(&inode) {
            Ok(mnt) => mnt,
            Err(errno) => return errno,
        };
        return match target_mnt.detach_recursive() {
            Ok(()) => SUCCESS,
            Err(e) => -(e as isize),
        };
    }
    match inode.umount() {
        Ok(_) => SUCCESS,
        Err(e) => {
            error!("[sys_umount2] inode.umount() failed for '{}': errno={}", lookup_path, e as isize);
            -(e as isize)
        }
    }
}
