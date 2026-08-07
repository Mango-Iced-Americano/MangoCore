use super::common::*;

pub fn sys_chdir(path: *const u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!("[sys_chdir] path: {}", path);
    if path.is_empty() {
        return ENOENT;
    }
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }

    // 克隆当前 cwd 状态后释放锁，避免在 find/open 持锁
    let (cwd_inode, old_path, root_inode) = {
        let fs_ref = task.process.fs();
        let lock = fs_ref.lock();
        (
            lock.working_inode.clone(),
            lock.working_path.clone(),
            lock.root_inode.clone(),
        )
    };

    // Check search permission on each parent directory component
    let (uid, fsgid, groups) = caller_ids_and_groups();
    if uid != 0 {
        let perm_result = check_parent_search_access(&cwd_inode.inode, &path, uid, fsgid, &groups);
        if perm_result != SUCCESS {
            return perm_result;
        }
    }

    let target = match vfs_lookup(&cwd_inode.inode, &path, true) {
        Ok(inode) => {
            // ENOTDIR: chdir target must be a directory
            let md = match inode.metadata() {
                Ok(md) => md,
                Err(e) => return -(e as isize),
            };
            if md.file_type != vfs::FileType::Dir {
                warn!("[sys_chdir] not a directory: {:?}", md.file_type);
                return ENOTDIR;
            }
            // Target directory must be searchable (exec permission)
            if uid != 0 && !has_search_access(&md, uid, fsgid, &groups) {
                return EACCES;
            }
            match vfs::File::new(inode, vfs::FileFlags::O_RDONLY) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            }
        }
        Err(errno) => return errno,
    };
    let working_path = if let Some(root_inode) = root_inode.as_ref() {
        // Absolute paths are relative to the process root after chroot(2).
        // Prefer the inode-derived path so a pre-existing stale cache cannot
        // reintroduce the global mount prefix into FsStatus.working_path.
        crate::fs::process_visible_path(&target.inode, Some(root_inode))
            .unwrap_or_else(|_| normalize_cwd(&old_path, &path))
    } else {
        target
            .inode
            .absolute_path()
            .unwrap_or_else(|_| normalize_cwd(&old_path, &path))
    };
    let fs_ref = task.process.fs();
    let mut lock = fs_ref.lock();
    lock.working_inode = target;
    lock.working_path = working_path;
    SUCCESS
}
