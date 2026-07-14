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
    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(e) => return e,
    };
    let (parent, leaf) = match vfs_lookup_parent_for_start(&start, &path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let file_type = match vfs::InodeMode::from_bits_truncate(mode) & vfs::InodeMode::S_IFMT {
        m if m == vfs::InodeMode::S_IFIFO => FileType::Pipe,
        m if m == vfs::InodeMode::S_IFBLK => FileType::BlockDevice,
        m if m == vfs::InodeMode::S_IFCHR => FileType::CharDevice,
        m if m == vfs::InodeMode::S_IFSOCK => FileType::Socket,
        m if m == vfs::InodeMode::S_IFREG || m.is_empty() => FileType::File,
        m if m == vfs::InodeMode::S_IFDIR => return EINVAL,
        _ => return EINVAL,
    };
    let perm = apply_current_umask(vfs::InodeMode::from_bits_truncate(mode));
    // Only pass device number for CHR/BLK; FIFO/socket use 0
    let rdev = if file_type == FileType::CharDevice || file_type == FileType::BlockDevice {
        dev
    } else {
        0
    };
    match parent.create_with_data(&leaf, file_type, perm, rdev) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}
