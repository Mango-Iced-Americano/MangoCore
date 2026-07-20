use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use crate::fs::{
    procfs::{proc_read_str, LockedProcInode},
    vfs::{IndexNode, InodeMode},
};
use crate::utils::error::SyscallErr;

fn get_pid(inode: &LockedProcInode) -> usize {
    inode.0.lock().extra_data
}

pub fn task_list_hook(inode: &LockedProcInode) -> Vec<String> {
    let pid = get_pid(inode);
    let Some(process) = crate::task::find_process_by_pid(pid) else {
        return Vec::new();
    };

    let mut tids: Vec<String> = process
        .threads()
        .into_iter()
        .map(|task| task.gettid().to_string())
        .collect();
    tids.sort();
    tids
}

pub fn task_find_hook(inode: &LockedProcInode, name: &str) -> Option<Arc<dyn IndexNode>> {
    let pid = get_pid(inode);
    let tid: usize = name.parse().ok()?;
    let task = crate::task::find_task_by_pid_tid(pid, tid)?;
    let (parent_weak, fs_weak) = {
        let dir_lock = inode.0.lock();
        (dir_lock.self_ref.clone(), dir_lock.fs.clone())
    };

    let dir =
        LockedProcInode::new_dir_wired(parent_weak, fs_weak, InodeMode::from_bits_truncate(0o555));
    {
        let mut data = dir.0.lock();
        data.extra_data = tid;
        data.process_ref = Some(Arc::downgrade(&task.process));
    }
    let stat_extra = (pid << 32) | (tid & 0xffff_ffff);
    dir.add_file(
        "stat",
        InodeMode::from_bits_truncate(0o444),
        super::stat::task_stat_content,
        stat_extra,
    )
    .ok()?;
    dir.add_file(
        "comm",
        InodeMode::from_bits_truncate(0o444),
        task_comm_content,
        stat_extra,
    )
    .ok()?;
    Some(dir as Arc<dyn IndexNode>)
}

pub fn task_comm_content(
    extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let pid = extra >> 32;
    let tid = extra & 0xffff_ffff;
    let task = match crate::task::find_task_by_pid_tid(pid, tid) {
        Some(task) => task,
        None => return Err(SyscallErr::ENOENT),
    };
    let comm = task.acquire_inner_lock().task_comm;
    let comm_len = comm
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(comm.len());
    let name = core::str::from_utf8(&comm[..comm_len]).unwrap_or("");
    let mut out = String::from(name);
    out.push('\n');
    proc_read_str(offset, len, buf, &out)
}
