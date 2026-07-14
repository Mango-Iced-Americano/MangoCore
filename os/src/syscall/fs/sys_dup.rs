use super::common::*;

pub fn sys_dup(oldfd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    let file = match fd_table.get_file(oldfd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let newfd = match fd_table.alloc_fd(file, false) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };
    info!("[sys_dup] oldfd: {}, newfd: {}", oldfd, newfd);
    newfd as isize
}
