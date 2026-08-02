use super::common::*;

pub fn sys_pwrite(fd: usize, buf: usize, count: usize, offset: usize) -> isize {
    let mut count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if offset_is_negative(offset) {
        return EINVAL;
    }
    if is_stream_file(&file) {
        return ESPIPE;
    }
    // fd is not open for writing
    if file.writable().is_err() {
        return EBADF;
    }
    let fsize_limit = task.acquire_inner_lock().fsize_limit_cur;
    count = match apply_fsize_limit(
        &file,
        count,
        pwrite_start_offset(&file, offset),
        fsize_limit,
    ) {
        Ok(count) => count,
        Err(errno) => return errno,
    };
    let token = task.get_user_token();
    pwrite_from_user(&file, token, buf, count, offset)
}
