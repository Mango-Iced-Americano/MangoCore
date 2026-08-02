use super::common::*;

pub fn sys_lseek(fd: usize, offset: isize, whence: u32) -> isize {
    info!(
        "[sys_lseek] fd: {}, offset: {}, whence: {}",
        fd, offset, whence,
    );
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };

    // O_PATH fds do not support lseek
    if is_path_fd(&file) {
        return EBADF;
    }

    // Explicit numeric match — SeekWhence bitflags lets 5,6,7 slip through
    match whence {
        0 => { /* SEEK_SET */ }
        1 => { /* SEEK_CUR */ }
        2 => { /* SEEK_END */ }
        3 | 4 => { /* SEEK_DATA / SEEK_HOLE */ }
        _ => {
            warn!("[sys_lseek] unknown whence: {}", whence);
            return EINVAL;
        }
    }

    // SEEK_DATA(3) / SEEK_HOLE(4): non-sparse files (treat all as dense)
    if whence == 3 || whence == 4 {
        let off = offset as i64;
        if off < 0 {
            return EINVAL;
        }
        // Release fd_table before I/O
        drop(fd_table);

        // Seekability: same check as File::lseek — non-seekable FDs return ESPIPE
        let ftype = file.file_type();
        if ftype != FileType::File && ftype != FileType::Dir {
            return ESPIPE;
        }

        let md = match file.metadata() {
            Ok(md) => md,
            Err(e) => return -(e as isize),
        };
        let file_size = md.size;
        if off >= file_size {
            return ENXIO;
        }
        match whence {
            3 => {
                // SEEK_DATA: return current offset (entire file is data)
                file.set_offset(off as usize);
                return off as isize;
            }
            4 => {
                // SEEK_HOLE: return file_size (hole at EOF)
                file.set_offset(file_size as usize);
                return file_size as isize;
            }
            _ => unreachable!(),
        }
    }

    drop(fd_table);
    let seek_from = match whence {
        0 => SeekFrom::SeekSet(offset as i64),
        2 => SeekFrom::SeekEnd(offset as i64),
        _ => SeekFrom::SeekCurrent(offset as i64),
    };
    match file.lseek(seek_from) {
        Ok(pos) => pos as isize,
        Err(e) => -(e as isize),
    }
}
