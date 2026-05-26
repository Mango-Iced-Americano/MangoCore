use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use crate::fs::{
    procfs::LockedProcInode,
    vfs::{IndexNode, InodeMode},
};

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

    let dir = LockedProcInode::new_dir_wired(
        parent_weak,
        fs_weak,
        InodeMode::from_bits_truncate(0o555),
    );
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
    Some(dir as Arc<dyn IndexNode>)
}
