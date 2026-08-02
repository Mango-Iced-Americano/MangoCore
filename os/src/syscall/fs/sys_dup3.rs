use super::common::*;

pub fn sys_dup3(oldfd: usize, newfd: usize, flags: u32) -> isize {
    info!(
        "[sys_dup3] oldfd: {}, newfd: {}, flags: {:X}",
        oldfd, newfd, flags
    );
    if oldfd == newfd {
        return EINVAL;
    }
    // Only O_CLOEXEC is valid for dup3; use direct bit check
    const O_CLOEXEC: u32 = 0o2000000;
    if flags & !O_CLOEXEC != 0 {
        warn!("[sys_dup3] invalid flags: {:X}", flags);
        return EINVAL;
    }
    let is_cloexec = (flags & O_CLOEXEC) != 0;
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();

    let file = match fd_table.get_file(oldfd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let replaced_flock = fd_table
        .get_file(newfd)
        .ok()
        .map(|file| (file.description_id(), Arc::strong_count(&file)));
    let (ret, old_file) = match fd_table.alloc_fd_at(newfd, file, is_cloexec) {
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
}
