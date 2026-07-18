use super::common::*;

pub fn sys_close(fd: usize) -> isize {
    info!("[sys_close] fd: {}", fd);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    match fd_table.drop_fd(fd) {
        Ok(file) => {
            let mut flock_releases = Vec::new();
            record_flock_close(&mut flock_releases, &file);
            drop(fd_table);
            release_closed_flock_descriptions(flock_releases);
            SUCCESS
        }
        Err(e) => -(e as isize),
    }
}
