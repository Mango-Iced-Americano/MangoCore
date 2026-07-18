use super::common::*;

pub fn sys_fallocate(fd: usize, mode: u32, offset: isize, len: isize) -> isize {
    const FALLOC_FL_KEEP_SIZE: u32 = 0x01;
    const FALLOC_FL_PUNCH_HOLE: u32 = 0x02;
    const FALLOC_PUNCH_HOLE_KEEP_SIZE: u32 = FALLOC_FL_KEEP_SIZE | FALLOC_FL_PUNCH_HOLE;

    if offset < 0 || len <= 0 {
        return EINVAL;
    }

    // Linux: offset+len overflow or exceeding max file size → EFBIG (not EINVAL)
    let end128 = (offset as i128) + (len as i128);
    let max_file_size = isize::MAX as i128;
    if end128 > max_file_size {
        return EFBIG;
    }
    let end = end128 as usize;

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let (file, is_regular) = {
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(fd) {
            Ok(file) => file,
            Err(e) => return -(e as isize),
        };
        let is_regular = file.file_type() == FileType::File;
        (file, is_regular)
    };
    drop(files_ref);

    // ENODEV if not a regular file
    if !is_regular {
        return ENODEV;
    }

    // O_PATH fd: fallocate must fail with EBADF
    if is_path_fd(&file) {
        return EBADF;
    }
    // fallocate requires a writable fd
    if file.writable().is_err() {
        return EBADF;
    }

    info!(
        "[sys_fallocate] fd: {}, mode: {:#x}, offset: {}, len: {}, end: {}",
        fd, mode, offset, len, end
    );

    // Supported modes: 0 (allocate), FALLOC_FL_KEEP_SIZE (allocate keep size),
    // FALLOC_PUNCH_HOLE|KEEP_SIZE (punch hole, no-op)
    if mode == FALLOC_PUNCH_HOLE_KEEP_SIZE {
        let seals = file.memfd_seal_bits().unwrap_or(0);
        if (seals & vfs::F_SEAL_WRITE) != 0 {
            return EPERM;
        }
        // Punch-hole is not implemented — return EOPNOTSUPP so LTP marks it unsupported
        return EOPNOTSUPP;
    }
    if mode != 0 && mode != FALLOC_FL_KEEP_SIZE {
        warn!("[sys_fallocate] unsupported mode: {:#x}", mode);
        return EOPNOTSUPP;
    }

    if mode == 0 {
        if file.get_size() >= end as usize {
            return SUCCESS;
        }
        if let Some(seals) = file.memfd_seal_bits() {
            if (seals & vfs::F_SEAL_GROW) != 0 {
                return EPERM;
            }
        }
        match file.truncate_size(end as usize) {
            Ok(()) => SUCCESS,
            Err(e) => -(e as isize),
        }
    } else {
        // mode == FALLOC_FL_KEEP_SIZE: allocate without extending file size.
        // No actual block preallocation support, just return success.
        SUCCESS
    }
}
