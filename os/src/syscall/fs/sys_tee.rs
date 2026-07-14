use super::common::*;

pub fn sys_tee(fd_in: usize, fd_out: usize, len: usize, flags: u32) -> isize {
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
    drop(fd_table);

    // tee requires BOTH fds to be pipes
    if in_file.file_type() != FileType::Pipe || out_file.file_type() != FileType::Pipe {
        return EINVAL;
    }
    // Validate flags
    if flags & !(SPLICE_F_NONBLOCK | SPLICE_F_MORE) != 0 {
        return EINVAL;
    }

    EINVAL
}
