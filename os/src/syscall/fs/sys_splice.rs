use super::common::*;
use crate::fs::dev::pipe::{send_sigpipe_to_current, Pipe, PipeRingBuffer};
use crate::fs::vfs::event::EPollEvent;
use crate::fs::vfs::index_node::IndexNode;
use spin::Mutex;

pub fn sys_splice(
    fd_in: usize,
    off_in: *mut usize,
    fd_out: usize,
    off_out: *mut usize,
    len: usize,
    flags: u32,
) -> isize {
    // ── Linux 6.6: len==0 returns 0 before any other check ────────────
    if len == 0 {
        return 0;
    }

    // ── Flag validation ────────────────────────────────────────────────
    if flags & !SPLICE_VALID_FLAGS != 0 {
        return EINVAL;
    }

    // ── FD lookup ──────────────────────────────────────────────────────
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
    if is_path_fd(&in_file) || is_path_fd(&out_file) {
        return EBADF;
    }
    drop(fd_table);

    info!("[sys_splice] outfd: {}, in_fd: {}", fd_out, fd_in);
    if in_file.readable().is_err() || out_file.writable().is_err() {
        return EBADF;
    }

    // ── Pipe validation: at least one fd must be a pipe ────────────────
    let in_pipe = in_file.file_type() == FileType::Pipe;
    let out_pipe = out_file.file_type() == FileType::Pipe;
    if !in_pipe && !out_pipe {
        return EINVAL;
    }

    // ── ESPIPE before same-pipe (Linux 6.6 order) ──────────────────────
    // Pipe fds must not have non-NULL offset pointer.
    if in_pipe && !off_in.is_null() {
        return ESPIPE;
    }
    if out_pipe && !off_out.is_null() {
        return ESPIPE;
    }

    // ── Same-pipe identity check ───────────────────────────────────────
    // Reject splice from a pipe onto itself via the shared ring buffer.
    if in_pipe && out_pipe && is_same_pipe(&in_file, &out_file) {
        return EINVAL;
    }

    // ── Read user-space offsets ────────────────────────────────────────
    let token = task.get_user_token();
    let mut off_in_val = match read_user_off(off_in, token) {
        Ok(opt) => opt,
        Err(errno) => return errno,
    };
    let mut off_out_val = match read_user_off(off_out, token) {
        Ok(opt) => opt,
        Err(errno) => return errno,
    };

    let nonblock =
        flags & SPLICE_F_NONBLOCK != 0 || in_file.is_nonblock() || out_file.is_nonblock();

    // ── Pipe→pipe: atomic ring-buffer transfer (no data loss) ──────────
    if in_pipe && out_pipe {
        // off_in / off_out must be NULL (validated by ESPIPE above).
        let p1 = in_file.inode.as_any_ref().downcast_ref::<Pipe>().unwrap();
        let p2 = out_file.inode.as_any_ref().downcast_ref::<Pipe>().unwrap();
        return splice_pipe_to_pipe(p1, p2, len, nonblock);
    }

    // ── File↔pipe: write-first staging buffer ─────────────────────────
    //
    // For file sources with an explicit *off_in, the offset is advanced
    // only after the target write succeeds — never speculatively in the
    // read path.  This prevents the file offset from advancing past data
    // that was never actually transferred.
    //
    // File writes are synchronous (regular files don't return EAGAIN),
    // so pipe→file with deferred offset advancement is data-safe.
    const BUF_CAP: usize = 4096;

    let mut buffer = Vec::<u8>::with_capacity(BUF_CAP);
    let mut buf_len: usize = 0;
    let mut buf_off: usize = 0;
    let mut total_sent: usize = 0;

    loop {
        // ── Write phase: flush buffered data first ─────────────────────
        if buf_off < buf_len {
            let chunk = &buffer[buf_off..buf_len];
            let wrote = match off_out_val.as_mut() {
                Some(off) => {
                    match out_file
                        .inode
                        .write_at(*off, chunk.len(), chunk, out_file.private_data())
                    {
                        Ok(n) => {
                            *off += n;
                            n
                        }
                        Err(_e) => break,
                    }
                }
                None => match splice_write_stream(&out_file, chunk, nonblock) {
                    Ok(n) => n,
                    Err(_errno) => break,
                },
            };

            if wrote == 0 {
                if total_sent > 0 {
                    break;
                }
                break;
            }

            buf_off += wrote;
            total_sent += wrote;

            // Commit source file offset only after bytes land in target.
            if let Some(ref mut off) = off_in_val {
                *off += wrote;
            }

            continue;
        }

        // ── Read phase: buffer empty, refill from source ───────────────
        if total_sent >= len {
            break;
        }

        let remaining = len - total_sent;
        let read_limit = core::cmp::min(remaining, BUF_CAP);

        unsafe {
            buffer.set_len(read_limit);
        }

        let n = {
            if let Some(ref off) = off_in_val {
                match in_file.inode.read_at(
                    *off,
                    read_limit,
                    buffer.as_mut_slice(),
                    in_file.private_data(),
                ) {
                    Ok(n) => n,
                    Err(_e) => {
                        if total_sent > 0 {
                            break;
                        }
                        return -(_e as isize);
                    }
                }
            } else {
                match splice_read_stream(&in_file, buffer.as_mut_slice(), nonblock) {
                    Ok(n) => n,
                    Err(errno) => {
                        if total_sent > 0 {
                            break;
                        }
                        return errno;
                    }
                }
            }
        };

        if n == 0 {
            break;
        }

        unsafe {
            buffer.set_len(n);
        }
        buf_len = n;
        buf_off = 0;
    }

    // ── Write back user-space offsets ──────────────────────────────────
    if let Some(offset) = off_in_val {
        if UserPtrMut::new(off_in).write(token, &offset).is_err() {
            return EFAULT;
        }
    }
    if let Some(offset) = off_out_val {
        if UserPtrMut::new(off_out).write(token, &offset).is_err() {
            return EFAULT;
        }
    }

    info!("[sys_splice] sent bytes: {}", total_sent);
    total_sent as isize
}

