use super::common::*;

pub fn sys_openat(dirfd: usize, path: *const u8, flags: u32, mode: u32) -> isize {
    let mode_bits = mode;
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    let flags = match OpenFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_openat] unknown flags");
            return EINVAL;
        }
    };
    let _mode = StatMode::from_bits(mode);
    info!(
        "[sys_openat] dirfd: {}, path: {}, flags: {:?}, mode: {:?}",
        dirfd as isize, path, flags, _mode
    );
    if let Some(result) = open_proc_self_fd(&path, flags) {
        let new_file = match result {
            Ok(file) => file,
            Err(errno) => return errno,
        };
        let files_ref = task.process.files();
        let mut fd_table = files_ref.lock();
        return match fd_table.alloc_fd(new_file, flags.contains(OpenFlags::O_CLOEXEC)) {
            Ok(fd) => fd as isize,
            Err(e) => -(e as isize),
        };
    }
    let create_mode = vfs::InodeMode::from_bits_truncate(mode_bits);
    let new_file = match open_file_at(dirfd, &path, flags, create_mode) {
        Ok(file) => file,
        Err(errno) => return errno,
    };

    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    let new_fd = match fd_table.alloc_fd(new_file, flags.contains(OpenFlags::O_CLOEXEC)) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };
    new_fd as isize
}
