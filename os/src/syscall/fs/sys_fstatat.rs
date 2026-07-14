use super::common::*;

pub fn sys_fstatat(dirfd: usize, path: *const u8, buf: *mut u8, flags: u32) -> isize {
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
            warn!("[sys_fstatat] unknown flags");
            return EINVAL;
        }
    };
    if path.is_empty() && !flags.contains(FstatatFlags::AT_EMPTY_PATH) {
        return ENOENT;
    }

    info!(
        "[sys_fstatat] dirfd: {}, path: {:?}, flags: {:?}",
        dirfd as isize, path, flags,
    );

    // AT_EMPTY_PATH + empty path: stat the dirfd itself, skip path resolution
    if path.is_empty() && flags.contains(FstatatFlags::AT_EMPTY_PATH) {
        let inode = match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let stat = match inode.metadata() {
            Ok(meta) => metadata_to_stat(&meta),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Stat).write(token, &stat).is_err() {
            return EFAULT;
        }
        return SUCCESS;
    }

    let no_follow = flags.contains(FstatatFlags::AT_SYMLINK_NOFOLLOW);
    let start = if path.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    // uid==0 (root) bypasses DAC — skip permission checks entirely
    let task = current_task().unwrap();
    let is_root = task.acquire_inner_lock().euid == 0;

    if no_follow {
        // AT_SYMLINK_NOFOLLOW: 使用新 VFS 路径解析
        let inode = match vfs_lookup(&start, &path, false) {
            Ok(inode) => inode,
            Err(errno) => {
                // On lookup failure, check if parent search permission was the cause
                if !is_root {
                    let (uid, fsgid, groups) = caller_ids_and_groups();
                    let perm = check_parent_search_access(&start, &path, uid, fsgid, &groups);
                    if perm != SUCCESS {
                        return perm;
                    }
                }
                return errno;
            }
        };
        let stat = match inode.metadata() {
            Ok(meta) => metadata_to_stat(&meta),
            Err(e) => return -(e as isize),
        };
        info!(
            "[sys_fstatat] dirfd: {}, path: {:?}, flags: {:?}, st_ino: {}",
            dirfd as isize, path, flags, stat.st_ino,
        );
        if UserPtrMut::new(buf as *mut Stat).write(token, &stat).is_err() {
            return EFAULT;
        }
        SUCCESS
    } else {
        let inode = match vfs_lookup(&start, &path, true) {
            Ok(inode) => inode,
            Err(errno) => {
                if !is_root {
                    let (uid, fsgid, groups) = caller_ids_and_groups();
                    let perm = check_parent_search_access(&start, &path, uid, fsgid, &groups);
                    if perm != SUCCESS {
                        return perm;
                    }
                }
                return errno;
            }
        };
        let stat = match inode.metadata() {
            Ok(meta) => metadata_to_stat(&meta),
            Err(e) => return -(e as isize),
        };
        info!(
            "[sys_fstatat] dirfd: {}, path: {:?}, flags: {:?}, st_ino: {}",
            dirfd as isize, path, flags, stat.st_ino,
        );
        if UserPtrMut::new(buf as *mut Stat).write(token, &stat).is_err() {
            log::error!("[sys_fstatat] Failed to copy to {:?}", buf);
            return EFAULT;
        };
        SUCCESS
    }
}
