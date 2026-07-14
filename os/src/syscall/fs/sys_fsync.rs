use super::common::*;

pub fn sys_fsync(fd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if !matches!(file.file_type(), FileType::File | FileType::Dir | FileType::BlockDevice) {
        return EINVAL;
    }
    drop(fd_table);
    match file.inode.sync() {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}
