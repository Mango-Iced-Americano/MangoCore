use super::common::*;

pub fn sys_linkat(
    olddirfd: usize,
    oldpath: *const u8,
    newdirfd: usize,
    newpath: *const u8,
    flags: u32,
) -> isize {
    if flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
        return EINVAL;
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();

    let oldpath_str = match user_cstring(token, oldpath) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let newpath_str = match user_cstring(token, newpath) {
        Ok(s) => s,
        Err(e) => return e,
    };

    if let Err(errno) = validate_path_len(&oldpath_str) {
        return errno;
    }
    if let Err(errno) = validate_path_len(&newpath_str) {
        return errno;
    }

    log::info!(
        "[sys_linkat] old: dirfd={} path={}, new: dirfd={} path={}",
        olddirfd as isize,
        oldpath_str,
        newdirfd as isize,
        newpath_str
    );

    // Linux: when path is absolute, dirfd is ignored — skip fd resolution
    let old_start = if oldpath_str.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(olddirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    if oldpath_str.is_empty() {
        return ENOENT;
    }

    // Permission: search access on old path parent directories
    let (uid, fsgid, groups) = caller_ids_and_groups();
    let old_perm = check_parent_search_access(&old_start, &oldpath_str, uid, fsgid, &groups);
    if old_perm != SUCCESS {
        return old_perm;
    }

    // 查找已存在的 inode
    let follow = (flags & AT_SYMLINK_FOLLOW) != 0;
    let existing = match crate::fs::vfs_lookup(&old_start, &oldpath_str, follow) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    // 解析新路径：获取父目录 + 叶子名
    // Linux: when path is absolute, dirfd is ignored — skip fd resolution
    let new_start = if newpath_str.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(newdirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    // Permission: search access on new path parent directories
    let new_perm = check_parent_search_access(&new_start, &newpath_str, uid, fsgid, &groups);
    if new_perm != SUCCESS {
        return new_perm;
    }

    // Resolve target parent directory and leaf name
    let components = crate::fs::parse_path(&newpath_str);
    let leaf = if let Some(n) = components.last() {
        n.clone()
    } else {
        return ENOENT;
    };

    let parent_dir = if components.len() == 1 {
        if newpath_str.starts_with('/') {
            crate::fs::current_root_inode()
        } else {
            new_start
        }
    } else {
        let parent_comps = &components[..components.len() - 1];
        let joined = parent_comps
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join("/");
        let parent_path = if newpath_str.starts_with('/') {
            if joined.is_empty() {
                String::from("/")
            } else {
                alloc::format!("/{}", joined)
            }
        } else {
            joined
        };
        match crate::fs::vfs_lookup(&new_start, &parent_path, true) {
            Ok(parent) => parent,
            Err(errno) => return errno,
        }
    };

    // Check if leaf already exists in parent directory (mandated by Linux link(2): EEXIST)
    // Use list_dirents() instead of find() — find can miss entries in some VFS edge cases
    if let Ok(entries) = parent_dir.list_dirents() {
        for (name, _ino, _ftype) in &entries {
            if name == &leaf {
                return EEXIST;
            }
        }
    }

    // Check write+search permission on target parent directory
    if let Err(errno) = check_parent_write_search_access(&parent_dir, uid, fsgid, &groups) {
        return errno;
    }

    // 禁止创建目录的硬链接（POSIX 不允许，除 root 外）
    // NOTE: This check must happen AFTER new path resolution (per Linux vfs_link ordering),
    // so that a bad newdirfd gets EBADF, not EPERM.
    let meta = match existing.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    if meta.file_type == crate::fs::vfs::FileType::Dir {
        return EPERM;
    }

    // Hard-link creation is another namespace metadata mutation and can
    // observe the same transient another_ext4 transaction contention.
    wait_io_core(
        || match parent_dir.link(&leaf, &existing) {
            Ok(_) => SUCCESS,
            Err(e) => -(e as isize),
        },
        false,
    )
}
