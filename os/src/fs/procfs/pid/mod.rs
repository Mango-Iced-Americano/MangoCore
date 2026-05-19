//! /proc/<pid>/ 目录 — 进程信息目录

pub mod cmdline;
pub mod status;

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use crate::{
    fs::{
        procfs::{FindHookFn, ListHookFn, LockedProcInode},
        vfs::{IndexNode, InodeMode},
    },
    utils::error::SyscallErr,
};

/// 设置根目录的动态 PID 钩子
pub fn setup_pid_hooks(root: &Arc<LockedProcInode>) {
    root.set_hooks(pid_find_hook, pid_list_hook);
}

fn pid_find_hook(inode: &LockedProcInode, name: &str) -> Option<Arc<dyn IndexNode>> {
    let pid: usize = name.parse().ok()?;
    crate::task::find_process_by_pid(pid)?;
    create_pid_dir(inode, pid).ok()
}

fn pid_list_hook(_inode: &LockedProcInode) -> Vec<String> {
    crate::task::all_pids()
        .into_iter()
        .map(|p| p.to_string())
        .collect()
}

fn create_pid_dir(parent: &LockedProcInode, pid: usize) -> Result<Arc<dyn IndexNode>, SyscallErr> {
    let (parent_weak, fs_weak) = {
        let pdata = parent.0.lock();
        (pdata.self_ref.clone(), pdata.fs.clone())
    };

    let dir = LockedProcInode::new_dir_wired(
        parent_weak,
        fs_weak,
        InodeMode::from_bits_truncate(0o555),
    );

    dir.add_file(
        "status",
        InodeMode::from_bits_truncate(0o444),
        status::pid_status_content,
        pid,
    )?;

    dir.add_file(
        "cmdline",
        InodeMode::from_bits_truncate(0o444),
        cmdline::pid_cmdline_content,
        pid,
    )?;

    Ok(dir)
}
