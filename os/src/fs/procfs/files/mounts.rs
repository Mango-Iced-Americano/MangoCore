//! /proc/mounts — 系统挂载表

use crate::utils::error::SyscallErr;
use crate::fs::procfs::proc_read_str;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub fn mounts_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let root = crate::fs::vfs_root();
    let mut s = String::with_capacity(2048);

    s.push_str("none / rootfs rw,relatime 0 0\n");

    const MAX_WALK: usize = 256;
    // BFS with cycle detection: snapshot children first, then recurse.
    let mut queue: Vec<Arc<crate::fs::vfs::MountFS>> = {
        let mps = root.mountpoints.lock();
        mps.values().cloned().collect()
    };
    let mut seen: Vec<usize> = alloc::vec![Arc::as_ptr(&root) as usize];
    let mut walked: usize = 0;

    while let Some(mfs) = queue.pop() {
        let ptr = Arc::as_ptr(&mfs) as usize;
        if seen.contains(&ptr) || walked >= MAX_WALK {
            continue;
        }
        seen.push(ptr);
        walked += 1;

        let inner_fs = mfs.inner_filesystem();
        let fs_name = inner_fs.name();
        let source = mfs.mount_source().unwrap_or_else(|| String::from("none"));
        let path = mfs.mount_path().unwrap_or_else(|| String::from("/?"));
        s.push_str(&source);
        s.push(' ');
        s.push_str(&path);
        s.push(' ');
        s.push_str(fs_name);
        s.push_str(" rw,relatime 0 0\n");

        // Snapshot children for BFS
        {
            let mps = mfs.mountpoints.lock();
            for child in mps.values() {
                queue.push(Arc::clone(child));
            }
        }
    }

    proc_read_str(offset, len, buf, &s)
}