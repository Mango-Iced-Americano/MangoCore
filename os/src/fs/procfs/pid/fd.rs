use alloc::{format, string::String, sync::Arc, vec::Vec, vec};
use crate::fs::procfs::{LockedProcInode, proc_read_str};
use crate::fs::vfs::InodeMode;

fn get_pid(inode: &LockedProcInode) -> usize {
    inode.0.lock().extra_data
}

fn fd_content_fn(extra: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, crate::utils::error::SyscallErr> {
    let pid = extra >> 16;
    let fd = extra & 0xFFFF;
    let target = match crate::task::find_process_by_pid(pid) {
        Some(task) => {
            let files = task.files();
            let guard = files.lock();
            match guard.get_file(fd) {
                Ok(_) => format!("/proc/{}/fd/{}", pid, fd),
                Err(_) => String::new(),
            }
        }
        None => String::new(),
    };
    proc_read_str(offset, len, buf, &target)
}

pub fn fd_list_hook(inode: &LockedProcInode) -> Vec<String> {
    let pid = get_pid(inode);
    let Some(task) = crate::task::find_process_by_pid(pid) else {
        return vec![];
    };
    let files = task.files();
    let guard = files.lock();
    let mut fds: Vec<String> = Vec::with_capacity(guard.len());
    for fd in 0..guard.len() {
        if guard.get_file(fd).is_ok() {
            fds.push(format!("{}", fd));
        }
    }
    fds
}

pub fn fd_find_hook(inode: &LockedProcInode, name: &str) -> Option<Arc<dyn crate::fs::vfs::IndexNode>> {
    let pid = get_pid(inode);
    let fd: usize = name.parse().ok()?;
    // 验证 fd 确实存在 — 无效 fd 应返回 ENOENT
    let task = crate::task::find_process_by_pid(pid)?;
    let files = task.files();
    let guard = files.lock();
    guard.get_file(fd).ok()?;
    drop(guard);

    let extra = (pid << 16) | (fd & 0xFFFF);
    let (parent_weak, fs_weak) = {
        let dir_lock = inode.0.lock();
        (dir_lock.self_ref.clone(), dir_lock.fs.clone())
    };
    let file = LockedProcInode::new_file_wired(
        parent_weak,
        fs_weak,
        InodeMode::from_bits_truncate(0o444),
        fd_content_fn,
        extra,
    );
    Some(file as Arc<dyn crate::fs::vfs::IndexNode>)
}
