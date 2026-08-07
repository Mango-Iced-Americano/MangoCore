use super::common::*;

pub fn sys_renameat2(
    olddirfd: usize,
    oldpath: *const u8,
    newdirfd: usize,
    newpath: *const u8,
    flags: u32,
) -> isize {
    use crate::fs::vfs::RENAME_NOREPLACE;
    // RENAME_SUPPORTED_FLAGS: only NOREPLACE for now
    const RENAME_SUPPORTED_FLAGS: u32 = RENAME_NOREPLACE;

    // reject unsupported flags
    if flags & !RENAME_SUPPORTED_FLAGS != 0 {
        return -(SyscallErr::EINVAL as isize);
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let oldpath_str = match user_cstring(token, oldpath) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let newpath_str = match user_cstring(token, newpath) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!(
        "[sys_renameat2] old: dirfd={} path={}, new: dirfd={} path={}, flags={:#x}",
        olddirfd as isize, oldpath_str, newdirfd as isize, newpath_str, flags
    );

    let old_start = if oldpath_str.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(olddirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };
    let new_start = if newpath_str.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(newdirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    let (uid, fsgid, groups) = caller_ids_and_groups();
    let old_perm = check_parent_search_access(&old_start, &oldpath_str, uid, fsgid, &groups);
    if old_perm != SUCCESS { return old_perm; }
    let new_perm = check_parent_search_access(&new_start, &newpath_str, uid, fsgid, &groups);
    if new_perm != SUCCESS { return new_perm; }

    // 解析 oldpath: 获取父目录 + 叶子名
    let (old_parent, old_leaf) = match vfs_lookup_parent_for_start(&old_start, &oldpath_str) {
        Ok(pair) => pair,
        Err(errno) => return errno,
    };

    // 解析 newpath: 获取父目录 + 叶子名
    let (new_parent, new_leaf) = match vfs_lookup_parent_for_start(&new_start, &newpath_str) {
        Ok(pair) => pair,
        Err(errno) => return errno,
    };

    // Check write+search permission on both parent directories
    if uid != 0 {
        if let Err(errno) = check_parent_write_search_access(&old_parent, uid, fsgid, &groups) {
            return errno;
        }
    }
    if uid != 0 {
        if let Err(errno) = check_parent_write_search_access(&new_parent, uid, fsgid, &groups) {
            return errno;
        }
    }

    // VFS 层 RENAME_NOREPLACE 预检（目标存在即返回 EEXIST）
    if flags & RENAME_NOREPLACE != 0 {
        match new_parent.find(&new_leaf) {
            Ok(_) => return -(SyscallErr::EEXIST as isize),
            Err(SyscallErr::ENOENT) => {} // 目标不存在，继续
            Err(e) => return -(e as isize),
        }
    }

    // sticky bit check on source directory: only file owner, dir owner, or root may rename
    let old_parent_meta = match old_parent.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    if old_parent_meta.mode.contains(vfs::InodeMode::S_ISVTX) {
        if uid != 0 && uid != old_parent_meta.uid {
            if let Ok(file_inode) = old_parent.find(&old_leaf) {
                if let Ok(file_meta) = file_inode.metadata() {
                    if uid != file_meta.uid {
                        return -(SyscallErr::EPERM as isize);
                    }
                }
            }
        }
    }

    // sticky bit check on target directory
    let new_parent_meta = match new_parent.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    if new_parent_meta.mode.contains(vfs::InodeMode::S_ISVTX) {
        if uid != 0 && uid != new_parent_meta.uid {
            if let Ok(file_inode) = new_parent.find(&new_leaf) {
                if let Ok(file_meta) = file_inode.metadata() {
                    if uid != file_meta.uid {
                        return -(SyscallErr::EPERM as isize);
                    }
                }
            }
        }
    }

    // another_ext4 serializes namespace metadata with a transaction gate;
    // a concurrent commit can make rename transiently return EAGAIN.  The
    // blocking rename syscall must wait and retry outside the VFS locks.
    wait_io_core(
        || match old_parent.rename(&old_leaf, &new_parent, &new_leaf, flags) {
            Ok(_) => SUCCESS,
            Err(e) => -(e as isize),
        },
        false,
    )
}
