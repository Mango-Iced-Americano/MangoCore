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
    // `absolute_path()` is global-VFS-relative.  Convert it at the process
    // root boundary before exposing it to user space; on a transient VFS
    // lookup failure retain the already validated cached path.
    let working_dir = match crate::fs::process_visible_path(&cwd_inode, root_inode.as_ref()) {
        Ok(visible_path) => {
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
    if !vm_ref.read(|vm| vm.contains_valid_buffer(buf, write_len, MapPermission::W)) {
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
    if let Err(errno) = user_buf.write_all(&cwd) {
        return errno;
    }
    write_len as isize
}
