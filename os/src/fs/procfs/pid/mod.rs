//! /proc/<pid>/ 目录 — 进程信息目录

pub mod cmdline;
pub mod exe;
pub mod fd;
pub mod maps;
pub mod stat;
pub mod status;
pub mod task;

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use crate::{
    fs::{
        procfs::LockedProcInode,
        vfs::{IndexNode, InodeMode},
    },
    task::ProcessControlBlock,
    utils::error::SyscallErr,
};

/// 设置根目录的动态 PID 钩子
pub fn setup_pid_hooks(root: &Arc<LockedProcInode>) {
    root.set_hooks(pid_find_hook, pid_list_hook);
}

fn pid_find_hook(inode: &LockedProcInode, name: &str) -> Option<Arc<dyn IndexNode>> {
    let pid: usize = name.parse().ok()?;
    let process = crate::task::find_process_by_pid(pid)?;
    create_pid_dir(inode, process).ok()
}

fn pid_list_hook(_inode: &LockedProcInode) -> Vec<String> {
    crate::task::all_pids()
        .into_iter()
        .map(|p| p.to_string())
        .collect()
}

fn create_pid_dir(
    parent: &LockedProcInode,
    process: Arc<ProcessControlBlock>,
) -> Result<Arc<dyn IndexNode>, SyscallErr> {
    let pid = process.pid;
    let (parent_weak, fs_weak) = {
        let pdata = parent.0.lock();
        (pdata.self_ref.clone(), pdata.fs.clone())
    };

    let dir = LockedProcInode::new_dir_wired(
        parent_weak,
        fs_weak,
        InodeMode::from_bits_truncate(0o555),
    );
    {
        let mut data = dir.0.lock();
        data.extra_data = pid;
        data.process_ref = Some(Arc::downgrade(&process));
    }

    dir.add_file(
        "status",
        InodeMode::from_bits_truncate(0o444),
        status::pid_status_content,
        pid,
    )?;

    dir.add_file(
        "stat",
        InodeMode::from_bits_truncate(0o444),
        stat::pid_stat_content,
        pid,
    )?;

    dir.add_file(
        "cmdline",
        InodeMode::from_bits_truncate(0o444),
        cmdline::pid_cmdline_content,
        pid,
    )?;

    dir.add_file(
        "maps",
        InodeMode::from_bits_truncate(0o444),
        maps::pid_maps_content,
        pid,
    )?;

    dir.add_file(
        "mounts",
        InodeMode::from_bits_truncate(0o444),
        crate::fs::procfs::files::mounts::mounts_content,
        pid,
    )?;

    dir.add_dynamic_symlink("exe", exe::pid_exe_content, pid)?;

    let fd_dir = dir.add_dir_locked("fd", InodeMode::from_bits_truncate(0o500))?;
    fd_dir.0.lock().extra_data = pid;
    fd_dir.set_hooks(fd::fd_find_hook, fd::fd_list_hook);

    let task_dir = dir.add_dir_locked("task", InodeMode::from_bits_truncate(0o555))?;
    task_dir.0.lock().extra_data = pid;
    task_dir.set_hooks(task::task_find_hook, task::task_list_hook);

    Ok(dir)
}
