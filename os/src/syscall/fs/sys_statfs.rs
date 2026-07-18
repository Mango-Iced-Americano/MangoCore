use super::common::*;

pub fn sys_statfs(pathname: *const u8, buf: *mut Statfs) -> isize {
    let token = current_user_token();
    let path = match user_cstring(token, pathname) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if path.is_empty() {
        return ENOENT;
    }
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    let start = current_task()
        .map(|t| t.process.fs().lock().working_inode.inode.clone())
        .unwrap_or_else(|| crate::fs::vfs_root().mountpoint_root_inode());

    // DAC search permission check: search (x) on every path component
    // Linux requires this for path-based statfs, same as path-based stat
    let (uid, fsgid, groups) = caller_ids_and_groups();
    let perm_err = check_parent_search_access(&start, &path, uid, fsgid, &groups);
    if perm_err != SUCCESS {
        return perm_err;
    }

    let inode = match crate::fs::vfs_lookup(&start, &path, true) {
        Ok(inode) => inode,
        Err(e) => return e,
    };
    let fs = inode.fs();
    let sb = match fs.statfs(&inode) {
        Ok(sb) => sb,
        Err(e) => return -(e as isize),
    };
    let statfs = superblock_to_statfs(&sb);
    write_statfs(buf, &statfs)
}
