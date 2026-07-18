use super::common::*;

pub fn sys_pread(fd: usize, buf: usize, count: usize, offset: usize) -> isize {
    let count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    // fd is not open for reading
    if file.readable().is_err() {
        return EBADF;
    }
    if offset_is_negative(offset) {
        return EINVAL;
    }
    let token = task.get_user_token();
    pread_into_user(&file, token, buf, count, offset)
}
