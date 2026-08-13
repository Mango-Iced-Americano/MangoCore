//! Writable Mango VFS bridge for the selected `another_ext4` backend.

mod blockdev;
mod errno;
mod fs;
mod inode;
mod lifetime;
mod mutations;
mod namespace;
mod page_cache;

pub(crate) use fs::prepare_stats_snapshots;
pub(crate) use fs::shutdown_all_instances;
pub(crate) use fs::sync_all_instances;
pub use fs::Ext4FileSystem;
