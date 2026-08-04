mod boot_block;
pub mod dev;
pub mod eventfd;
pub mod eventpoll;
pub mod ext4;
pub mod ext4_lwext4;
pub mod fat32;
mod filesystem;
#[cfg(feature = "initramfs")]
pub mod initramfs;
pub mod iov;
mod layout;
mod page_cache;
pub mod pidfd;
pub use page_cache::{
    entries_global_stats, evict_all_clean_pages, flush_all_page_caches, registry_stats, PageCache,
    PageCacheBackend, PageState,
};
pub(crate) use page_cache::{PageCacheFault, PageCacheTestHook};
pub mod poll;
pub mod procfs;
pub mod ramfs;
pub mod reclaim;
#[cfg(feature = "swap")]
pub mod swap;
pub mod sysfs;
pub mod timerfd;
pub mod tmpfs;
// Xein add this
pub mod dirent;
// file_descriptor module removed — migrated to vfs::File
mod inode;
mod timestamp;
pub mod vfs;

pub use self::dev::pipe::*;

pub use self::layout::*;

pub use self::fat32::DiskInodeType;
pub use self::filesystem::{detect_fs, detect_fs_layout, DetectedFs, FS_Type};
pub use crate::drivers::block::BlockDevice;

use self::vfs::FileSystem as _;
use self::vfs::IndexNode;
use alloc::{string::String, sync::Arc};
use boot_block::register_boot_block_devices as register_discovered_boot_block_devices;
use core::sync::atomic::Ordering;
pub use dirent::Dirent;
use lazy_static::*;

/// Adapt a physical device to a filesystem's native block unit and mount mode.
pub fn adapt_filesystem_device(
    device: Arc<dyn BlockDevice>,
    detected: DetectedFs,
    read_only: bool,
) -> Arc<dyn BlockDevice> {
    let device: Arc<dyn BlockDevice> = if detected.block_size == crate::config::PAGE_SIZE {
        device
    } else {
        Arc::new(crate::drivers::block::partition::BlockSizeAdapter::new(
            device,
            detected.block_size,
        ))
    };
    if read_only {
        Arc::new(crate::drivers::block::partition::ReadOnlyBlockDevice::new(device))
    } else {
        device
    }
}

/// initramfs 模式：创建 RamFS → 解包内嵌 cpio → 准备挂载点并挂载 devfs。
/// 完全不依赖 BLOCK_DEVICE；PID1 负责其余伪文件系统和磁盘挂载策略。
#[cfg(feature = "initramfs")]
lazy_static! {
    pub static ref VFS_ROOT: Arc<self::vfs::MountFS> = {
        let ramfs = self::ramfs::RamFS::new();
        let mfs = self::vfs::MountFS::new(self::vfs::BackendLifecycle::new(ramfs), self::vfs::MountFlags::empty());

        // 解包 initramfs cpio
        match self::initramfs::unpack_embedded(&mfs) {
            Ok(stats) => {
                println!(
                    "[initramfs] unpacked: files={} dirs={} symlinks={} bytes={}",
                    stats.files, stats.dirs, stats.symlinks, stats.bytes,
                );
            }
            Err(e) => {
                println!("[initramfs] WARNING: unpack failed: {:?}, continuing with empty root", e);
            }
        }

        // Only devfs belongs to the kernel bootstrap: TaskControlBlock::new()
        // opens /dev/tty for PID1's fd 0/1/2. All other mounts are PID1 policy.
        prepare_kernel_bootstrap_filesystem(&mfs);
        mfs
    };
}

