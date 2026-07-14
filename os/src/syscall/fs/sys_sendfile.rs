use super::common::*;

pub fn sys_sendfile(out_fd: usize, in_fd: usize, offset: *mut usize, count: usize) -> isize {
    let count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(in_fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(out_fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if is_path_fd(&in_file) || is_path_fd(&out_file) {
        return EBADF;
    }
    drop(fd_table);

    info!("[sys_sendfile] outfd: {}, in_fd: {}", out_fd, in_fd);
    if in_file.readable().is_err() || out_file.writable().is_err() {
        return EBADF;
    }

    let token = task.get_user_token();
    let mut offset_val = match read_user_off(offset, token) {
        Ok(opt) => opt,
        Err(errno) => return errno,
    };

    // a buffer in kernel
    const BUFFER_SIZE: usize = 4096;
    let mut buffer = Vec::<u8>::with_capacity(BUFFER_SIZE);
    let mut buffer_ptr: Option<&[u8]> = None;

    let mut left_bytes = count;
    loop {
        let write_buffer = match buffer_ptr {
            Some(buffer_ptr) => buffer_ptr,
            None => {
                unsafe {
                    buffer.set_len(left_bytes.min(BUFFER_SIZE));
                }
                let read_size = {
                    if let Some(off_val) = offset_val.as_mut() {
                        let n = match in_file.inode.read_at(
                            *off_val,
                            buffer.len(),
                            buffer.as_mut_slice(),
                            in_file.private_data(),
                        ) {
                            Ok(n) => n,
                            Err(e) => {
                                let ret = -(e as isize);
                                if count - left_bytes > 0 {
                                    break;
                                }
                                return ret;
                            }
                        };
                        *off_val += n;
                        n
                    } else {
                        match in_file.read(buffer.as_mut_slice()) {
                            Ok(n) => n,
                            Err(e) => {
                                let ret = -(e as isize);
                                if count - left_bytes > 0 {
                                    break;
                                }
                                return ret;
                            }
                        }
                    }
                };
                if read_size == 0 {
                    break;
                }
                unsafe {
                    buffer.set_len(read_size);
                }
                buffer.as_slice()
            }
        };

        let read_size = write_buffer.len();

        let mut fallback = |redundant_bytes: usize| {
            match offset_val.as_mut() {
                Some(offset) => *offset -= redundant_bytes,
                None => match in_file.lseek(SeekFrom::SeekCurrent(-(redundant_bytes as i64))) {
                    Ok(_) => {}
                    Err(errno) => log::error!("splice fallback lseek failed: errno {:?}", errno),
                },
            }
        };

        let write_size = match out_file.write(write_buffer) {
            Ok(n) => n,
            Err(e) => {
                if count - left_bytes == 0 {
                    return -(e as isize);
                }
                fallback(write_buffer.len());
                break;
            }
        };
        if write_size == 0 {
            fallback(read_size);
            break;
        }

        buffer_ptr = if write_size < read_size {
            Some(&write_buffer[write_size..])
        } else {
            None
        };
        left_bytes -= write_size;
    }
    let send_size = count - left_bytes;
    if let Some(offset_value) = offset_val {
        if UserPtrMut::new(offset).write(token, &offset_value).is_err() {
            return EFAULT;
        }
    }
    info!("[sys_sendfile] send bytes: {}", send_size);
    send_size as isize
}
