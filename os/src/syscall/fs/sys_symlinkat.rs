use super::common::*;

pub fn sys_symlinkat(target: *const u8, newdirfd: usize, linkpath: *const u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();

    let target_str = match user_cstring(token, target) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let linkpath_str = match user_cstring(token, linkpath) {
        Ok(s) => s,
        Err(e) => return e,
    };

    if let Err(errno) = validate_path_len(&target_str) {
        return errno;
    }
    if let Err(errno) = validate_path_len(&linkpath_str) {
        return errno;
    }

    log::info!(
        "[sys_symlinkat] target: {}, newdirfd: {}, linkpath: {}",
        target_str,
        newdirfd as isize,
        linkpath_str
    );

    let start = if linkpath_str.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(newdirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    let (uid, fsgid, groups) = caller_ids_and_groups();
    let search_result = check_parent_search_access(&start, &linkpath_str, uid, fsgid, &groups);
    if search_result != SUCCESS {
        return search_result;
    }

    let components = crate::fs::parse_path(&linkpath_str);
    let leaf = match components.last() {
        Some(n) => n.clone(),
        None => return ENOENT,
    };

    let parent_dir = if components.len() == 1 {
        if linkpath_str.starts_with('/') {
            crate::fs::current_root_inode()
        } else {
            start
        }
    } else {
        let parent_comps = &components[..components.len() - 1];
        let joined = parent_comps
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join("/");
        let parent_path = if linkpath_str.starts_with('/') {
            if joined.is_empty() {
                String::from("/")
            } else {
                alloc::format!("/{}", joined)
            }
        } else {
            joined
        };
        match crate::fs::vfs_lookup(&start, &parent_path, true) {
            Ok(parent) => parent,
            Err(errno) => return errno,
        }
    };

    if let Err(errno) = check_parent_write_search_access(&parent_dir, uid, fsgid, &groups) {
        return errno;
    }

    // Inherit GID if parent has S_ISGID (like mkdirat)
    let child_gid = if let Ok(parent_meta) = parent_dir.metadata() {
        if parent_meta.mode.contains(vfs::InodeMode::S_ISGID) {
            parent_meta.gid
        } else {
            fsgid
        }
    } else {
        fsgid
    };
    match parent_dir.symlink_with_attrs(
        &leaf,
        &target_str,
        vfs::CreateAttrs {
            mode: vfs::InodeMode::S_IRWXUGO,
            uid,
            gid: child_gid,
        },
    ) {
        Ok(_) => SUCCESS,
        Err(error) => -(error as isize),
    }
}
