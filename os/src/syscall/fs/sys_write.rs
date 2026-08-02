use super::common::*;

pub fn sys_write(fd: usize, buf: usize, count: usize) -> isize {
    // Foreground split bucket (a): fd table lookup + fsize limit preparation.
    // Recorded once per syscall just before the write dispatch, aligned with
    // the existing pwrite boundary counters (perf_diag diagnostic only).
    let _prep_start = crate::task::perf::perf_memory_io_time_now();
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
    if file.writable().is_err() {
        return EBADF;
    }
    if file.is_dev_null() || file.is_dev_zero() {
        return count as isize;
    }
    let fsize_limit = task.acquire_inner_lock().fsize_limit_cur;
    count = match apply_fsize_limit(&file, count, write_start_offset(&file), fsize_limit) {
        Ok(count) => count,
        Err(errno) => return errno,
    };
    let is_nonblock = file.is_nonblock();
    let token = task.get_user_token();
    crate::task::perf::record_write_fd_prep(
        crate::task::perf::perf_memory_io_time_now().wrapping_sub(_prep_start),
    );
    if is_nonblock {
        write_from_user(&file, token, buf, count)
    } else if let Some(wq) = file.inode.write_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = write_from_user(&file, token, buf, count);
            if ret == -(SyscallErr::EAGAIN as isize) {
                None
            } else {
                Some(ret)
            }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        // Regular files: skip wait_io_core() overhead.
        write_from_user(&file, token, buf, count)
    }
}
