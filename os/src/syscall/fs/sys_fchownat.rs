use super::common::*;

pub fn sys_fchownat(dirfd: usize, path: *const u8, owner: u32, group: u32, flags: u32) -> isize {
    let token = current_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }

    let flags = match FchownatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return EINVAL,
    };
    if path.is_empty() && !flags.contains(FchownatFlags::AT_EMPTY_PATH) {
        return ENOENT;
    }

    let follow_final = !flags.contains(FchownatFlags::AT_SYMLINK_NOFOLLOW);
    let inode = if path.is_empty() {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    } else {
        let start = if path.starts_with('/') {
            crate::fs::current_root_inode()
        } else {
            match resolve_start_inode(dirfd) {
                Ok(inode) => inode,
                Err(errno) => return errno,
            }
        };
        // Permission: search access on parent directories
        let (uid, fsgid, groups) = caller_ids_and_groups();
        if uid != 0 {
            let perm_result = check_parent_search_access(&start, &path, uid, fsgid, &groups);
            if perm_result != SUCCESS {
                return perm_result;
            }
        }
        match vfs_lookup(&start, &path, follow_final) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    // Check read-only filesystem (must precede EPERM per Linux semantics:
    // EROFS takes priority over EPERM)
    if let Some(mnt) = inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }

    do_chown(&inode, owner, group)
}
