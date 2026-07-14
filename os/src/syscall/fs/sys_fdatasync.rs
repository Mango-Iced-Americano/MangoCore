use super::common::*;

pub fn sys_fdatasync(fd: usize) -> isize {
    let task = current_task().unwrap();

    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if is_path_fd(&file) {
        return EBADF;
    }
    drop(fd_table);

    if !matches!(file.file_type(), FileType::File | FileType::Dir | FileType::BlockDevice) {
        return EINVAL;
    }

    match file.inode.datasync() {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}
