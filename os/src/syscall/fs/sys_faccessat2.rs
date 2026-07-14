use super::common::*;

pub fn sys_faccessat2(dirfd: usize, pathname: *const u8, mode: u32, flags: u32) -> isize {
    let token = current_user_token();
    let pathname = match user_cstring(token, pathname) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let mode = match FaccessatMode::from_bits(mode) {
        Some(mode) => mode,
        None => {
            warn!("[sys_faccessat2] unknown mode");
            return EINVAL;
        }
    };
    let flags = match FaccessatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_faccessat2] unknown flags");
            return EINVAL;
        }
    };

    info!(
        "[sys_faccessat2] dirfd: {}, pathname: {}, mode: {:?}, flags: {:?}",
        dirfd as isize, pathname, mode, flags
    );

    if pathname.is_empty() {
        return ENOENT;
    }
    if let Err(errno) = validate_path_len(&pathname) {
        return errno;
    }

    let nofollow = flags.contains(FaccessatFlags::AT_SYMLINK_NOFOLLOW);
    let start_inode = if pathname.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };
    let (uid, gid) = access_subject_ids(flags.contains(FaccessatFlags::AT_EACCESS));
    let parent_result = check_parent_search_access(&start_inode, &pathname, uid, gid);
    if parent_result != SUCCESS {
        return parent_result;
    }
    let inode = match vfs_lookup(&start_inode, &pathname, !nofollow) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };
    let meta = match inode.metadata() {
        Ok(meta) => meta,
        Err(e) => return -(e as isize),
    };
    if mode.contains(FaccessatMode::W_OK) {
        if let Some(mnt) = inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
            if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
                return EROFS;
            }
        }
    }
    if has_final_access(&meta, mode, uid, gid) {
        SUCCESS
    } else {
        EACCES
    }
}
