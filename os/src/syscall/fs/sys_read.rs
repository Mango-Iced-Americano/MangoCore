use super::common::*;

pub fn sys_read(fd: usize, buf: usize, count: usize) -> isize {
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
    if file.readable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    if file.is_dev_null() {
        return 0;
    }
    if file.is_dev_zero() {
        return read_zero_into_user(token, buf, count);
    }
    let is_nonblock = file.is_nonblock();
    if is_nonblock {
        read_into_user(&file, token, buf, count)
    } else if let Some(wq) = file.inode.read_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = read_into_user(&file, token, buf, count);
            if ret == -(SyscallErr::EAGAIN as isize) { None } else { Some(ret) }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        // Regular files: no WaitQueue, I/O completes immediately — skip
        // wait_io_core() overhead (poll/yield loop not needed for PageCache).
        read_into_user(&file, token, buf, count)
    }
}
