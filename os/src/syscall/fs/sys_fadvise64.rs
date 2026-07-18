use super::common::*;

/// Linux 6.6 generic_fadvise semantics:
///
/// | Check              | Result  |
/// |--------------------|---------|
/// | invalid fd         | EBADF   |
/// | O_PATH fd          | EBADF   |
/// | FIFO (Pipe)        | ESPIPE  |
/// | negative len       | EINVAL  |
/// | bad advice         | EINVAL  |
/// | File/Dir/Socket/Blk| 0 (no-op)|
/// | other file type    | EINVAL  |
///
/// Key differences from a naive "only regular files" implementation:
/// - FIFO fires BEFORE range/advice validation (unlike splice/tee which
///   validate offset first).
/// - Negative offset is accepted as-is (i64 semantic on a usize).
/// - Directories and sockets have an address_space (f_mapping) equivalent
///   and succeed as a no-op rather than EINVAL.
pub fn sys_fadvise64(fd: usize, offset: usize, len: usize, advice: i32) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if is_path_fd(&file) {
        return EBADF;
    }

    // Linux 6.6 generic_fadvise: S_ISFIFO → ESPIPE (checked before
    // range/advice validation — even a negative len or bad advice on a
    // FIFO must yield ESPIPE, not EINVAL).
    let ft = file.file_type();
    if ft == FileType::Pipe {
        return ESPIPE;
    }

    if offset_is_negative(len) {
        return EINVAL;
    }
    if !(0..=5).contains(&advice) {
        return EINVAL;
    }

    // Linux 6.6: fadvise operates on the file's address_space
    // (f_mapping).  Regular files, directories, sockets, and raw block
    // devices all carry a valid mapping → no-op success.
    //
    // BlockDevice added per blkdev_open() setting f_mapping in Linux.
    // CharDevice (/dev/null, /dev/zero, etc.) correctly fails: default
    // chrdev_open never sets f_mapping.
    match ft {
        FileType::File | FileType::Dir | FileType::Socket | FileType::BlockDevice => SUCCESS,
        _ => EINVAL,
    }
}
