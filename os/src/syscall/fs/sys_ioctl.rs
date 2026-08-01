use super::common::*;

pub fn sys_ioctl(fd: usize, cmd: u32, arg: usize) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // file 是 Arc；faultable uaccess 前释放 fd table，避免把全进程描述符锁带入 VM fault。
    drop(fd_table);
    if is_path_fd(&file) {
        return EBADF;
    }

    if cmd == FIONREAD {
        // Let inode try first (PTY uses internal buffer size)
        match file.inode.ioctl(cmd, arg, file.private_data()) {
            Ok(n) => return n as isize,
            Err(SyscallErr::ENOSYS) => { /* fall through */ }
            Err(e) => return -(e as isize),
        }
        let md = match file.metadata() {
            Ok(m) => m,
            Err(e) => return -(e as isize),
        };
        let remaining = (md.size as usize).saturating_sub(file.offset());
        let val = remaining.min(i32::MAX as usize) as i32;
        if crate::mm::copy_to_user(token, &val, arg as *mut i32).is_err() {
            return EFAULT;
        }
        return 0;
    }

    if cmd == FIONBIO {
        let arg_ptr = arg as *mut i32;
        if arg_ptr.is_null() {
            return EFAULT;
        }
        let value = match crate::mm::get_from_user(token, arg_ptr) {
            Ok(value) => value,
            Err(_) => return EFAULT,
        };
        file.set_nonblock(value != 0);
        return 0;
    }

    match file.inode.ioctl(cmd, arg, file.private_data()) {
        Ok(n) => n as isize,
        Err(SyscallErr::ENOSYS) => ENOTTY,
        Err(e) => -(e as isize),
    }
}