// ── Pipe→pipe atomic transfer helpers ──────────────────────────────────

/// Lock both ring buffers in a deterministic order (by backing-buffer
/// address), then move up to `max_bytes` from `src` into `dst` atomically.
///
/// Decision order (Linux 6.6 priority):
/// 1. Source EOF (empty + all write-ends closed) → `Ok(0)`.
///    EOF takes priority over EPIPE because nothing can ever be transferred.
/// 2. Destination has no readers → `Err(EPIPE)`.
///    EPIPE is checked only when source data is available or writers are
///    still open — never when the source is at EOF.  Source data is NOT
///    consumed on EPIPE because the check happens before splice_move.
/// 3. Transfer data → `Ok(n)` (n > 0).
/// 4. Non-terminal → `Err(EAGAIN)`.
///    (source empty with writers open, or destination full with readers)
fn splice_atomic_single(
    buf1: &Arc<Mutex<PipeRingBuffer>>,
    buf2: &Arc<Mutex<PipeRingBuffer>>,
    max_bytes: usize,
) -> Result<usize, isize> {
    let ptr1 = Arc::as_ptr(buf1) as usize;
    let ptr2 = Arc::as_ptr(buf2) as usize;

    // Same Arc is impossible here — same-pipe is already rejected.
    if ptr1 < ptr2 {
        let mut r1 = buf1.lock();
        let mut r2 = buf2.lock();
        // 1. Source EOF — terminal, no transfer to attempt.
        if r1.get_used_size() == 0 && r1.all_write_ends_closed() {
            return Ok(0);
        }
        // 2. Destination has no readers — EPIPE, source data NOT consumed.
        if r2.all_read_ends_closed() {
            return Err(EPIPE);
        }
        // 3. Try transfer.
        let n = PipeRingBuffer::splice_move(&mut r1, &mut r2, max_bytes);
        if n > 0 {
            return Ok(n);
        }
        // 4. Non-terminal.
        Err(EAGAIN)
    } else {
        let mut r2 = buf2.lock();
        let mut r1 = buf1.lock();
        if r1.get_used_size() == 0 && r1.all_write_ends_closed() {
            return Ok(0);
        }
        if r2.all_read_ends_closed() {
            return Err(EPIPE);
        }
        let n = PipeRingBuffer::splice_move(&mut r1, &mut r2, max_bytes);
        if n > 0 {
            return Ok(n);
        }
        Err(EAGAIN)
    }
}

