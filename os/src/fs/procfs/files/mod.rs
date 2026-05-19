//! procfs 文件实现 — /proc/* 的各个文件

pub mod config;
pub mod cpuinfo;
pub mod meminfo;
pub mod mounts;
pub mod self_;
pub mod stat;
pub mod uptime;
pub mod version;

use alloc::sync::Arc;
use crate::fs::vfs::{IndexNode, InodeMode};
use crate::utils::error::SyscallErr;

pub fn register_all(root: &Arc<crate::fs::procfs::LockedProcInode>) -> Result<(), SyscallErr> {
    root.add_file("version", InodeMode::from_bits_truncate(0o444), version::version_content, 0)?;
    root.add_file("uptime", InodeMode::from_bits_truncate(0o444), uptime::uptime_content, 0)?;
    root.add_file("meminfo", InodeMode::from_bits_truncate(0o444), meminfo::meminfo_content, 0)?;
    root.add_file("cpuinfo", InodeMode::from_bits_truncate(0o444), cpuinfo::cpuinfo_content, 0)?;
    root.add_file("mounts", InodeMode::from_bits_truncate(0o444), mounts::mounts_content, 0)?;
    root.add_file("stat", InodeMode::from_bits_truncate(0o444), stat::stat_content, 0)?;
    root.add_file("config", InodeMode::from_bits_truncate(0o444), config::config_content, 0)?;
    root.add_dynamic_symlink("self", self_::self_content)?;

    crate::fs::procfs::pid::setup_pid_hooks(root);

    Ok(())
}
