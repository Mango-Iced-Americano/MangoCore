use super::common::*;

pub fn sys_getcwd(buf: usize, size: usize) -> isize {
    info!("[sys_getcwd] buf={:#x}, size={}", buf, size);
    let task = current_task().unwrap();
    let fs_ref = task.process.fs();
    let (cwd_inode, cached_path, root_inode) = {
        let fs_lock = fs_ref.lock();
        (
            fs_lock.working_inode.inode.clone(),
            fs_lock.working_path.clone(),
            fs_lock.root_inode.clone(),
        )
    };
    // Inside a chroot the cached path is already maintained in the process
    // namespace by chdir/fchdir.  Walking the global VFS parent chain here
    // can cross the chroot mount and block BuildStorm's shell while it merely
    // updates PWD.  Keep the namespace-local value instead.
    let working_dir = if root_inode.is_some() {
        cached_path
    } else {
        match cwd_inode.absolute_path() {
            Ok(visible_path) => {
                if visible_path != cached_path {
                    fs_ref.lock().working_path = visible_path.clone();
                }
                visible_path
            }
            Err(_) => cached_path,
        }
    };
    // ERANGE must be checked BEFORE buffer validation:
    // Linux returns ERANGE if buffer is too small, even if buf is partially invalid
    if working_dir.len() + 1 > size {
        return ERANGE;
    }
    let write_len = working_dir.len() + 1;
    // UserBufferWriter performs the authoritative range check and fault-in
    // under the current address-space lock. The old VMA-only precheck could
    // reject a valid lazy heap/mmap allocation before its pages were faulted
    // in, returning EFAULT to libc's getcwd and making rustc report a
    // misleading current-working-directory "Bad address" error.
    let token = task.get_user_token();
    let mut user_buf = match UserBufferWriter::new(token, buf as *mut u8, write_len) {
        Ok(writer) => writer,
        Err(errno) => return errno,
    };
    let mut cwd = Vec::with_capacity(write_len);
    cwd.extend_from_slice(working_dir.as_bytes());
    cwd.push(0);
    if let Err(errno) = user_buf.write_all(&cwd) {
        return errno;
    }
    write_len as isize
}
