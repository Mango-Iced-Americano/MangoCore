use super::common::*;

pub fn sys_fstatfs(fd: usize, buf: *mut Statfs) -> isize {
    let Some(task) = current_task() else {
        return ESRCH;
    };
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let inode = match fd_table.get_file(fd) {
        Ok(file) => file.inode.clone(),
        Err(e) => return -(e as isize),
    };
    drop(fd_table);

    let fs = inode.fs();
    let sb = match fs.statfs(&inode) {
        Ok(sb) => sb,
        Err(e) => return -(e as isize),
    };
    let statfs = superblock_to_statfs(&sb);
    write_statfs(buf, &statfs)
}
