use super::common::*;

pub fn sys_unlinkat(dirfd: usize, path: *const u8, flags: u32) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let flags = match UnlinkatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_unlinkat] unknown flags");
            return EINVAL;
        }
    };
    info!(
        "[sys_unlinkat] dirfd: {}, path: {}, flags: {:?}",
        dirfd as isize, path, flags
    );

    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }

    if flags.contains(UnlinkatFlags::AT_REMOVEDIR) {
        let trimmed = path.trim_end_matches('/');
        if trimmed == "." || trimmed.ends_with("/.") {
            return EINVAL;
        }
    }

    let start = if path.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };
    let (uid, fsgid, groups) = caller_ids_and_groups();
    let parent_result = check_parent_search_access(&start, &path, uid, fsgid, &groups);
    if parent_result != SUCCESS {
        return parent_result;
    }
    let (parent, leaf) = match vfs_lookup_parent_for_start(&start, &path) {
        Ok(result) => result,
        Err(errno) => return errno,
    };
    if let Err(errno) = check_parent_write_search_access(&parent, uid, fsgid, &groups) {
        return errno;
    }
    // sticky bit: only file owner, dir owner, or root may delete from sticky dir
    let parent_meta = match parent.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    if parent_meta.mode.contains(vfs::InodeMode::S_ISVTX) && uid != 0 && uid != parent_meta.uid
    {
        if let Ok(file_inode) = parent.find(&leaf) {
            if let Ok(file_meta) = file_inode.metadata() {
                if uid != file_meta.uid {
                    return EPERM;
                }
            }
        }
    }
    let result = if flags.contains(UnlinkatFlags::AT_REMOVEDIR) {
        parent.rmdir(&leaf)
    } else {
        parent.unlink(&leaf)
    };
    match result {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}
