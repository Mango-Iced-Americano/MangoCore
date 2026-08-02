use super::common::*;

pub fn sys_truncate(path: *const u8, length: isize) -> isize {
    if length < 0 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    if path.is_empty() {
        return ENOENT;
    }
    let cwd_inode = {
        let fs_ref = task.process.fs();
        let lock = fs_ref.lock();
        lock.working_inode.inode.clone()
    };
    // Check parent directory search permission before lookup (correct errno order)
    let (uid, gid, groups) = caller_ids_and_groups();
    let start = if path.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        cwd_inode.clone()
    };
    let parent_result = check_parent_search_access(&start, &path, uid, gid, &groups);
    if parent_result != SUCCESS {
        return parent_result;
    }
    let inode = if path.starts_with('/') {
        match vfs_lookup_absolute(&path) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    } else {
        match vfs_lookup(&cwd_inode, &path, true) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };
    let md = match inode.metadata() {
        Ok(md) => md,
        Err(e) => return -(e as isize),
    };
    if md.file_type == FileType::Dir {
        return EISDIR;
    }
    // Check RLIMIT_FSIZE
    let fsize_limit = task.process.fsize_limit();
    if (length as usize) > fsize_limit {
        return EFBIG;
    }
    // Check write permission on the file itself
    if uid != 0 && (permission_class_bits(&md, uid, gid, &groups) & 0o2) == 0 {
        return EACCES;
    }
    match inode.resize(length as usize) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}