/// Create PID1 mount points and install the minimal devfs/tty bootstrap.
///
/// The directories are not mounts: PID1 owns procfs, sysfs, tmpfs, and disk
/// policy after the kernel has registered available block devices.
fn prepare_kernel_bootstrap_filesystem(mfs: &Arc<self::vfs::MountFS>) {
    let root = mfs.mountpoint_root_inode();

    for (name, mode) in [
        ("proc", 0o555),
        ("sys", 0o555),
        ("run", 0o755),
        ("tmp", 0o1777),
        ("sdcard", 0o755),
        ("tools", 0o755),
        ("mnt", 0o755),
        ("var", 0o755),
    ] {
        root.find(name).unwrap_or_else(|_| {
            root.create(
                name,
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(mode),
            )
            .expect("failed to create initramfs mount point")
        });
    }
    let var = root.find("var").expect("missing /var mount point");
    var.find("tmp").unwrap_or_else(|_| {
        var.create(
            "tmp",
            self::vfs::FileType::Dir,
            self::vfs::InodeMode::from_bits_truncate(0o1777),
        )
        .expect("failed to create /var/tmp")
    });

    // /dev remains a kernel responsibility because INITPROC opens /dev/tty
    // before userspace has executed its first instruction.
    {
        let dev_inode = root.find("dev").unwrap_or_else(|_| {
            root.create(
                "dev",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o755),
            )
            .expect("failed to create /dev")
        });
        let devfs = crate::fs::dev::DEV_FS.clone();
        devfs
            .add_dev(
                "tty",
                crate::fs::dev::tty::TTY.clone() as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/tty");
        devfs
            .add_dev(
                "null",
                alloc::sync::Arc::new(crate::fs::dev::null::Null) as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/null");
        devfs
            .add_dev(
                "zero",
                alloc::sync::Arc::new(crate::fs::dev::zero::Zero) as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/zero");
        devfs
            .add_dev(
                "urandom",
                alloc::sync::Arc::new(crate::fs::dev::urandom::Urandom)
                    as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/urandom");
        devfs
            .add_dev(
                "full",
                alloc::sync::Arc::new(crate::fs::dev::full::Full) as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/full");
        devfs
            .add_dev(
                "random",
                alloc::sync::Arc::new(crate::fs::dev::urandom::Urandom)
                    as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/random");
        devfs
            .add_dev(
                "console",
                crate::fs::dev::tty::TTY.clone() as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/console");
        devfs
            .add_dev(
                "ptmx",
                alloc::sync::Arc::new(crate::fs::dev::pty::PtmxMasterInode)
                    as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/ptmx");
        devfs
            .add_dev(
                "pts",
                alloc::sync::Arc::new(crate::fs::dev::pty::PtsDirInode)
                    as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/pts");
        devfs
            .add_dev(
                "rtc",
                alloc::sync::Arc::new(crate::fs::dev::rtc::Rtc) as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/rtc");
        devfs
            .add_dev(
                "cpu_dma_latency",
                alloc::sync::Arc::new(crate::fs::dev::null::Null) as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/cpu_dma_latency");
        let misc_dir = devfs
            .add_dir("misc", self::vfs::InodeMode::from_bits_truncate(0o755))
            .expect("devfs: failed to register /dev/misc");
        misc_dir
            .add_dev(
                "rtc",
                alloc::sync::Arc::new(crate::fs::dev::rtc::Rtc) as Arc<dyn self::vfs::IndexNode>,
            )
            .expect("devfs: failed to register /dev/misc/rtc");
        // Keep only the cover directory. PID1 mounts tmpfs over it after
        // procfs, sysfs, /run, and /tmp are ready.
        devfs
            .add_dir("shm", self::vfs::InodeMode::from_bits_truncate(0o1777))
            .expect("devfs: failed to register /dev/shm");
        let dev_inode_id = dev_inode
            .metadata()
            .expect("dev_inode metadata failed")
            .inode_id;
        let devfs_mnt = self::vfs::MountFS::new(
            self::vfs::BackendLifecycle::new(devfs),
            self::vfs::MountFlags::empty(),
        );
        devfs_mnt.set_mount_path(Some(alloc::string::String::from("/dev")));
        if let Some(dev_mfsi) = dev_inode
            .as_any_ref()
            .downcast_ref::<self::vfs::MountFSInode>()
        {
            let backref = self::vfs::MountFSInode::new(
                dev_mfsi.inner_inode.clone(),
                dev_mfsi.mount_fs.clone(),
            );
            devfs_mnt.set_self_mountpoint(Some(backref));
        }
        mfs.add_mount(dev_inode_id, devfs_mnt)
            .expect("failed to mount devfs at /dev");
    }
}

/// 返回新的 VFS 根（MountFS 实例）的共享引用。
pub fn vfs_root() -> Arc<self::vfs::MountFS> {
    VFS_ROOT.clone()
}

/// Discover boot disks and register `/dev/vd*` nodes without mounting them.
pub fn register_boot_block_devices() {
    register_discovered_boot_block_devices();
}

/// 主动初始化 initramfs VFS_ROOT（触发 lazy_static）。
/// 在 `mm::init()` 之后、`drivers::init_net_device()` 之前调用。
#[cfg(feature = "initramfs")]
pub fn initramfs_init() {
    let _root = VFS_ROOT.clone();
    println!("[initramfs] VFS_ROOT initialized (ramfs + cpio unpack + devfs bootstrap)");
}

/// 路径规范化：按 '/' 分割，处理 '.' 和 '..'
pub fn parse_path(path: &str) -> alloc::vec::Vec<alloc::string::String> {
    path.split('/')
        .fold(alloc::vec::Vec::with_capacity(8), |mut v, s| {
            match s {
                "" | "." => {}
                ".." => {
                    if v.last().map_or(true, |s| s == "..") {
                        v.push(String::from(s));
                    } else {
                        v.pop();
                    }
                }
                _ => v.push(String::from(s)),
            }
            v
        })
}

/// 符号链接最大跟随层数
const MAX_SYMLINK_FOLLOW: usize = 40;

/// 核心路径查找 — 支持符号链接跟随。
///
/// Resolve the effective root inode for absolute paths:
/// uses per-process chroot root if set, otherwise falls back to the global VFS root.
pub fn current_root_inode() -> Arc<dyn self::vfs::IndexNode> {
    crate::task::current_task()
        .and_then(|t| {
            let fs = t.process.fs();
            let lock = fs.lock();
            lock.root_inode.clone()
        })
        .unwrap_or_else(|| vfs_root().mountpoint_root_inode())
}

/// 对标 DragonOS `IndexNode::do_lookup_follow_symlink`。
///
/// - `start`: 查找起点（对于绝对路径传入 `vfs_root().root_inode()`）
/// - `path`: 待解析的路径（可为绝对路径或相对路径）
/// - `follow_final`: 是否跟随最后一个路径组件的符号链接
use core::sync::atomic::AtomicUsize;

static VFS_LOOKUP_CALLS: AtomicUsize = AtomicUsize::new(0);
static VFS_LOOKUP_TICKS: AtomicUsize = AtomicUsize::new(0);

pub fn reset_vfs_lookup_counters() {
    VFS_LOOKUP_CALLS.store(0, Ordering::Relaxed);
    VFS_LOOKUP_TICKS.store(0, Ordering::Relaxed);
}

pub fn vfs_lookup_calls() -> usize {
    VFS_LOOKUP_CALLS.load(Ordering::Relaxed)
}
pub fn vfs_lookup_ticks() -> usize {
    VFS_LOOKUP_TICKS.load(Ordering::Relaxed)
}

#[cfg(feature = "perf_diag")]
struct VfsLookupGuard(usize);
#[cfg(feature = "perf_diag")]
impl Drop for VfsLookupGuard {
    fn drop(&mut self) {
        VFS_LOOKUP_TICKS.fetch_add(
            crate::timer::get_time().wrapping_sub(self.0) as usize,
            Ordering::Relaxed,
        );
    }
}

pub fn vfs_lookup(
    start: &Arc<dyn self::vfs::IndexNode>,
    path: &str,
    follow_final: bool,
) -> Result<Arc<dyn self::vfs::IndexNode>, isize> {
    #[cfg(feature = "perf_diag")]
    {
        VFS_LOOKUP_CALLS.fetch_add(1, Ordering::Relaxed);
        let _guard = VfsLookupGuard(crate::timer::get_time());
    }
    use self::vfs::{FilePrivateData, FileType, IndexNode as _};
    let root_inode: Arc<dyn self::vfs::IndexNode> = current_root_inode();

    let has_trailing_slash = path.ends_with('/') && path != "/";

    let (mut current, mut components) = if let Some(rest) = path.strip_prefix('/') {
        (root_inode.clone(), parse_path(rest))
    } else {
        (start.clone(), parse_path(path))
    };

    // Linux returns -1/ENOENT for empty path (including empty relative path)
    if components.is_empty() && !path.starts_with('/') && path != "." && path != ".." {
        return Err(crate::syscall::errno::ENOENT);
    }

    let mut symlink_count = 0;
    let mut comp_idx = 0;

    while comp_idx < components.len() {
        let name = &components[comp_idx];
        let is_last = comp_idx == components.len() - 1;

        let cur_type = current.metadata().map_err(|e| -(e as isize))?.file_type;
        if cur_type != FileType::Dir {
            return Err(crate::syscall::errno::ENOTDIR);
        }

        // ".." 解析：先尝试 find("..")，失败后通过 absolute_path() 回退
        if name == ".." {
            // 在 chroot 根目录或全局根目录阻止 ".." 逃逸
            if alloc::sync::Arc::ptr_eq(&current, &root_inode) {
                comp_idx += 1;
                continue;
            }
            if let Ok(parent) = current.find("..") {
                current = parent;
            } else if let Ok(abs) = current.absolute_path() {
                let parent_path = if let Some(pos) = abs.rfind('/') {
                    if pos == 0 {
                        "/"
                    } else {
                        &abs[..pos]
                    }
                } else {
                    "/"
                };
                current = vfs_lookup(&root_inode, parent_path, true)?;
            }
            comp_idx += 1;
            continue;
        }

        let next = current.find(name).map_err(|e| -(e as isize))?;

        let next_md = next.metadata().map_err(|e| -(e as isize))?;
        let file_type = next_md.file_type;

        if !is_last && file_type != FileType::Dir && file_type != FileType::SymLink {
            return Err(crate::syscall::errno::ENOTDIR);
        }

        if is_last && !follow_final && file_type == FileType::SymLink && !has_trailing_slash {
            return Ok(next);
        }

        if file_type == FileType::SymLink {
            // MS_NOSYMFOLLOW — if the mount containing this symlink has the
            // flag set, do NOT follow; return ELOOP (Linux mount(2) semantics).
            // Check the symlink's MountFS (next), not the parent's (current).
            if let Some(next_mfsi) = next
                .as_any_ref()
                .downcast_ref::<crate::fs::vfs::MountFSInode>()
            {
                if next_mfsi
                    .mount_fs
                    .mount_flags()
                    .contains(crate::fs::vfs::MountFlags::NOSYMFOLLOW)
                {
                    return Err(crate::syscall::errno::ELOOP);
                }
            }

            if symlink_count >= MAX_SYMLINK_FOLLOW {
                return Err(crate::syscall::errno::ELOOP);
            }
            symlink_count += 1;

            // 读取符号链接内容（所有 inode 现在都原生实现 IndexNode）
            let target: String = {
                let link_len = next_md.size.max(0) as usize;
                let mut link_buf = alloc::vec![0u8; link_len.min(4096)];
                let n = next
                    .read_at(
                        0,
                        link_buf.len(),
                        &mut link_buf,
                        spin::Mutex::new(FilePrivateData::Unused).lock(),
                    )
                    .map_err(|e| -(e as isize))?;
                // Safety: `n` comes from `read_at()` which returns the number
                // of bytes actually read; this is always ≤ `link_buf.len()`
                // (the buffer is initialised with capacity
                // `link_len.min(4096)` directly above).  `set_len(n)` shrinks
                // the logical length within the allocated capacity.
                unsafe { link_buf.set_len(n) };
                let s = core::str::from_utf8(&link_buf[..n])
                    .map_err(|_| crate::syscall::errno::EINVAL)?;
                String::from(s.trim_end_matches('\0'))
            };

            // 组装新路径：符号链接目标 + 剩余组件
            let remaining: alloc::vec::Vec<&str> = components[comp_idx + 1..]
                .iter()
                .map(|s| s.as_str())
                .collect();
            let new_path = if remaining.is_empty() {
                String::from(target)
            } else {
                alloc::format!("{}/{}", target, remaining.join("/"))
            };

            // 将符号链接目标 + 剩余组件组装为最终查找路径
            if new_path.starts_with('/') {
                // 绝对符号链接目标：从根重新开始
                components = parse_path(if let Some(rest) = new_path.strip_prefix('/') {
                    rest
                } else {
                    &new_path
                });
                current = root_inode.clone();
                comp_idx = 0;
                continue;
            } else {
                // 相对符号链接目标：POSIX 语义 — 以 symlink 所在父目录为起点解析
                components = parse_path(&new_path);
                // current 保持为 symlink 的父目录，comp_idx 归零继续
                comp_idx = 0;
                continue;
            };
        } else {
            current = next;
            comp_idx += 1;
        }
    }

    if has_trailing_slash {
        let final_type = current.metadata().map_err(|e| -(e as isize))?.file_type;
        if final_type != FileType::Dir {
            return Err(crate::syscall::errno::ENOTDIR);
        }
    }

    Ok(current)
}

/// 使用新 VFS 解析绝对路径，跟随所有符号链接，返回目标 IndexNode。
pub fn vfs_lookup_absolute(path: &str) -> Result<Arc<dyn self::vfs::IndexNode>, isize> {
    vfs_lookup(&current_root_inode(), path, true)
}

/// 使用新 VFS 解析路径，返回 (父目录 IndexNode, 最后一级文件名)。
///
/// 用于需要修改目录结构的操作（mkdir/unlink/rename/symlink/open 创建模式）。
/// 中间路径组件的符号链接会被跟随。
pub fn vfs_lookup_parent(path: &str) -> Result<(Arc<dyn self::vfs::IndexNode>, String), isize> {
    let components = parse_path(path);
    let leaf = components.last().ok_or(crate::syscall::errno::ENOENT)?;
    let leaf_name = leaf.clone();

    // 构建父目录路径（不含最后一级）
    let parent_path = if components.len() == 1 {
        if path.starts_with('/') {
            String::from("/")
        } else {
            String::from(".")
        }
    } else {
        let parent_comps = &components[..components.len() - 1];
        let joined = parent_comps
            .iter()
            .map(|s| s.as_str())
            .collect::<alloc::vec::Vec<&str>>()
            .join("/");
        if path.starts_with('/') {
            alloc::format!("/{}", joined)
        } else {
            joined
        }
    };

    vfs_lookup(&current_root_inode(), &parent_path, true).map(|parent| (parent, leaf_name))
}

/// `vfs_lookup_parent` 的变体：支持指定起始 inode（用于相对路径+dirfd 场景）
pub fn vfs_lookup_parent_for_start(
    start: &Arc<dyn self::vfs::IndexNode>,
    path: &str,
) -> Result<(Arc<dyn self::vfs::IndexNode>, String), isize> {
    let components = parse_path(path);
    let leaf = components.last().ok_or(crate::syscall::errno::ENOENT)?;
    let leaf_name = leaf.clone();

    let parent_dir = if components.len() == 1 {
        if path.starts_with('/') {
            current_root_inode()
        } else {
            start.clone()
        }
    } else {
        let parent_comps = &components[..components.len() - 1];
        let joined = parent_comps
            .iter()
            .map(|s| s.as_str())
            .collect::<alloc::vec::Vec<&str>>()
            .join("/");
        let parent_path = if path.starts_with('/') {
            if joined.is_empty() {
                String::from("/")
            } else {
                alloc::format!("/{}", joined)
            }
        } else {
            joined
        };
        vfs_lookup(start, &parent_path, true)?
    };

    Ok((parent_dir, leaf_name))
}
