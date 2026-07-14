use super::common::*;

pub fn sys_statx(dirfd: usize, path: *const u8, flags: u32, mask: u32, buf: *mut u8) -> isize {
    let token = current_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    let flags = match FstatatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_statx] unknown flags");
            return EINVAL;
        }
    };

    info!(
        "[sys_statx] dirfd: {}, path: {:?}, flags: {:?}",
        dirfd as isize, path, flags,
    );

    // AT_EMPTY_PATH: stat the dirfd itself (glibc dynamic linker on la64)
    if path.is_empty() {
        if !flags.contains(FstatatFlags::AT_EMPTY_PATH) {
            return ENOENT;
        }
        let start = match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let statx = match start.metadata() {
            Ok(meta) => metadata_to_statx(&meta, mask),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Statx).write(token, &statx).is_err() {
            return EFAULT;
        }
        return SUCCESS;
    }

    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    let no_follow = flags.contains(FstatatFlags::AT_SYMLINK_NOFOLLOW);
    if no_follow {
        // AT_SYMLINK_NOFOLLOW: 使用新 VFS 路径解析
        let inode = match vfs_lookup(&start, &path, false) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let statx = match inode.metadata() {
            Ok(meta) => metadata_to_statx(&meta, mask),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Statx).write(token, &statx).is_err() {
            return EFAULT;
        }
        SUCCESS
    } else {
        let inode = match vfs_lookup(&start, &path, true) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let statx = match inode.metadata() {
            Ok(meta) => metadata_to_statx(&meta, mask),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Statx).write(token, &statx).is_err() {
            log::error!("[sys_statx] Failed to copy to {:?}", buf);
            return EFAULT;
        };
        log::debug!("[sys_statx] statx:\n{:?}", statx);
        SUCCESS
    }
}
