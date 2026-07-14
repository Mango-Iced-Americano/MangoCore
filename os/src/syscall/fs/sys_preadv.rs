use super::common::*;

pub fn sys_preadv(fd: usize, iov: usize, iovcnt: usize, offset: usize) -> isize {
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
    if offset_is_negative(offset) {
        return EINVAL;
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
    let total_len = user_iov.capped_len();
    if total_len == 0 {
        return 0;
    }

    // UserBuffer fast path for inodes that support direct user I/O
    if file.inode.supports_user_buffer_io() {
        let chunk_cap = total_len.min(crate::hal::IO_CHUNK_SIZE);
        let mut done = 0usize;
        while done < total_len {
            let want = (total_len - done).min(chunk_cap);
            let file_off = match offset.checked_add(done) {
                Some(v) => v,
                None => return if done > 0 { done as isize } else { -(SyscallErr::EINVAL as isize) },
            };
            let accessible = match iov_writable_len_for_read(&user_iov, done, want) {
                Ok(n) => n,
                Err(errno) => return if done > 0 { done as isize } else { errno },
            };

            let mut ubuf = match user_iov.writer_buffer_at(done, accessible) {
                Ok(b) => b,
                Err(errno) => return if done > 0 { done as isize } else { errno },
            };

            let n = match file.pread_user(file_off, &mut ubuf) {
                Ok(n) => n,
                Err(e) => return if done > 0 { done as isize } else { -(e as isize) },
            };
            if n == 0 { break; }
            done += n;
            if n < accessible { break; }
            if let Some(task) = current_task() {
                if crate::task::has_actionable_signal(&task) { break; }
            }
        }
        return done as isize;
    }

    let chunk_cap = total_len.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut done = 0usize;  // ← re-declare for kbuf path
    while done < total_len {
        let want = (total_len - done).min(chunk_cap);
        let file_off = match offset.checked_add(done) {
            Some(v) => v,
            None => return if done > 0 { done as isize } else { -(SyscallErr::EINVAL as isize) },
        };
        let accessible = match iov_writable_len_for_read(&user_iov, done, want) {
            Ok(n) => n,
            Err(errno) => return if done > 0 { done as isize } else { errno },
        };

        let n = match file.pread(file_off, &mut kbuf[..accessible]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if done > 0 { done as isize } else { ret };
            }
        };
        if n == 0 {
            break;
        }

        // Write to user iovec one page at a time for cross-page fault-in
        let mut copied = 0usize;
        while copied < n {
            let chunk = (n - copied).min(crate::config::PAGE_SIZE);
            let mut ubuf = match user_iov.writer_buffer_at(done + copied, chunk) {
                Ok(b) => b,
                Err(errno) => {
                    if copied > 0 { done += copied; }
                    return if done > 0 { done as isize } else { errno };
                }
            };
            let c = ubuf.write_at(0, &kbuf[copied..copied + chunk]);
            copied += c;
            if c < chunk { break; }
        }

        done += copied;
        if copied < n {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    done as isize
}
