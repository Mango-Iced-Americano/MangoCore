use super::common::*;

pub fn sys_tee(fd_in: usize, fd_out: usize, len: usize, flags: u32) -> isize {
    // tee accepts the same flags as splice (Linux 6.6: flags & ~SPLICE_F_ALL)
    if flags & !SPLICE_VALID_FLAGS != 0 {
        return EINVAL;
    }

    // len == 0 → return 0 before fd-table access (Linux 6.6 ordering)
    if len == 0 {
        return 0;
    }

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(fd_in) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(fd_out) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    drop(fd_table);

    // tee requires BOTH fds to be pipes
    if in_file.file_type() != FileType::Pipe || out_file.file_type() != FileType::Pipe {
        return EINVAL;
    }

    // Check readability/writability
    if in_file.readable().is_err() || out_file.writable().is_err() {
        return EBADF;
    }

    // Same-pipe identity: tee duplicates across pipes, not within
    if is_same_pipe(&in_file, &out_file) {
        return EINVAL;
    }

    let nonblock =
        flags & SPLICE_F_NONBLOCK != 0 || in_file.is_nonblock() || out_file.is_nonblock();

    // Downcast to Pipe for direct peek/write access
    use crate::fs::dev::pipe::Pipe;
    let in_pipe = match in_file.inode.as_any_ref().downcast_ref::<Pipe>() {
        Some(p) => p,
        None => return EINVAL,
    };
    let _out_pipe = match out_file.inode.as_any_ref().downcast_ref::<Pipe>() {
        Some(p) => p,
        None => return EINVAL,
    };

    const BUFFER_SIZE: usize = 4096;
    let mut kbuf = alloc::vec![0u8; len.min(BUFFER_SIZE)];

    // Step 1: Peek data from source pipe (non-consuming read)
    let peek_n = in_pipe.peek_at(&mut kbuf);
    let actual = len.min(peek_n).min(kbuf.len());

    // Step 2: If no data available
    if actual == 0 {
        // Closed/EOF: all write ends gone, pipe empty → return 0, not EAGAIN
        if in_pipe.buffer_arc().lock().all_write_ends_closed() {
            return 0;
        }
        if nonblock {
            return -(SyscallErr::EAGAIN as isize);
        }
        // Blocking path: wait for data on source pipe read_wait_queue
        if let Some(wq) = in_file.inode.read_wait_queue() {
            let mut found: Option<usize> = None;
            let wait_ret = WaitQueue::wait_until_interruptible(wq, || {
                let n = in_pipe.peek_at(&mut kbuf);
                let a = len.min(n).min(kbuf.len());
                if a > 0 {
                    found = Some(a);
                    return Some(0); // signal ready
                }
                // EOF: all write ends closed and pipe empty
                if in_pipe.buffer_arc().lock().all_write_ends_closed() {
                    found = Some(0);
                    return Some(0);
                }
                None // keep waiting
            });
            match wait_ret {
                WaitResult::Ready(_) => {
                    let n = found.unwrap_or(0);
                    if n == 0 {
                        return 0;
                    }
                    return tee_write_out(
                        &out_file, &mut kbuf[..n], nonblock,
                    );
                }
                WaitResult::Interrupted => return -(SyscallErr::ERESTART as isize),
                WaitResult::TimedOut => return -(SyscallErr::EAGAIN as isize),
            }
        }
        return 0;
    }

    // Step 3: Write to destination pipe
    tee_write_out(&out_file, &mut kbuf[..actual], nonblock)
}

/// Write kernel buffer to destination pipe.  Handles blocking/nonblocking
/// and ERESTART conversion.
fn tee_write_out(
    out_file: &crate::fs::vfs::File,
    data: &mut [u8],
    nonblock: bool,
) -> isize {
    let mut try_write = || -> Result<usize, SyscallErr> { out_file.write(data) };
    if nonblock {
        return match try_write() {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        };
    }
    if let Some(wq) = out_file.inode.write_wait_queue() {
        let mut found: Option<isize> = None;
        let wait_ret = WaitQueue::wait_until_interruptible(wq, || {
            match try_write() {
                Ok(n) => {
                    found = Some(n as isize);
                    Some(0)
                }
                Err(SyscallErr::EAGAIN) => None,
                Err(e) => {
                    found = Some(-(e as isize));
                    Some(0)
                }
            }
        });
        match wait_ret {
            WaitResult::Ready(_) => found.unwrap_or(0),
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        match try_write() {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        }
    }
}
