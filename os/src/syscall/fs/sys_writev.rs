use super::common::*;

pub fn sys_writev(fd: usize, iov: usize, iovcnt: usize) -> isize {
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
        write_start_offset(&file),
        fsize_limit,
    ) {
        Ok(count) => count,
        Err(errno) => return errno,
    };

    // Fast path for /dev/null, /dev/zero — skip UserBuffer construction
    if file.inode.is_discard_write() {
        return match file.write_discard(allowed) {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        };
    }

    // UserBuffer fast path for inodes that support direct user I/O
    if file.inode.supports_user_buffer_io() {
        let chunk_cap = allowed.min(crate::hal::IO_CHUNK_SIZE);
        let mut done = 0usize;
        while done < allowed {
            let want = (allowed - done).min(chunk_cap);
            let mut accessible = user_iov.accessible_len_at(done, want, crate::mm::UserAccess::Read);
            if accessible == 0 {
                if done > 0 { return done as isize; }
                accessible = want.min(crate::config::PAGE_SIZE);
            }

            let ubuf = match user_iov.reader_buffer_at(done, accessible) {
                Ok(b) => b,
                Err(errno) => return if done > 0 { done as isize } else { errno },
            };

            let n = match file.write_user(&ubuf) {
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

    let mut done = 0usize;
    while done < allowed {
        let want = (allowed - done).min(chunk_cap);
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
        let copied = match ubuf.read_into(&mut kbuf[..accessible]) {
            Ok(copied) => copied,
            Err(errno) => return if done > 0 { done as isize } else { errno },
        };

        let n = match file.write(&kbuf[..copied.min(accessible)]) {
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
