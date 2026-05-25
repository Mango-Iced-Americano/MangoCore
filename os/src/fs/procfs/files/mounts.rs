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
    let mut s = String::with_capacity(1024);

    s.push_str("none / rootfs rw,relatime 0 0\n");

    fn walk_mounts(mfs: &crate::fs::vfs::MountFS, output: &mut String) {
        let mountpoints = mfs.mountpoints.lock();
        for (_ino, child_mfs) in mountpoints.iter() {
            let inner_fs = child_mfs.inner_filesystem();
            let fs_name = inner_fs.name();
            let source = child_mfs.mount_source().unwrap_or_else(|| String::from("none"));
            let path = child_mfs.mount_path().unwrap_or_else(|| String::from("/?"));
            output.push_str(&source);
            output.push(' ');
            output.push_str(&path);
            output.push(' ');
            output.push_str(fs_name);
            output.push_str(" rw,relatime 0 0\n");
            walk_mounts(child_mfs, output);
        }
    }
    walk_mounts(&root, &mut s);

    proc_read_str(offset, len, buf, &s)
}
