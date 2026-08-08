use super::common::*;

pub fn sys_pipe2(pipefd: usize, flags: u32) -> isize {
    const VALID_FLAGS: OpenFlags =
        OpenFlags::from_bits_truncate(0o2000000 /* O_CLOEXEC */ | 0o4000 /* O_NONBLOCK */);
    let flags = match OpenFlags::from_bits(flags) {
        Some(flags) => {
            // only O_CLOEXEC | O_NONBLOCK are valid in pipe2()
            if flags.difference(VALID_FLAGS).is_empty() {
                flags
            } else {
                // some flags are invalid in pipe2(), they are all valid OpenFlags though
                warn!(
                    "[sys_pipe2] invalid flags: {:?}",
                    flags.difference(VALID_FLAGS)
                );
                return EINVAL;
            }
        }
        None => {
            // contains invalid OpenFlags
            warn!("[sys_pipe2] unknown flags");
            return EINVAL;
        }
    };
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    let (pipe_read, pipe_write) = make_pipe();
    let cloexec = flags.contains(OpenFlags::O_CLOEXEC);
    let nonblock = flags.contains(OpenFlags::O_NONBLOCK);
    let vf_read = vfs::File::new_without_open(
        pipe_read,
        vfs::FileFlags::O_RDONLY
            | if nonblock {
                vfs::FileFlags::O_NONBLOCK
            } else {
                vfs::FileFlags::empty()
            },
        vfs::FileType::Pipe,
    );
    let read_fd = match fd_table.alloc_fd(vf_read, cloexec) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };
    let vf_write = vfs::File::new_without_open(
        pipe_write,
        vfs::FileFlags::O_WRONLY
            | if nonblock {
                vfs::FileFlags::O_NONBLOCK
            } else {
                vfs::FileFlags::empty()
            },
        vfs::FileType::Pipe,
    );
    let write_fd = match fd_table.alloc_fd(vf_write, cloexec) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = fd_table.drop_fd(read_fd);
            return -(e as isize);
        }
    };

    let token = task.get_user_token();
    let fds = [read_fd as u32, write_fd as u32];
    if UserSlice::new(pipefd as *const u32, 2)
        .write_array_from(token, &fds)
        .is_err()
    {
        log::error!("[sys_pipe2] Failed to copy to {:?}", pipefd);
        let _ = fd_table.drop_fd(read_fd);
        let _ = fd_table.drop_fd(write_fd);
        return EFAULT;
    };
    info!(
        "[sys_pipe2] read_fd: {}, write_fd: {}, flags: {:?}",
        read_fd, write_fd, flags
    );
    SUCCESS
}
