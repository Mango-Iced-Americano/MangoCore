use super::common::*;

pub fn sys_fstat(fd: usize, statbuf: *mut u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();

    info!("[sys_fstat] fd: {}", fd);
    let file = match fd {
        AT_FDCWD => task.process.fs().lock().working_inode.clone(),
        fd => {
            let files_ref = task.process.files();
        let fd_table = files_ref.lock();
            match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            }
        }
    };
    let stat = match file.metadata() {
        Ok(meta) => metadata_to_stat(&meta),
        Err(e) => return -(e as isize),
    };
    if UserPtrMut::new(statbuf as *mut Stat).write(token, &stat).is_err() {
        log::error!("[sys_fstat] Failed to copy to {:?}", statbuf);
        return EFAULT;
    };
    SUCCESS
}
