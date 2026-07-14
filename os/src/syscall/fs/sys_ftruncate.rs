use super::common::*;

pub fn sys_ftruncate(fd: usize, length: isize) -> isize {
    if length < 0 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let inode = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(fd) {
            Ok(file) => file,
            Err(e) => return -(e as isize),
        };
        if is_path_fd(&file) {
            return EBADF;
        }
        if file.is_dir() {
            return EISDIR;
        }
        if matches!(file.file_type(), vfs::FileType::Pipe | vfs::FileType::Socket) {
            return EINVAL;
        }
        if !file.flags().is_writable() {
            return EINVAL;
        }
        if let Err(errno) = check_memfd_truncate_seals(&*file, length as usize) {
            return errno;
        }
        file.inode.clone()
    };
    // RLIMIT_FSIZE check
    let fsize_limit = {
        let inner = task.acquire_inner_lock();
        inner.fsize_limit_cur
    };
    if (length as usize) > fsize_limit {
        return EFBIG;
    }
    match inode.resize(length as usize) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}
