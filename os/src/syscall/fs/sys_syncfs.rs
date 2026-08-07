use super::common::*;

pub fn sys_syncfs(fd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(_e) => return EBADF,
    };
    if is_path_fd(&file) {
        return EBADF;
    }
    drop(fd_table);

    // Flush all page caches (global, but correct for single-fs system)
    if let Err(error) = crate::fs::flush_all_page_caches() {
        return -(error as isize);
    }

    // Flush ext4 metadata caches for the filesystem containing this fd.
    // Must unwrap MountFSInode to reach the real Ext4FileSystem.
    let inode = vfs::MountFSInode::unwrap_inode(&file.inode);
    let fs = inode.fs();
    #[cfg(feature = "ext4_another_backend")]
    if let Some(ext4) = fs
        .as_any_ref()
        .downcast_ref::<crate::fs::ext4_another::Ext4FileSystem>()
    {
        return match ext4.sync_all() {
            Ok(()) => SUCCESS,
            Err(error) => -(error as isize),
        };
    }
    if let Some(ext4) = fs.as_any_ref().downcast_ref::<crate::fs::ext4::ext4fs::Ext4FileSystem>() {
        ext4.flush_metadata_cache();
    }

    SUCCESS
}
