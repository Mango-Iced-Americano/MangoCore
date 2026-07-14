use super::common::*;

pub fn sys_chroot(path: *const u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!("[sys_chroot] path: {}", path);
    if path.is_empty() {
        return ENOENT;
    }
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }

    // clone cwd while not holding fs lock
    let cwd_inode = {
        let fs_ref = task.process.fs();
        let lock = fs_ref.lock();
        lock.working_inode.inode.clone()
    };

    // Linux: search permission check before privilege check → EACCES, not EPERM
    let (uid, fsgid, groups) = caller_ids_and_groups();
    let search_result = check_parent_search_access(&cwd_inode, &path, uid, fsgid, &groups);
    if search_result != SUCCESS {
        return search_result;
    }

    let target = match vfs_lookup(&cwd_inode, &path, true) {
        Ok(inode) => {
            let md = match inode.metadata() {
                Ok(md) => md,
                Err(e) => return -(e as isize),
            };
            if md.file_type != vfs::FileType::Dir {
                return ENOTDIR;
            }
            // Check search access on the target directory itself
            if !has_final_access(&md, FaccessatMode::X_OK, uid, fsgid, &groups) {
                return EACCES;
            }
            match vfs::File::new(inode, vfs::FileFlags::O_RDONLY) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            }
        }
        Err(errno) => return errno,
    };

    // only root may chroot
    if uid != 0 {
        return EPERM;
    }

    let target_inode = target.inode.clone();
    let fs_ref = task.process.fs();
    let mut lock = fs_ref.lock();
    lock.working_inode = target;
    lock.working_path = alloc::string::String::from("/");
    lock.root_inode = Some(target_inode);
    SUCCESS
}
