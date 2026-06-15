//! /proc/<pid>/ 目录 — 进程信息目录

pub mod cmdline;
pub mod exe;
pub mod fd;
pub mod io;
pub mod maps;
pub mod ns;
pub mod pagemap;
pub mod smaps;
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
    if let Some(process) = crate::task::find_process_by_pid(pid) {
        return create_pid_dir(inode, process).ok();
    }
    create_dead_ns_dir(inode, pid)
}

fn create_dead_ns_dir(parent: &LockedProcInode, pid: usize) -> Option<Arc<dyn IndexNode>> {
    let netns = crate::task::net_namespace::find_ns_by_pid(pid)
        .unwrap_or_else(|| crate::task::INIT_NET_NAMESPACE.clone());
    let (parent_weak, fs_weak) = {
        let pdata = parent.0.lock();
        (pdata.self_ref.clone(), pdata.fs.clone())
    };
    let dir = LockedProcInode::new_dir_wired(
        parent_weak,
        fs_weak,
        InodeMode::from_bits_truncate(0o500),
    );
    let (dir_self_ref, dir_fs) = {
        let guard = dir.0.lock();
        (guard.self_ref.clone(), guard.fs.clone())
    };
    let ns_dir = LockedProcInode::new_dir_wired(
        dir_self_ref,
        dir_fs,
        InodeMode::from_bits_truncate(0o500),
    );
    let ns_fs = ns_dir.0.lock().fs.clone();
    ns_dir.0.lock().children.insert(String::from("net"),
        Arc::new(ns::ProcNsNetInode::new(netns, ns_fs.clone())) as Arc<dyn IndexNode>);
    ns_dir.0.lock().children.insert(String::from("mnt"),
        Arc::new(ns::ProcNsMntInode::new(crate::task::INIT_MOUNT_NAMESPACE.clone(), ns_fs.clone())) as Arc<dyn IndexNode>);
    ns_dir.0.lock().children.insert(String::from("ipc"),
        Arc::new(ns::ProcNsIpcInode::new(crate::task::INIT_IPC_NAMESPACE.clone(), ns_fs)) as Arc<dyn IndexNode>);
    dir.0.lock().children.insert(String::from("ns"), ns_dir);
    Some(dir)
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
        "comm",
        InodeMode::from_bits_truncate(0o444),
        task::task_comm_content,
        (pid << 32) | (pid & 0xffff_ffff),
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

    dir.add_cached_text_file(
        "smaps",
        InodeMode::from_bits_truncate(0o444),
        smaps::pid_smaps_snapshot,
        pid,
    )?;

    dir.add_file(
        "mounts",
        InodeMode::from_bits_truncate(0o444),
        crate::fs::procfs::files::mounts::mounts_content,
        pid,
    )?;

    dir.add_file(
        "mountinfo",
        InodeMode::from_bits_truncate(0o444),
        crate::fs::procfs::files::mounts::mountinfo_content,
        pid,
    )?;

    dir.add_file(
        "io",
        InodeMode::from_bits_truncate(0o444),
        io::pid_io_content,
        pid,
    )?;

    dir.add_file(
        "pagemap",
        InodeMode::from_bits_truncate(0o444),
        pagemap::pid_pagemap_content,
        pid,
    )?;

    dir.add_dynamic_symlink("exe", exe::pid_exe_content, pid)?;

    let fd_dir = dir.add_dir_locked("fd", InodeMode::from_bits_truncate(0o500))?;
    fd_dir.0.lock().extra_data = pid;
    fd_dir.set_hooks(fd::fd_find_hook, fd::fd_list_hook);

    let task_dir = dir.add_dir_locked("task", InodeMode::from_bits_truncate(0o555))?;
    task_dir.0.lock().extra_data = pid;
    task_dir.set_hooks(task::task_find_hook, task::task_list_hook);

    let ns_dir = dir.add_dir_locked("ns", InodeMode::from_bits_truncate(0o500))?;
    let fs_weak = ns_dir.0.lock().fs.clone();

    let netns = process.net();
    let ns_inode =
        Arc::new(ns::ProcNsNetInode::new(netns, fs_weak.clone())) as Arc<dyn IndexNode>;
    ns_dir.0.lock().children.insert(String::from("net"), ns_inode);

    let mntns = process.mnt();
    let mnt_inode =
        Arc::new(ns::ProcNsMntInode::new(mntns, fs_weak.clone())) as Arc<dyn IndexNode>;
    ns_dir.0.lock().children.insert(String::from("mnt"), mnt_inode);

    let ipcns = process.ipc();
    let ipc_inode =
        Arc::new(ns::ProcNsIpcInode::new(ipcns, fs_weak.clone())) as Arc<dyn IndexNode>;
    ns_dir.0.lock().children.insert(String::from("ipc"), ipc_inode);

    Ok(dir)
}
