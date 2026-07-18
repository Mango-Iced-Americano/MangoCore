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
    let working_dir = match cwd_inode.absolute_path() {
        Ok(global_path) => {
            // `absolute_path()` is global-VFS-relative.  After chroot(2),
            // expose a path relative to the calling process's root instead of
            // leaking a host prefix that cannot be reopened from the jail.
            let visible_path = if let Some(root_inode) = root_inode.as_ref() {
                if Arc::ptr_eq(&cwd_inode, root_inode) {
                    String::from("/")
                } else {
                    let root_path = match root_inode.absolute_path() {
                        Ok(path) => path,
                        Err(_) => return ENOENT,
                    };
                    let root_prefix = root_path.trim_end_matches('/');
                    if root_prefix.is_empty() {
                        global_path
                    } else if let Some(suffix) = global_path.strip_prefix(root_prefix) {
                        if suffix.is_empty() {
                            String::from("/")
                        } else if suffix.starts_with('/') {
                            String::from(suffix)
                        } else {
                            return ENOENT;
                        }
                    } else {
                        return ENOENT;
                    }
                }
            } else {
                global_path
            };
            if visible_path != cached_path {
                fs_ref.lock().working_path = visible_path.clone();
            }
            visible_path
        }
        Err(_) => cached_path,
    };
    // ERANGE must be checked BEFORE buffer validation:
    // Linux returns ERANGE if buffer is too small, even if buf is partially invalid
    if working_dir.len() + 1 > size {
        return ERANGE;
    }
    let vm_ref = task.process.vm();
    let write_len = working_dir.len() + 1;
    if !vm_ref
        .lock()
        .contains_valid_buffer(buf, write_len, MapPermission::W)
    {
        return EFAULT;
    }
    let token = task.get_user_token();
    let mut user_buf = match UserBufferWriter::new(token, buf as *mut u8, write_len) {
        Ok(writer) => writer,
        Err(errno) => return errno,
    };
    let mut cwd = Vec::with_capacity(write_len);
    cwd.extend_from_slice(working_dir.as_bytes());
    cwd.push(0);
    if let Err(errno) = user_buf.write_from(&cwd) {
        return errno;
    }
    write_len as isize
}
