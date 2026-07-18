use super::common::*;

pub fn sys_pwritev(fd: usize, iov: usize, iovcnt: usize, offset: usize) -> isize {
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if offset_is_negative(offset) {
        return EINVAL;
    }
    if is_stream_file(&file) {
        return ESPIPE;
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
    let total_len = user_iov.capped_len();
    if total_len == 0 {
        return 0;
    }

    let fsize_limit = task.acquire_inner_lock().fsize_limit_cur;
    let allowed = match apply_fsize_limit(
        &file,
        total_len,
        pwrite_start_offset(&file, offset),
        fsize_limit,
    ) {
        Ok(count) => count,
        Err(errno) => return errno,
    };

    // UserBuffer fast path for inodes that support direct user I/O
    if file.inode.supports_user_buffer_io() {
        let chunk_cap = allowed.min(crate::hal::IO_CHUNK_SIZE);
        let mut done = 0usize;
        while done < allowed {
            let want = (allowed - done).min(chunk_cap);
            let file_off = match offset.checked_add(done) {
                Some(v) => v,
                None => return if done > 0 { done as isize } else { -(SyscallErr::EINVAL as isize) },
            };
            let mut accessible = user_iov.accessible_len_at(done, want, crate::mm::UserAccess::Read);
            if accessible == 0 {
                if done > 0 { return done as isize; }
                accessible = want.min(crate::config::PAGE_SIZE);
            }

            let ubuf = match user_iov.reader_buffer_at(done, accessible) {
                Ok(b) => b,
                Err(errno) => return if done > 0 { done as isize } else { errno },
            };

            let n = match file.pwrite_user(file_off, &ubuf) {
                Ok(n) => n,
                Err(e) => return if done > 0 { done as isize } else { -(e as isize) },
            };

            done += n;
            if n == 0 || n < accessible { break; }

            if let Some(task) = current_task() {
                if crate::task::has_actionable_signal(&task) { break; }
            }
        }
        return done as isize;
    }

    let chunk_cap = allowed.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut done = 0usize;  // ← re-declare for kbuf path
    while done < allowed {
        let want = (allowed - done).min(chunk_cap);
        let file_off = match offset.checked_add(done) {
            Some(v) => v,
            None => return if done > 0 { done as isize } else { -(SyscallErr::EINVAL as isize) },
        };
        let mut accessible = user_iov.accessible_len_at(done, want, crate::mm::UserAccess::Read);
        if accessible == 0 {
            if done > 0 {
                return done as isize;
            }
            accessible = want.min(crate::config::PAGE_SIZE);
        }

        let ubuf = match user_iov.reader_buffer_at(done, accessible) {
            Ok(b) => b,
            Err(errno) => return if done > 0 { done as isize } else { errno },
        };
        let copied = ubuf.read(&mut kbuf[..accessible]);

        let n = match file.pwrite(file_off, &kbuf[..copied.min(accessible)]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if done > 0 { done as isize } else { ret };
            }
        };

        done += n;
        if n == 0 || n < copied {
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
