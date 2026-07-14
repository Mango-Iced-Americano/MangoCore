use super::common::*;

pub fn sys_mknodat(dirfd: usize, path: *const u8, mode: u32, dev: usize) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path_str = match UserCString::from_addr(path as usize).read(token) {
        Ok(s) => s,
        Err(_) => return EFAULT,
    };
    if path_str.is_empty() {
        return ENOENT;
    }
    // Linux: path components must not exceed NAME_MAX (255) — ENAMETOOLONG before any other check
    if let Err(errno) = validate_path_len(&path_str) {
        return errno;
    }
    let start = if path_str.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(e) => return e,
        }
    };
    let (parent, leaf) = match vfs_lookup_parent_for_start(&start, &path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // EROFS takes priority over EACCES: check read-only mount before DAC
    if let Some(mnt) = parent.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }
    // Check write+search permission on parent directory (non-root only)
    let (uid, fsgid, groups) = caller_ids_and_groups();
    if uid != 0 {
        if let Err(errno) = check_parent_write_search_access(&parent, uid, fsgid, &groups) {
            return errno;
        }
    }
    let file_type = match vfs::InodeMode::from_bits_truncate(mode) & vfs::InodeMode::S_IFMT {
        m if m == vfs::InodeMode::S_IFIFO => FileType::Pipe,
        m if m == vfs::InodeMode::S_IFBLK => FileType::BlockDevice,
        m if m == vfs::InodeMode::S_IFCHR => FileType::CharDevice,
        m if m == vfs::InodeMode::S_IFSOCK => FileType::Socket,
        m if m == vfs::InodeMode::S_IFREG || m.is_empty() => FileType::File,
        m if m == vfs::InodeMode::S_IFDIR => return EINVAL,
        _ => return EINVAL,
    };
    // CAP_MKNOD proxy: block/char devices require root (MangoCore has no capability system)
    if (file_type == FileType::BlockDevice || file_type == FileType::CharDevice) && uid != 0 {
        return EPERM;
    }
    let perm = apply_current_umask(vfs::InodeMode::from_bits_truncate(mode));
    // Only pass device number for CHR/BLK; FIFO/socket use 0
    let rdev = if file_type == FileType::CharDevice || file_type == FileType::BlockDevice {
        dev
    } else {
        0
    };
    let child_gid = if let Ok(parent_meta) = parent.metadata() {
        if parent_meta.mode.contains(vfs::InodeMode::S_ISGID) {
            parent_meta.gid
        } else {
            fsgid
        }
    } else {
        fsgid
    };
    match parent.create_with_data(&leaf, file_type, perm, rdev) {
        Ok(inode) => {
            if let Ok(mut child_meta) = inode.metadata() {
                child_meta.uid = uid;
                child_meta.gid = child_gid;
                let _ = inode.set_metadata(&child_meta);
            }
            SUCCESS
        }
        Err(e) => -(e as isize),
    }
}
