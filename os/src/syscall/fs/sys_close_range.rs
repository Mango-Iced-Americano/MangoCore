use super::common::*;

pub fn sys_close_range(first: usize, last: usize, flags: u32) -> isize {
    const CLOSE_RANGE_UNSHARE: u32 = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: u32 = 1 << 2;
    const VALID_FLAGS: u32 = CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC;

    if first > last || (flags & !VALID_FLAGS) != 0 {
        return EINVAL;
    }

    let task = current_task().unwrap();
    let files_ref = if (flags & CLOSE_RANGE_UNSHARE) != 0 {
        match task.process.unshare_files() {
            Ok(files) => files,
            Err(e) => return -(e as isize),
        }
    } else {
        task.process.files()
    };
    let mut fd_table = files_ref.lock();
    if (flags & CLOSE_RANGE_CLOEXEC) != 0 {
        fd_table.set_cloexec_range(first, last);
    } else {
        let mut flock_releases = Vec::new();
        for fd in first..=last {
            if let Ok(file) = fd_table.drop_fd(fd) {
                record_flock_close(&mut flock_releases, &file);
            } else if fd >= fd_table.len() {
                break;
            }
        }
        drop(fd_table);
        release_closed_flock_descriptions(flock_releases);
    }
    SUCCESS
}
