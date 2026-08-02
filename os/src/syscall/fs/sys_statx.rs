use super::common::*;

// AT_STATX sync type flags (Linux 6.6 uapi/linux/fcntl.h).
// NOT added to FstatatFlags — fstatat must reject them.
const AT_STATX_SYNC_TYPE: u32 = 0x6000;
const AT_STATX_FORCE_SYNC: u32 = 0x2000;
const AT_STATX_DONT_SYNC: u32 = 0x4000;

pub fn sys_statx(dirfd: usize, path: *const u8, flags: u32, mask: u32, buf: *mut u8) -> isize {
    let token = current_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    // Validate statx-only sync type flags before FstatatFlags parsing.
    // This prevents fstatat from accidentally accepting them.
    let sync_type = flags & AT_STATX_SYNC_TYPE;
    if sync_type != 0 && sync_type != AT_STATX_FORCE_SYNC && sync_type != AT_STATX_DONT_SYNC {
        return EINVAL;
    }
    // Strip sync bits, parse remainder as shared fstatat flags.
    let flags = match FstatatFlags::from_bits(flags & !AT_STATX_SYNC_TYPE) {
        Some(flags) => flags,
        None => {
            warn!("[sys_statx] unknown flags");
            return EINVAL;
        }
    };
    // Linux 6.6: reject requests with reserved-bit set (future struct statx expansion)
    if mask & STATX__RESERVED != 0 {
        return EINVAL;
    }

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
        if UserPtrMut::new(buf as *mut Statx)
            .write(token, &statx)
            .is_err()
        {
            return EFAULT;
        }
        return SUCCESS;
    }

    let start = if path.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    // Check search permission on parent directories (mirrors sys_fstatat)
    let (uid, fsgid, groups) = caller_ids_and_groups();
    if uid != 0 {
        let perm_result = check_parent_search_access(&start, &path, uid, fsgid, &groups);
        if perm_result != SUCCESS {
            return perm_result;
        }
    }

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
        if UserPtrMut::new(buf as *mut Statx)
            .write(token, &statx)
            .is_err()
        {
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
        if UserPtrMut::new(buf as *mut Statx)
            .write(token, &statx)
            .is_err()
        {
            log::error!("[sys_statx] Failed to copy to {:?}", buf);
            return EFAULT;
        };
        log::debug!("[sys_statx] statx:\n{:?}", statx);
        SUCCESS
    }
}
