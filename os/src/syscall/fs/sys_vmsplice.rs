use super::common::*;

pub fn sys_vmsplice(fd: usize, iov: usize, iovcnt: usize, flags: u32) -> isize {
    if flags & !VMSPLICE_VALID_FLAGS != 0 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    // vmsplice: fd must be a pipe (non-pipe → EBADF per Linux)
    if file.file_type() != FileType::Pipe {
        return EBADF;
    }
    if file.writable().is_err() {
        return EBADF;
    }

    let token = task.get_user_token();
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        iov as *const crate::fs::iov::IOVec,
        iovcnt,
        crate::hal::MAX_RW_COUNT,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };
    let user_buf = match user_iov.reader_buffer() {
        Ok(buffer) => buffer,
        Err(errno) => return errno,
    };
    let mut kernel_buf = alloc::vec![0u8; user_buf.len()];
    let copied = match user_buf.read_into(&mut kernel_buf) {
        Ok(copied) => copied,
        Err(errno) => return errno,
    };
    kernel_buf.truncate(copied);

    let mut try_write = || match file.write(&kernel_buf) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    };
    let is_nonblock = file.is_nonblock() || flags & SPLICE_F_NONBLOCK != 0;
    if is_nonblock {
        return try_write();
    }
    if let Some(wq) = file.inode.write_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = try_write();
            if ret == -(SyscallErr::EAGAIN as isize) {
                None
            } else {
                Some(ret)
            }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => crate::task::RestartKind::RestartSys.syscall_result(),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        try_write()
    }
}
