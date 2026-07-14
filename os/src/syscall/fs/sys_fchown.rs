use super::common::*;

pub fn sys_fchown(fd: usize, owner: u32, group: u32) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(_) => return EBADF,
    };
    // O_PATH fd: operations other than close() fail with EBADF
    if is_path_fd(&file) {
        return EBADF;
    }
    // Clone inode and drop lock before metadata operations
    let inode = file.inode.clone();
    drop(fd_table);
    // Check read-only filesystem (must precede EPERM per Linux semantics:
    // EROFS takes priority over EPERM)
    if let Some(mnt) = inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }

    do_chown(&inode, owner, group)
}
