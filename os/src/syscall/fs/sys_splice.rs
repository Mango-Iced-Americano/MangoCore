use super::common::*;

pub fn sys_splice(
    fd_in: usize,
    off_in: *mut usize,
    fd_out: usize,
    off_out: *mut usize,
    len: usize,
    flags: u32,
) -> isize {
    if flags & !SPLICE_VALID_FLAGS != 0 {
        return EINVAL;
    }
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

    info!("[sys_splice] outfd: {}, in_fd: {}", fd_out, fd_in);
    if in_file.readable().is_err() || out_file.writable().is_err() {
        return EBADF;
    }

    // Pipe validation: at least one fd must be a pipe
    let in_pipe = in_file.file_type() == FileType::Pipe;
    let out_pipe = out_file.file_type() == FileType::Pipe;
    if !in_pipe && !out_pipe {
        return EINVAL;
    }

    // splice with len == 0 → no data to transfer
    if len == 0 {
        return 0;
    }

    // Same-pipe splice: reading from and writing to the same pipe → EINVAL
    if in_pipe && out_pipe && fd_in == fd_out {
        return EINVAL;
    }

    info!("[sys_splice] off_in: {:?}, off_out: {:?}", off_in, off_out);
    // a buffer in kernel
    const BUFFER_SIZE: usize = 4096;
    let mut buffer = Vec::<u8>::with_capacity(BUFFER_SIZE);
    let mut buffer_ptr: Option<&[u8]> = None;

    let token = task.get_user_token();
    // Pipe fds must not have non-NULL offset (ESPIPE)
    if in_pipe && !off_in.is_null() {
        return ESPIPE;
    }
    if out_pipe && !off_out.is_null() {
        return ESPIPE;
    }
    let mut off_in_val = match read_user_off(off_in, token) {
        Ok(opt) => opt,
        Err(errno) => return errno,
    };
    let mut off_out_val = match read_user_off(off_out, token) {
        Ok(opt) => opt,
        Err(errno) => return errno,
    };

    let mut left_bytes = len;
    let nonblock =
        flags & SPLICE_F_NONBLOCK != 0 || in_file.is_nonblock() || out_file.is_nonblock();
    loop {
        let write_buffer = match buffer_ptr {
            Some(buffer_ptr) => buffer_ptr,
            None => {
                unsafe {
                    buffer.set_len(left_bytes.min(BUFFER_SIZE));
                }
                let read_size = {
                    if let Some(off_val) = off_in_val.as_mut() {
                        let n = match in_file.inode.read_at(
                            *off_val,
                            buffer.len(),
                            buffer.as_mut_slice(),
                            in_file.private_data(),
                        ) {
                            Ok(n) => n,
                            Err(e) => return -(e as isize),
                        };
                        *off_val += n;
                        n
                    } else {
                        match splice_read_stream(&in_file, buffer.as_mut_slice(), nonblock) {
                            Ok(n) => n,
                            Err(errno) => return errno,
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

        let write_size = {
            if let Some(off_val) = off_out_val.as_mut() {
                let n = match out_file.inode.write_at(
                    *off_val,
                    write_buffer.len(),
                    write_buffer,
                    out_file.private_data(),
                ) {
                    Ok(n) => n,
                    Err(e) => return -(e as isize),
                };
                *off_val += n;
                n
            } else {
                match splice_write_stream(&out_file, write_buffer, nonblock) {
                    Ok(n) => n,
                    Err(errno) => return errno,
                }
            }
        };
        if write_size == 0 {
            break;
        }

        buffer_ptr = if write_size < read_size {
            Some(&write_buffer[write_size..])
        } else {
            None
        };
        left_bytes -= write_size;
    }
    let send_size = len - left_bytes;
    if let Some(offset) = off_in_val {
        if UserPtrMut::new(off_in).write(token, &offset).is_err() {
            return EFAULT;
        }
    }
    if let Some(offset) = off_out_val {
        if UserPtrMut::new(off_out).write(token, &offset).is_err() {
            return EFAULT;
        }
    }
    info!("[sys_splice] sent bytes: {}", send_size);
    send_size as isize
}
