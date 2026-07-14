use super::common::*;

pub fn sys_fchdir(fd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if !file.is_dir() {
        return ENOTDIR;
    }
    let inode = file.inode.clone();
    drop(fd_table);
    let meta = match inode.metadata() {
        Ok(meta) => meta,
        Err(e) => return -(e as isize),
    };
    let (uid, gid) = open_subject_ids();
    if !has_search_access(&meta, uid, gid) {
        return EACCES;
    }
    let working_path = inode.absolute_path().ok();
    let file = match vfs::File::new(inode, vfs::FileFlags::O_RDONLY) {
        Ok(f) => f,
        Err(e) => return -(e as isize),
    };
    let fs_ref = task.process.fs();
    let mut lock = fs_ref.lock();
    lock.working_inode = file;
    if let Some(path) = working_path {
        lock.working_path = path;
    }
    SUCCESS
}
