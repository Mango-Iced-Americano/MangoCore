use super::common::*;

pub fn sys_fadvise64(fd: usize, offset: usize, len: usize, advice: i32) -> isize {
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

    if offset_is_negative(offset) || offset_is_negative(len) {
        return EINVAL;
    }
    if !(0..=5).contains(&advice) {
        return EINVAL;
    }
    if is_stream_file(&file) {
        return ESPIPE;
    }

    SUCCESS
}
