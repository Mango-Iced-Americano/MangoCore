use super::common::*;

pub fn sys_copy_file_range(
    fd_in: usize,
    off_in: *mut usize,
    fd_out: usize,
    off_out: *mut usize,
    len: usize,
    flags: u32,
) -> isize {
    if flags != 0 {
        return EINVAL;
    }
    let len = len.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(fd_in) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(fd_out) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if is_path_fd(&in_file) || is_path_fd(&out_file) {
        return EBADF;
    }
    drop(fd_table);

    if in_file.readable().is_err() || out_file.writable().is_err() {
        return EBADF;
    }

    let token = task.get_user_token();
    let mut in_offset = if off_in.is_null() {
        None
    } else {
        match UserPtrMut::new(off_in).read(token) {
            Ok(offset) => Some(offset),
            Err(errno) => return errno,
        }
    };
    let mut out_offset = if off_out.is_null() {
        None
    } else {
        match UserPtrMut::new(off_out).read(token) {
            Ok(offset) => Some(offset),
            Err(errno) => return errno,
        }
    };

    const BUFFER_SIZE: usize = 4096;
    let mut buffer = Vec::<u8>::with_capacity(BUFFER_SIZE);
    let mut copied = 0usize;

    while copied < len {
        let chunk = (len - copied).min(BUFFER_SIZE);
        unsafe { buffer.set_len(chunk); }

        let read_size = if let Some(offset) = in_offset {
            match in_file.pread(offset, buffer.as_mut_slice()) {
                Ok(n) => n,
                Err(e) => {
                    if copied > 0 {
                        break;
                    }
                    return -(e as isize);
                }
            }
        } else {
            match in_file.read(buffer.as_mut_slice()) {
                Ok(n) => n,
                Err(e) => {
                    if copied > 0 {
                        break;
                    }
                    return -(e as isize);
                }
            }
        };
        if read_size == 0 {
            break;
        }
        unsafe { buffer.set_len(read_size); }

        let write_size = if let Some(offset) = out_offset {
            match out_file.pwrite(offset, buffer.as_slice()) {
                Ok(n) => n,
                Err(e) => {
                    if copied > 0 {
                        if in_offset.is_none() {
                            let _ = in_file.lseek(SeekFrom::SeekCurrent(-(read_size as i64)));
                        }
                        break;
                    }
                    return -(e as isize);
                }
            }
        } else {
            match out_file.write(buffer.as_slice()) {
                Ok(n) => n,
                Err(e) => {
                    if copied > 0 {
                        if in_offset.is_none() {
                            let _ = in_file.lseek(SeekFrom::SeekCurrent(-(read_size as i64)));
                        }
                        break;
                    }
                    return -(e as isize);
                }
            }
        };

        if write_size == 0 {
            if in_offset.is_none() {
                let _ = in_file.lseek(SeekFrom::SeekCurrent(-(read_size as i64)));
            }
            break;
        }

        if let Some(offset) = in_offset.as_mut() {
            *offset += write_size;
        } else if write_size < read_size {
            let _ = in_file.lseek(SeekFrom::SeekCurrent(-((read_size - write_size) as i64)));
        }
        if let Some(offset) = out_offset.as_mut() {
            *offset += write_size;
        }

        copied += write_size;
        if write_size < read_size {
            break;
        }
    }

    if let Some(offset) = in_offset {
        if UserPtrMut::new(off_in).write(token, &offset).is_err() {
            return EFAULT;
        }
    }
    if let Some(offset) = out_offset {
        if UserPtrMut::new(off_out).write(token, &offset).is_err() {
            return EFAULT;
        }
    }

    copied as isize
}
