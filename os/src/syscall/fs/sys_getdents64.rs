use super::common::*;

pub fn sys_getdents64(fd: usize, dirp: *mut u8, count: usize) -> isize {
    if count == 0 {
        return EINVAL;
    }
    // Linux: if buffer cannot hold even the first dirent record, return EINVAL
    // Minimum linux_dirent64 record is 24 bytes (d_ino + d_off + d_reclen + d_type + "." = 24)
    const MIN_DIRENT64_RECLEN: usize = 24;
    if count < MIN_DIRENT64_RECLEN {
        return EINVAL;
    }
    // 防御性限制：单次 getdents64 最多返回 128KB 的目录项，防止超大 Vec 分配导致内核堆 OOM
    let count = count.min(128 * 1024);
    let task = current_task().unwrap();
    let token = task.get_user_token();

    // Cheap addr range check — does NOT fault in pages (unlike UserBufferWriter::new)
    if check_user_range(dirp as usize, count).is_err() {
        return EFAULT;
    }

    // 获取文件描述符
    let file = match fd {
        AT_FDCWD => task.process.fs().lock().working_inode.clone(),
        fd => {
            let files_ref = task.process.files();
            let fd_table = files_ref.lock();
            match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            }
        }
    };

    // O_PATH fd: getdents is not allowed
    if is_path_fd(&file) {
        return EBADF;
    }

    // Save old offset for rollback on copy failure
    let old_offset = file.offset();

    let mut kernel_buf = alloc::vec![0u8; count];
    let written = match file.get_dirent64(&mut kernel_buf) {
        Ok(n) => n,
        Err(errno) => return errno,
    };

    if written == 0 {
        return 0;
    }

    // Writer created with WRITTEN (actual bytes), not COUNT
    let mut writer = match UserBufferWriter::new(token, dirp, written) {
        Ok(w) => w,
        Err(_) => {
            file.set_offset(old_offset); // rollback
            return EFAULT;
        }
    };
    let copied = match writer.write_from(&kernel_buf[..written]) {
        Ok(c) => c,
        Err(_) => {
            file.set_offset(old_offset); // rollback
            return EFAULT;
        }
    };
    if copied != written {
        log::error!(
            "[sys_getdents64] Partial copy to {:?}: {}/{} bytes, returning EFAULT",
            dirp,
            copied,
            written
        );
        file.set_offset(old_offset); // rollback
        return EFAULT;
    }
    info!("[sys_getdents64] fd: {}, count: {}", fd, count);
    written as isize
}