/// Notify the peer ends after a successful splice transfer.
fn notify_pipe_splice(p1: &Pipe, p2: &Pipe) {
    if let Some(we) = p1.peer_write_end() {
        we.write_wait
            .notify_events_at_most(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM, 1);
    }
    if let Some(re) = p2.peer_read_end() {
        re.read_wait
            .notify_events_at_most(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM, 1);
    }
}

/// Pipe→pipe splice: atomic transfer with per-call blocking when
/// `nonblock` is false.
fn splice_pipe_to_pipe(p1: &Pipe, p2: &Pipe, len: usize, nonblock: bool) -> isize {
    let buf1 = p1.buffer_arc();
    let buf2 = p2.buffer_arc();

    // ── Nonblocking path: single-shot ──────────────────────────────────
    if nonblock {
        match splice_atomic_single(buf1, buf2, len) {
            Ok(0) => 0, // EOF — source empty, all writers closed
            Ok(n) => {
                notify_pipe_splice(p1, p2);
                n as isize
            }
            Err(EAGAIN) => EAGAIN,
            Err(EPIPE) => {
                send_sigpipe_to_current();
                EPIPE
            }
            Err(e) => e,
        }
    } else {
        // ── Blocking path: wait for progress ───────────────────────────
        let dst_wq = p2.write_wait_queue().unwrap();
        let src_wq = p1.read_wait_queue().unwrap();

        loop {
            match splice_atomic_single(buf1, buf2, len) {
                Ok(0) => return 0, // EOF — signal does not preempt
                Ok(n) => {
                    notify_pipe_splice(p1, p2);
                    return n as isize;
                }
                Err(EPIPE) => {
                    send_sigpipe_to_current();
                    return EPIPE;
                }
                Err(EAGAIN) => {
                    // Non-terminal: no progress.  Check signals before waiting.
                    if let Some(task) = current_task() {
                        if crate::task::has_actionable_signal(&task) {
                            return ERESTART;
                        }
                    }
                }
                Err(e) => return e,
            }

            // Wait on destination write-queue (typical bottleneck).
            // TimedOut falls through to src-wait; Ready/Interrupted return.
            match WaitQueue::wait_until_interruptible(dst_wq, || {
                match splice_atomic_single(buf1, buf2, len) {
                    Ok(0) => Some(0),          // EOF
                    Ok(n) => Some(n as isize), // progress
                    Err(EPIPE) => Some(EPIPE),
                    Err(EAGAIN) => None, // non-terminal — keep waiting
                    Err(e) => Some(e),
                }
            }) {
                WaitResult::Ready(n) => {
                    if n > 0 {
                        notify_pipe_splice(p1, p2);
                    } else if n == EPIPE {
                        send_sigpipe_to_current();
                    }
                    return n;
                }
                WaitResult::Interrupted => return ERESTART,
                WaitResult::TimedOut => { /* dst_wq timed out — try src_wq */ }
            }

            // Re-check signals between waits.
            if let Some(task) = current_task() {
                if crate::task::has_actionable_signal(&task) {
                    return ERESTART;
                }
            }

            // Wait on source read-queue.  TimedOut loops back to the top.
            match WaitQueue::wait_until_interruptible(src_wq, || {
                match splice_atomic_single(buf1, buf2, len) {
                    Ok(0) => Some(0),
                    Ok(n) => Some(n as isize),
                    Err(EPIPE) => Some(EPIPE),
                    Err(EAGAIN) => None,
                    Err(e) => Some(e),
                }
            }) {
                WaitResult::Ready(n) => {
                    if n > 0 {
                        notify_pipe_splice(p1, p2);
                    } else if n == EPIPE {
                        send_sigpipe_to_current();
                    }
                    return n;
                }
                WaitResult::Interrupted => return ERESTART,
                WaitResult::TimedOut => { /* loop back */ }
            }
        }
    }
}
