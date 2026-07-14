use super::common::*;

pub fn sys_fchmodat(dirfd: usize, path: *const u8, mode: u32, _flags: u32) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path_str = match UserCString::from_addr(path as usize).read(token) {
        Ok(s) => s,
        Err(_) => return EFAULT,
    };
    if let Err(errno) = validate_path_len(&path_str) {
        return errno;
    }
    if path_str.is_empty() {
        return ENOENT;
    }
    let inode = if path_str.starts_with('/') {
        match vfs_lookup_absolute(&path_str) {
            Ok(inode) => inode,
            Err(e) => return e,
        }
    } else {
        let start = match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let (uid, gid) = open_subject_ids();
        let perm_err = check_parent_search_access(&start, &path_str, uid, gid);
        if perm_err != SUCCESS {
            return perm_err;
        }
        match vfs_lookup(&start, &path_str, true) {
            Ok(inode) => inode,
            Err(e) => return e,
        }
    };
    // Check read-only filesystem (must precede EPERM per Linux semantics:
    // EROFS takes priority over EPERM)
    if let Some(mnt) = inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }
    match do_fchmod(&inode, mode) {
        Ok(()) => 0,
        Err(e) => e,
    }
}
