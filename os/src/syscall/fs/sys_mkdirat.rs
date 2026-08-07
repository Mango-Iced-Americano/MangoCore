use super::common::*;

pub fn sys_mkdirat(dirfd: usize, path: *const u8, mode: u32) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!(
        "[sys_mkdirat] dirfd: {}, path: {}, mode: {:?}",
        dirfd as isize,
        path,
        StatMode::from_bits(mode)
    );
    let start = if path.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };
    // Root directory "/" already exists
    if path == "/" || path == "." {
        return EEXIST;
    }
    // Linux: path components must not exceed NAME_MAX (255) — ENAMETOOLONG before any other check
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    let (uid, fsgid, groups) = caller_ids_and_groups();
    let parent_result = check_parent_search_access(&start, &path, uid, fsgid, &groups);
    if parent_result != SUCCESS {
        return parent_result;
    }
    let (parent, leaf) = match vfs_lookup_parent_for_start(&start, &path) {
        Ok(result) => result,
        Err(errno) => return errno,
    };

    // Check write permission on parent
    if let Err(errno) = check_parent_write_search_access(&parent, uid, fsgid, &groups) {
        return errno;
    }

    // mkdir04: check EEXIST first (Linux ordering, before creation attempt)
    let parent_meta = match parent.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    if parent.find(&leaf).is_ok() {
        return EEXIST;
    }

    // Apply umask and SGID inheritance
    let mut dir_mode = apply_current_umask(vfs::InodeMode::from_bits_truncate(mode));
    let child_gid = if parent_meta.mode.contains(vfs::InodeMode::S_ISGID) {
        parent_meta.gid
    } else {
        fsgid
    };
    if parent_meta.mode.contains(vfs::InodeMode::S_ISGID) {
        dir_mode.insert(vfs::InodeMode::S_ISGID);
    }

    // Create with attrs.  another_ext4 can report metadata transaction
    // contention as EAGAIN; mkdir is a blocking namespace mutation, so wait
    // and retry without holding any VFS lookup lock.
    let mut created = None;
    let ret = wait_io_core(
        || match parent.mkdir(&leaf, dir_mode) {
            Ok(inode) => {
                created = Some(inode);
                SUCCESS
            }
            Err(e) => -(e as isize),
        },
        false,
    );
    if ret < 0 {
        return ret;
    }
    let inode = created.expect("mkdir succeeded without returning an inode");
    // Set uid/gid after creation
    if let Ok(mut child_meta) = inode.metadata() {
        child_meta.uid = uid;
        child_meta.gid = child_gid;
        if let Err(e) = inode.set_metadata(&child_meta) {
            log::error!("[sys_mkdirat] set_metadata failed for '{}': {:?}", path, e);
            // Don't fail the mkdir — the dir was created, just uid/gid might be wrong
        }
    }
    SUCCESS
}
