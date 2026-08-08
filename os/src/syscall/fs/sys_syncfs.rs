use super::common::*;

pub fn sys_syncfs(fd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(_e) => return EBADF,
    };
    if is_path_fd(&file) {
        return EBADF;
    }
    drop(fd_table);

    // Resolve the filesystem selected by this fd and delegate the durability
    // contract through VFS. Backends with instance-local registries avoid a
    // global flush; legacy backends retain the default compatibility path.
    let inode = vfs::MountFSInode::unwrap_inode(&file.inode);
    let fs = inode.fs();
    match fs.sync() {
        Ok(()) => SUCCESS,
        Err(error) => -(error as isize),
    }
}
