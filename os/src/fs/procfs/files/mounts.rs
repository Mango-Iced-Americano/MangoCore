//! /proc/mounts — 系统挂载表

use crate::utils::error::SyscallErr;
use crate::fs::procfs::proc_read_str;
use alloc::string::String;

pub fn mounts_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let root = crate::fs::vfs_root();
    let mut s = String::with_capacity(512);

    // Root mount
    let root_fs = root.inner_filesystem();
    let root_fs_name = root_fs.name();
    let root_source = "rootfs";
    s.push_str(root_source);
    s.push_str(" / ");
    s.push_str(root_fs_name);
    s.push_str(" rw,relatime 0 0\n");

    // List sub-mounts from VFS_ROOT
    let mountpoints = root.mountpoints.lock();
    for (_ino, mfs) in mountpoints.iter() {
        let fs = mfs.inner_filesystem();
        let fs_name = fs.name();
        s.push_str(fs_name);
        s.push_str(" /");
        s.push_str(fs_name);
        s.push(' ');
        s.push_str(fs_name);
        s.push_str(" rw,relatime 0 0\n");
    }

    proc_read_str(offset, len, buf, &s)
}
