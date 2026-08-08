use super::common::*;

pub fn sys_dup2(oldfd: usize, newfd: usize) -> isize {
    let task = current_task().unwrap();

    let ret = {
        let files_ref = task.process.files();
        let mut fd_table = files_ref.lock();
        if oldfd == newfd {
            return match fd_table.get_file(oldfd) {
                Ok(_) => oldfd as isize,
                Err(e) => -(e as isize),
            };
        }
        let file = match fd_table.get_file(oldfd) {
            Ok(file) => file,
            Err(e) => return -(e as isize),
        };
        let replaced_flock = fd_table
            .get_file(newfd)
            .ok()
            .map(|file| (file.description_id(), Arc::strong_count(&file)));

        let (ret, old_file) = match fd_table.alloc_fd_at(newfd, file, false) {
            Ok((fd, old)) => (fd as isize, old),
            Err(e) => (-(e as isize), None),
        };
        drop(fd_table);
        // Drop the replaced file OUTSIDE the fd_table lock so File::Drop → inode.close()
        // won't deadlock on page-cache or other locks that the lock holder may block on.
        drop(old_file);
        if ret >= 0 {
            if let Some((description, refs)) = replaced_flock {
                if refs <= 1 {
                    release_flock_description(description);
                }
            }
        }
        ret
    };
    if ret < 0 {
        return ret;
    }
    info!("[sys_dup2] oldfd: {}, newfd: {}", oldfd, newfd);
    newfd as isize
}
