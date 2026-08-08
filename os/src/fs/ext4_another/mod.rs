//! Writable Mango VFS bridge for the selected `another_ext4` backend.

mod blockdev;
mod errno;
mod fs;
mod inode;
mod lifetime;
mod mutations;
mod namespace;
mod page_cache;

pub use fs::Ext4FileSystem;
