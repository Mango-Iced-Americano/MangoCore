pub mod dev;
pub mod eventfd;
pub mod eventpoll;
pub mod ext4;
pub mod fat32;
pub mod pidfd;
mod filesystem;
#[cfg(feature = "initramfs")]
pub mod initramfs;
pub mod iov;
mod layout;
mod page_cache;
pub use page_cache::{entries_global_stats, evict_all_clean_pages, flush_all_page_caches, registry_stats};
pub mod poll;
pub mod procfs;
pub mod ramfs;
pub mod reclaim;
pub mod sysfs;
pub mod timerfd;
pub mod tmpfs;
#[cfg(feature = "swap")]
pub mod swap;
// Xein add this
pub mod dirent;
// file_descriptor module removed — migrated to vfs::File
mod inode;
mod timestamp;
pub mod vfs;

pub use self::dev::{
    pipe::*,
};

pub use self::layout::*;

pub use self::fat32::DiskInodeType;
pub use crate::drivers::block::BlockDevice;
pub use self::filesystem::{detect_fs, FS_Type};

use alloc::{string::String, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};
pub use dirent::Dirent;
use lazy_static::*;
use self::vfs::IndexNode;
use self::vfs::FileSystem as _;

/// 强制使用 ramfs，跳过块设备检测（用于 VFS 层调试 / legacy_block_root 模式）
static FORCE_RAMFS: AtomicBool = AtomicBool::new(false);

/// 在 BLOCK_DEVICE 初始化之前调用此函数，可跳过块设备检测，直接使用 ramfs 启动
pub fn force_ramfs() {
    FORCE_RAMFS.store(true, Ordering::Relaxed);
    crate::drivers::block::disable_block_device();
}

// ── VFS_ROOT：根据特性选择初始化策略 ──

/// 非 initramfs 模式：传统块设备检测，失败时 fallback 到 ramfs
#[cfg(not(feature = "initramfs"))]
lazy_static! {
    pub static ref VFS_ROOT: Arc<self::vfs::MountFS> = {
        let fs_type = if FORCE_RAMFS.load(Ordering::Relaxed) {
            self::filesystem::FS_Type::Null
        } else {
            self::filesystem::pre_mount()
        };
        let mfs = match fs_type {
            self::filesystem::FS_Type::Fat32 => {
                let efs = self::fat32::EasyFileSystem::open(
                    crate::drivers::BLOCK_DEVICE.clone(),
                );
                self::vfs::MountFS::new(efs, self::vfs::MountFlags::empty())
            }
            self::filesystem::FS_Type::Ext4 => {
                let ext4 = self::ext4::ext4fs::Ext4FileSystem::open_ext4rs(
                    crate::drivers::BLOCK_DEVICE.clone(),
                );
                self::vfs::MountFS::new(ext4, self::vfs::MountFlags::empty())
            }
            self::filesystem::FS_Type::Null => {
                println!("[kernel] No filesystem found, falling back to ramfs");
                let ramfs = self::ramfs::RamFS::new();
                self::vfs::MountFS::new(ramfs, self::vfs::MountFlags::empty())
            }
        };
        mount_common_filesystems(&mfs);
        mfs
    };
}

/// initramfs 模式：创建 RamFS → 解包内嵌 cpio → 挂载 devfs/procfs/tmpfs
/// 完全不依赖 BLOCK_DEVICE，块设备在后续阶段可选挂载到 /sdcard 和 /tools
#[cfg(feature = "initramfs")]
lazy_static! {
    pub static ref VFS_ROOT: Arc<self::vfs::MountFS> = {
        let ramfs = self::ramfs::RamFS::new();
        let mfs = self::vfs::MountFS::new(ramfs, self::vfs::MountFlags::empty());

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

        // 挂载 devfs/procfs/tmpfs（不含 /dev/vda/vdb）
        mount_common_filesystems(&mfs);
        mfs
    };
}

/// 无论根文件系统类型，统一挂载 DevFS、ProcFS、tmpfs
fn mount_common_filesystems(mfs: &Arc<self::vfs::MountFS>) {
    let root = mfs.mountpoint_root_inode();

    // ── /dev — 设备文件系统 ──
    {
        let dev_inode = root.find("dev").unwrap_or_else(|_| {
            root.create("dev", self::vfs::FileType::Dir, self::vfs::InodeMode::from_bits_truncate(0o755))
                .expect("failed to create /dev")
        });
        let devfs = crate::fs::dev::DEV_FS.clone();
        devfs.add_dev("tty", crate::fs::dev::tty::TTY.clone() as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/tty");
        devfs.add_dev("null", alloc::sync::Arc::new(crate::fs::dev::null::Null) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/null");
        devfs.add_dev("zero", alloc::sync::Arc::new(crate::fs::dev::zero::Zero) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/zero");
        devfs.add_dev("urandom", alloc::sync::Arc::new(crate::fs::dev::urandom::Urandom) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/urandom");
        devfs.add_dev("full", alloc::sync::Arc::new(crate::fs::dev::full::Full) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/full");
        devfs.add_dev("random", alloc::sync::Arc::new(crate::fs::dev::urandom::Urandom) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/random");
        devfs.add_dev("console", crate::fs::dev::tty::TTY.clone() as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/console");
        devfs.add_dev("ptmx", alloc::sync::Arc::new(crate::fs::dev::pty::PtmxMasterInode) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/ptmx");
        devfs.add_dev("pts", alloc::sync::Arc::new(crate::fs::dev::pty::PtsDirInode) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/pts");
        devfs.add_dev("rtc", alloc::sync::Arc::new(crate::fs::dev::rtc::Rtc) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/rtc");
        devfs.add_dev("cpu_dma_latency", alloc::sync::Arc::new(crate::fs::dev::null::Null) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/cpu_dma_latency");
        let misc_dir = devfs
            .add_dir("misc", self::vfs::InodeMode::from_bits_truncate(0o755))
            .expect("devfs: failed to register /dev/misc");
        misc_dir
            .add_dev("rtc", alloc::sync::Arc::new(crate::fs::dev::rtc::Rtc) as Arc<dyn self::vfs::IndexNode>)
            .expect("devfs: failed to register /dev/misc/rtc");
        let shmfs = crate::fs::tmpfs::TmpFS::new_with_options(4096 * 4096); // ~16MB for /dev/shm
        if let Ok(mut meta) = shmfs.root_inode().metadata() {
            meta.mode = self::vfs::InodeMode::from_bits_truncate(0o1777);
            shmfs.root_inode().set_metadata(&meta).ok();
        }
        // Create a regular directory in devfs as the cover mount point,
        // rather than leaking shmfs.root_inode() directly.
        let shm_dir = devfs
            .add_dir("shm", self::vfs::InodeMode::from_bits_truncate(0o1777))
            .expect("devfs: failed to register /dev/shm");
        let shm_inode_id = shm_dir
            .metadata()
            .expect("devfs: failed to read /dev/shm metadata")
            .inode_id;
        let dev_inode_id = dev_inode.metadata().expect("dev_inode metadata failed").inode_id;
        let devfs_mnt = self::vfs::MountFS::new(devfs, self::vfs::MountFlags::empty());
        devfs_mnt.set_mount_path(Some(alloc::string::String::from("/dev")));
        if let Some(dev_mfsi) = dev_inode.as_any_ref().downcast_ref::<self::vfs::MountFSInode>() {
            let backref = self::vfs::MountFSInode::new(
                dev_mfsi.inner_inode.clone(),
                dev_mfsi.mount_fs.clone(),
            );
            devfs_mnt.set_self_mountpoint(Some(backref));
        }
        // Mount shmfs as a sub-mount of devfs, so MountFS owns the Arc<TmpFS>
        // and TmpFSInode.fs.upgrade() stays valid.
        let shmfs_mnt = self::vfs::MountFS::new(shmfs, self::vfs::MountFlags::empty());
        shmfs_mnt.set_mount_path(Some(alloc::string::String::from("/dev/shm")));
        let shm_backref = self::vfs::MountFSInode::new(
            shm_dir.clone() as Arc<dyn self::vfs::IndexNode>,
            devfs_mnt.clone(),
        );
        shmfs_mnt.set_self_mountpoint(Some(shm_backref));
        devfs_mnt
            .add_mount(shm_inode_id, shmfs_mnt)
            .expect("failed to mount tmpfs at /dev/shm");
        mfs.add_mount(dev_inode_id, devfs_mnt)
            .expect("failed to mount devfs at /dev");
    }

    // ── /proc — 进程信息文件系统 ──
    {
        let proc_inode = root.find("proc").unwrap_or_else(|_| {
            root.create("proc", self::vfs::FileType::Dir, self::vfs::InodeMode::from_bits_truncate(0o555))
                .expect("failed to create /proc")
        });
        let proc_inode_id = proc_inode.metadata().expect("proc_inode metadata failed").inode_id;
        let procfs = crate::fs::procfs::ProcFS::new();
        crate::fs::procfs::files::register_all(procfs.root())
            .expect("procfs: failed to register root entries");
        let procfs_mnt = self::vfs::MountFS::new(procfs, self::vfs::MountFlags::empty());
        procfs_mnt.no_dentry_cache.store(true, core::sync::atomic::Ordering::Relaxed);
        procfs_mnt.set_mount_path(Some(alloc::string::String::from("/proc")));
        if let Some(proc_mfsi) = proc_inode.as_any_ref().downcast_ref::<self::vfs::MountFSInode>() {
            let backref = self::vfs::MountFSInode::new(
                proc_mfsi.inner_inode.clone(),
                proc_mfsi.mount_fs.clone(),
            );
            procfs_mnt.set_self_mountpoint(Some(backref));
        }
        mfs.add_mount(proc_inode_id, procfs_mnt)
            .expect("failed to mount procfs at /proc");
    }

     // ── /sys — kernel object filesystem ──
    {
        let sys_inode = root.find("sys").unwrap_or_else(|_| {
            root.create("sys", self::vfs::FileType::Dir, self::vfs::InodeMode::from_bits_truncate(0o555))
                .expect("failed to create /sys")
        });
        let sys_inode_id = sys_inode.metadata().expect("sys_inode metadata failed").inode_id;
        let sysfs = crate::fs::sysfs::SysFS::new();
        crate::fs::sysfs::files::register_all(sysfs.root())
            .expect("sysfs: failed to register root entries");
        let sysfs_mnt = self::vfs::MountFS::new(sysfs, self::vfs::MountFlags::empty());
        sysfs_mnt.no_dentry_cache.store(true, core::sync::atomic::Ordering::Relaxed);
        sysfs_mnt.set_mount_path(Some(alloc::string::String::from("/sys")));
        if let Some(sys_mfsi) = sys_inode.as_any_ref().downcast_ref::<self::vfs::MountFSInode>() {
            let backref = self::vfs::MountFSInode::new(
                sys_mfsi.inner_inode.clone(),
                sys_mfsi.mount_fs.clone(),
            );
            sysfs_mnt.set_self_mountpoint(Some(backref));
        }
        mfs.add_mount(sys_inode_id, sysfs_mnt)
            .expect("failed to mount sysfs at /sys");
    }

     // ── /tmp — 临时文件系统（ramfs, 不受配额限制）──
    {
        let tmp_inode = root.find("tmp").unwrap_or_else(|_| {
            root.create("tmp", self::vfs::FileType::Dir, self::vfs::InodeMode::from_bits_truncate(0o1777))
                .expect("failed to create /tmp")
        });
        let tmp_inode_id = tmp_inode.metadata().expect("tmp_inode metadata failed").inode_id;
        let tmpfs = crate::fs::tmpfs::TmpFS::new(); // unlimited for /tmp
        // 设置挂载后的根 inode 为 01777（sticky bit + 全局可读写）
        if let Ok(mut meta) = tmpfs.root_inode().metadata() {
            meta.mode = self::vfs::InodeMode::from_bits_truncate(0o1777);
            tmpfs.root_inode().set_metadata(&meta).ok();
        }
        let tmpfs_mnt = self::vfs::MountFS::new(tmpfs, self::vfs::MountFlags::empty());
        tmpfs_mnt.set_mount_path(Some(alloc::string::String::from("/tmp")));
        if let Some(tmp_mfsi) = tmp_inode.as_any_ref().downcast_ref::<self::vfs::MountFSInode>() {
            let backref = self::vfs::MountFSInode::new(
                tmp_mfsi.inner_inode.clone(),
                tmp_mfsi.mount_fs.clone(),
            );
            tmpfs_mnt.set_self_mountpoint(Some(backref));
        }
        mfs.add_mount(tmp_inode_id, tmpfs_mnt)
            .expect("failed to mount tmpfs at /tmp");
    }
    // ── /mnt — 挂载点目录 ──
    {
        root.find("mnt").unwrap_or_else(|_| {
            root.create("mnt", self::vfs::FileType::Dir, self::vfs::InodeMode::from_bits_truncate(0o755))
                .expect("failed to create /mnt")
        });
    }
    // ── /run — 运行时文件 ──
    {
        root.find("run").unwrap_or_else(|_| {
            root.create("run", self::vfs::FileType::Dir, self::vfs::InodeMode::from_bits_truncate(0o755))
                .expect("failed to create /run")
        });
    }
    // ── /var/tmp — 临时文件备选 ──
    {
        root.find("var").unwrap_or_else(|_| {
            root.create("var", self::vfs::FileType::Dir, self::vfs::InodeMode::from_bits_truncate(0o755))
                .expect("failed to create /var")
        });
        let var = root.find("var").unwrap();
        var.find("tmp").unwrap_or_else(|_| {
            var.create("tmp", self::vfs::FileType::Dir, self::vfs::InodeMode::from_bits_truncate(0o1777))
                .expect("failed to create /var/tmp")
        });
    }
}

/// 启动时挂载块设备上的文件系统。
///
/// 在 VFS_ROOT 下创建挂载点目录，打开块设备上的 ext4/fat32，
/// 包装为 MountFS 并注册到挂载树。
///
/// 返回挂载后的 MountFS，失败时打印错误并返回 None。
pub fn mount_block_fs(
    parent_mfs: &Arc<self::vfs::MountFS>,
    block_device: &Arc<dyn BlockDevice>,
    mount_point: &str,
    label: &str,
) -> Option<Arc<self::vfs::MountFS>> {
    use self::filesystem::detect_fs;
    use self::vfs::MountFlags;

    let fs_type = detect_fs(block_device);
    let mfs = match fs_type {
        self::filesystem::FS_Type::Ext4 => {
            let ext4 = self::ext4::ext4fs::Ext4FileSystem::open_ext4rs(block_device.clone());
            self::vfs::MountFS::new(ext4, MountFlags::empty())
        }
        self::filesystem::FS_Type::Fat32 => {
            let efs = self::fat32::EasyFileSystem::open(block_device.clone());
            self::vfs::MountFS::new(efs, MountFlags::empty())
        }
        self::filesystem::FS_Type::Null => {
            println!("[kernel] mount_block_fs: {} — no filesystem detected on block device", label);
            return None;
        }
    };

    let root = parent_mfs.mountpoint_root_inode();
    let mount_inode = root.find(mount_point).unwrap_or_else(|_| {
        root.create(mount_point, self::vfs::FileType::Dir,
            self::vfs::InodeMode::from_bits_truncate(0o755))
            .expect("failed to create mount point")
    });
    let inode_id = mount_inode.metadata().expect("mount_inode metadata failed").inode_id;

    let mount_path = if mount_point.starts_with('/') {
        alloc::string::String::from(mount_point)
    } else {
        alloc::format!("/{}", mount_point)
    };
    mfs.set_mount_path(Some(mount_path.clone()));
    mfs.set_mount_source(Some(mount_path));

    if let Some(mfsi) = mount_inode.as_any_ref().downcast_ref::<self::vfs::MountFSInode>() {
        let backref = self::vfs::MountFSInode::new(
            mfsi.inner_inode.clone(),
            mfsi.mount_fs.clone(),
        );
        mfs.set_self_mountpoint(Some(backref));
    } else {
        println!("[kernel] WARNING: mount_block_fs {} — mount_inode not a MountFSInode, self_mountpoint NOT set", label);
    }

    if let Err(e) = parent_mfs.add_mount(inode_id, mfs.clone()) {
        println!("[kernel] mount_block_fs: failed to mount {} at {}: {:?}", label, mount_point, e);
        return None;
    }

    println!("[kernel] {} mounted at {}", label, mount_point);
    Some(mfs)
}

/// 尝试挂载工具盘（BLOCK_DEVICES[1]）到 /tools。
/// 设备不存在或挂载失败时不 panic，打印日志并优雅跳过。
pub fn mount_tools_disk() {
    let tools_dev = match crate::drivers::BLOCK_DEVICES[1].as_ref() {
        Some(dev) => dev,
        None => {
            println!("[kernel] no tools disk (x1) found, skipping /tools mount");
            return;
        }
    };
    let root = vfs_root();
    mount_block_fs(&root, tools_dev, "tools", "tools disk");
}

/// 返回新的 VFS 根（MountFS 实例）的共享引用。
pub fn vfs_root() -> Arc<self::vfs::MountFS> {
    VFS_ROOT.clone()
}

/// 注册 /dev/vda 和 /dev/vdb 块设备节点到当前 devfs。
/// 无需单独调用 — 由 mount_boot_block_devices 间接调用。
pub fn register_block_device_nodes() {
    // 保留空函数体供未来 devfs 重构使用；当前块设备节点已在
    // mount_common_filesystems 中注册（使用 block_devices() 安全探测）。
}

/// 在 initramfs 模式下，首次触发 BLOCK_DEVICES 探测，并尝试挂载块设备：
/// - x0 → /sdcard（官方 fs）
/// - x1 → /tools（工具盘）
///
/// 任何设备缺失或挂载失败都只打印 warning，不 panic。
pub fn mount_boot_block_devices() {
    let root = vfs_root();

    // 首次真正触发 BLOCK_DEVICES probe，但此时 VFS_ROOT 已经存在
    let devs = crate::drivers::block::block_devices();

    // 注册 /dev/vda（原始根设备，不做 MBR 解析以避免 FAT32 55AA 误判）
    if let Some(ref blk0) = devs[0] {
        let _ = crate::fs::dev::DEV_FS.add_dev(
            "vda",
            crate::fs::dev::block::BlockDevInode::new(blk0.clone(), 0, String::from("vda")),
        );
    }

    // 尝试挂载 x0 → /sdcard
    match devs[0].as_ref() {
        Some(dev) => {
            if mount_block_fs(&root, dev, "sdcard", "official fs (x0)").is_none() {
                println!("[initramfs] official fs (x0) mount failed, leaving /sdcard empty");
            }
        }
        None => {
            println!("[initramfs] official fs (x0) not found, skipping /sdcard mount");
        }
    }

    // 注册 /dev/vdb（原始工具盘）+ MBR 分区解析 + 分区设备注册 + 挂载
    match devs[1].as_ref() {
        Some(raw_vdb) => {
            let _ = crate::fs::dev::DEV_FS.add_dev(
                "vdb",
                crate::fs::dev::block::BlockDevInode::new(
                    raw_vdb.clone(), 1, String::from("vdb"),
                ),
            );

            use crate::drivers::block::partition::{probe_mbr, PartitionBlockDevice, MbrProbe};

            match probe_mbr(raw_vdb) {
                MbrProbe::Partitions(parts) => {
                    let mut vdb1_dev: Option<Arc<dyn BlockDevice>> = None;

                    for p in &parts {
                        let part_dev = Arc::new(PartitionBlockDevice::new(
                            raw_vdb.clone(),
                            p.start_lba,
                            p.sectors,
                        )) as Arc<dyn BlockDevice>;

                        let name = alloc::format!("vdb{}", p.partno);
                        let minor = 1 + p.partno as u64; // vdb1→2, vdb2→3
                        let inode = crate::fs::dev::block::BlockDevInode::new(
                            part_dev.clone(), minor, name.clone(),
                        );
                        let _ = crate::fs::dev::DEV_FS.add_dev(&name, inode);
                        println!(
                            "[mbr] registered /dev/{} (type={:#x}, size={}M)",
                            name,
                            p.type_code,
                            p.sectors * 512 / (1024 * 1024)
                        );

                        let alias = alloc::format!("vda{}", p.partno);
                        let alias_minor = 100 + p.partno as u64;
                        let alias_inode = crate::fs::dev::block::BlockDevInode::new(
                            part_dev.clone(), alias_minor, alias.clone(),
                        );
                        let _ = crate::fs::dev::DEV_FS.add_dev(&alias, alias_inode);

                        if p.partno == 1 {
                            vdb1_dev = Some(part_dev);
                        }
                    }

                    // 从 vdb1 挂载 /tools
                    match vdb1_dev {
                        Some(ref dev) => {
                            if mount_block_fs(&root, dev, "tools", "tools disk (vdb1)").is_none() {
                                println!("[initramfs] tools disk (vdb1) mount failed, leaving /tools empty");
                            }
                        }
                        None => {
                            println!("[mbr] no partition 1 found, /tools not mounted");
                        }
                    }
                }
                MbrProbe::NoMbr => {
                    // 无 MBR → 旧镜像兼容：从原始 /dev/vdb 挂载
                    println!("[mbr] no MBR on tools disk, mounting raw /dev/vdb as /tools");
                    if mount_block_fs(&root, raw_vdb, "tools", "tools disk (raw vdb)").is_none() {
                        println!("[initramfs] tools disk (raw vdb) mount failed, leaving /tools empty");
                    }
                }
                MbrProbe::Unsupported => {
                    println!("[mbr] tools disk MBR present but no supported partitions, /tools not mounted");
                }
            }
        }
        None => {
            println!("[initramfs] tools disk (x1) not found, skipping /tools mount");
        }
    }
}

/// 主动初始化 initramfs VFS_ROOT（触发 lazy_static）。
/// 在 `mm::init()` 之后、`drivers::init_net_device()` 之前调用。
#[cfg(feature = "initramfs")]
pub fn initramfs_init() {
    let _root = VFS_ROOT.clone();
    println!("[initramfs] VFS_ROOT initialized (ramfs + cpio unpack + dev/proc/tmp)");
}

/// 安装预装载的用户态 payload（initproc、bash、busybox 等）。
/// 用于 initramfs + preload_payloads 迁移阶段。
#[cfg(feature = "preload_payloads")]
pub fn install_preload_payloads() {
    flush_preload();
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
pub fn vfs_lookup(
    start: &Arc<dyn self::vfs::IndexNode>,
    path: &str,
    follow_final: bool,
) -> Result<Arc<dyn self::vfs::IndexNode>, isize> {
    use self::vfs::{FileType, FilePrivateData, IndexNode as _};
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
                    if pos == 0 { "/" } else { &abs[..pos] }
                } else {
                    "/"
                };
                current = vfs_lookup(&root_inode, parent_path, true)?;
            }
            comp_idx += 1;
            continue;
        }

        let next = current.find(name).map_err(|e| -(e as isize))?;
        let file_type = next.metadata().map_err(|e| -(e as isize))?.file_type;

        if !is_last && file_type != FileType::Dir && file_type != FileType::SymLink {
            return Err(crate::syscall::errno::ENOTDIR);
        }

        if is_last && !follow_final && file_type == FileType::SymLink {
            return Ok(next);
        }

        if file_type == FileType::SymLink {
            if symlink_count >= MAX_SYMLINK_FOLLOW {
                return Err(crate::syscall::errno::ELOOP);
            }
            symlink_count += 1;

            // 读取符号链接内容（所有 inode 现在都原生实现 IndexNode）
            let target: String = {
                let md = next.metadata().map_err(|e| -(e as isize))?;
                let link_len = md.size.max(0) as usize;
                let mut link_buf = alloc::vec![0u8; link_len.min(4096)];
                let n = next
                    .read_at(
                        0,
                        link_buf.len(),
                        &mut link_buf,
                        spin::Mutex::new(FilePrivateData::Unused).lock(),
                    )
                    .map_err(|e| -(e as isize))?;
                unsafe { link_buf.set_len(n) };
                let s = core::str::from_utf8(&link_buf[..n])
                    .map_err(|_| crate::syscall::errno::EINVAL)?;
                String::from(s.trim_end_matches('\0'))
            };

            // 组装新路径：符号链接目标 + 剩余组件
            let remaining: alloc::vec::Vec<&str> =
                components[comp_idx + 1..].iter().map(|s| s.as_str()).collect();
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

/// 使用新 VFS 创建或打开文件，返回 Arc<vfs::File>
fn create_or_open_file(path: &str) -> Result<Arc<self::vfs::File>, isize> {
    use self::vfs::{FileType, InodeMode};
    let (parent, name) = vfs_lookup_parent(path)?;
    let inode = match parent.find(&name) {
        Ok(existing) => existing,
        Err(_) => parent
            .create(&name, FileType::File, InodeMode::S_IRWXU)
            .map_err(|e| e as isize)?,
    };
    self::vfs::File::new(inode, self::vfs::FileFlags::O_RDWR).map_err(|e| e as isize)
}

/// 使用新 VFS 查找文件并返回 metadata size
fn file_size(path: &str) -> Result<usize, isize> {
    let inode = vfs_lookup_absolute(path)?;
    Ok(inode.metadata().map_err(|e| e as isize)?.size.max(0) as usize)
}

fn ensure_etc_dir() -> Option<Arc<dyn self::vfs::IndexNode>> {
    let root = vfs_root().mountpoint_root_inode();
    match root.find("etc") {
        Ok(etc) => Some(etc),
        Err(_) => root
            .create(
                "etc",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o755),
            )
            .ok(),
    }
}

fn write_etc_compat_file(etc: &Arc<dyn self::vfs::IndexNode>, name: &str, content: &str) {
    use self::vfs::{FileFlags, FileType, InodeMode};

    let inode = match etc.find(name) {
        Ok(inode) => inode,
        Err(_) => match etc.create(name, FileType::File, InodeMode::from_bits_truncate(0o644)) {
            Ok(inode) => inode,
            Err(_) => return,
        },
    };
    if inode.metadata().map(|m| m.file_type) != Ok(FileType::File) {
        return;
    }
    let _ = inode.resize(0);
    let _ = self::vfs::File::new(inode, FileFlags::O_RDWR)
        .and_then(|file| file.write(content.as_bytes()));
}

fn ensure_ltp_compat_etc_files() {
    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/sh\n\
nobody:x:65534:65534:nobody:/nonexistent:/sbin/nologin\n";
    const GROUP: &str = "\
root:x:0:\n\
daemon:x:1:\n\
nogroup:x:65534:nobody\n\
nobody:x:65534:\n";
    const HOSTS: &str = "\
127.0.0.1 localhost\n\
::1 localhost ip6-localhost ip6-loopback\n";
    const PROTOCOLS: &str = "\
ip 0 IP\n\
hopopt 0 HOPOPT\n\
icmp 1 ICMP\n\
igmp 2 IGMP\n\
ggp 3 GGP\n\
ipv4 4 IPv4\n\
tcp 6 TCP\n\
udp 17 UDP\n\
ipv6 41 IPv6\n\
ipv6-route 43 IPv6-Route\n\
ipv6-frag 44 IPv6-Frag\n\
esp 50 ESP\n\
ah 51 AH\n\
ipv6-icmp 58 IPv6-ICMP\n\
ipv6-nonxt 59 IPv6-NoNxt\n\
ipv6-opts 60 IPv6-Opts\n\
raw 255 RAW\n";

    if let Some(etc) = ensure_etc_dir() {
        write_etc_compat_file(&etc, "passwd", PASSWD);
        write_etc_compat_file(&etc, "group", GROUP);
        write_etc_compat_file(&etc, "hosts", HOSTS);
        write_etc_compat_file(&etc, "protocols", PROTOCOLS);
        write_etc_compat_file(&etc, "termcap", "");
    }
}

#[allow(unused)]
pub fn flush_preload() {
    extern "C" {
        fn sinitproc();
        fn einitproc();
        fn sbash();
        fn ebash();
        fn sbusybox();
        fn ebusybox();
        fn sosconfig();
        fn eosconfig();
        fn sfstest();
        fn efstest();
        fn sltpcompat();
        fn eltpcompat();
        fn sltprunner();
        fn eltprunner();
    }
    println!(
        "sinitproc: {:X}, einitproc: {:X}, sbash: {:X}, ebash: {:X}, sbusybox: {:X}, ebusybox: {:X}, sosconfig: {:X}, eosconfig: {:X}",
        sinitproc as usize, einitproc as usize, sbash as usize, ebash as usize, sbusybox as usize, ebusybox as usize,
        sosconfig as usize, eosconfig as usize,
    );
    let initproc = create_or_open_file("initproc").unwrap();
    let initproc_len = einitproc as usize - sinitproc as usize;
    let written = initproc.write(unsafe {
        core::slice::from_raw_parts(sinitproc as *const u8, initproc_len)
    }).unwrap();
    log::debug!(
        "[kernel] flush_preload: initproc write len={} => written={} size_after={}",
        initproc_len,
        written,
        file_size("initproc").unwrap_or(0)
    );
    for ppn in crate::mm::PPNRange::new(
        crate::mm::PhysAddr::from(sinitproc as usize).floor(),
        crate::mm::PhysAddr::from(einitproc as usize).floor(),
    ) {
        crate::mm::frame_dealloc(ppn);
    }
    // bash/busybox/os_test.conf/fs_test: 失败不阻塞启动
    let _ = create_or_open_file("bash").map(|f| {
        let _ = f.write(unsafe { core::slice::from_raw_parts(sbash as *const u8, ebash as usize - sbash as usize) });
    });
    for ppn in crate::mm::PPNRange::new(
        crate::mm::PhysAddr::from(sbash as usize).floor(),
        crate::mm::PhysAddr::from(ebash as usize).floor(),
    ) {
        crate::mm::frame_dealloc(ppn);
    }
    let _ = create_or_open_file("busybox").map(|f| {
        let _ = f.write(unsafe { core::slice::from_raw_parts(sbusybox as *const u8, ebusybox as usize - sbusybox as usize) });
    });
    // /bin/busybox: 必须在 frame_dealloc 之前写入，否则嵌入数据已被释放
    {
        let _ = vfs_lookup_parent("/bin/busybox").or_else(|_| {
            let root = vfs_root().mountpoint_root_inode();
            let _ = root.create("bin", self::vfs::FileType::Dir, self::vfs::InodeMode::S_IRWXUGO);
            vfs_lookup_parent("/bin/busybox")
        }).map(|(parent, _)| {
            if parent.find("busybox").is_err() {
                let _ = parent.create("busybox", self::vfs::FileType::File, self::vfs::InodeMode::S_IRWXUGO);
            }
        });
        let _ = create_or_open_file("/bin/busybox").map(|f| {
            let _ = f.write(unsafe {
                core::slice::from_raw_parts(sbusybox as *const u8, ebusybox as usize - sbusybox as usize)
            });
        });
    }
    for ppn in crate::mm::PPNRange::new(
        crate::mm::PhysAddr::from(sbusybox as usize).floor(),
        crate::mm::PhysAddr::from(ebusybox as usize).floor(),
    ) {
        crate::mm::frame_dealloc(ppn);
    }
    match file_size("os_test.conf") {
        Ok(size) => {
            log::info!("[kernel] flush_preload: keep existing /os_test.conf size={}", size);
        }
        Err(_) => {
            let _ = create_or_open_file("os_test.conf").map(|f| {
                let _ = f.write(unsafe {
                    core::slice::from_raw_parts(sosconfig as *const u8, eosconfig as usize - sosconfig as usize)
                });
            });
        }
    }
    for ppn in crate::mm::PPNRange::new(
        crate::mm::PhysAddr::from(sosconfig as usize).floor(),
        crate::mm::PhysAddr::from(eosconfig as usize).floor(),
    ) {
        crate::mm::frame_dealloc(ppn);
    }
    {
        let _ = create_or_open_file("fs_test").map(|f| {
            let _ = f.write(unsafe { core::slice::from_raw_parts(sfstest as *const u8, efstest as usize - sfstest as usize) });
        });
        for ppn in crate::mm::PPNRange::new(
            crate::mm::PhysAddr::from(sfstest as usize).floor(),
            crate::mm::PhysAddr::from(efstest as usize).floor(),
        ) {
            crate::mm::frame_dealloc(ppn);
        }
    }
    {
        let _ = vfs_lookup_absolute("ltp_proto_compat.so")
            .and_then(|inode| inode.resize(0).map_err(|e| e as isize));
        let _ = create_or_open_file("ltp_proto_compat.so").map(|f| {
            let _ = f.write(unsafe { core::slice::from_raw_parts(sltpcompat as *const u8, eltpcompat as usize - sltpcompat as usize) });
        });
        for ppn in crate::mm::PPNRange::new(
            crate::mm::PhysAddr::from(sltpcompat as usize).floor(),
            crate::mm::PhysAddr::from(eltpcompat as usize).floor(),
        ) {
            crate::mm::frame_dealloc(ppn);
        }
    }
    {
        let _ = vfs_lookup_absolute("ltprunner")
            .and_then(|inode| inode.resize(0).map_err(|e| e as isize));
        let _ = create_or_open_file("ltprunner").map(|f| {
            let _ = f.write(unsafe { core::slice::from_raw_parts(sltprunner as *const u8, eltprunner as usize - sltprunner as usize) });
        });
        for ppn in crate::mm::PPNRange::new(
            crate::mm::PhysAddr::from(sltprunner as usize).floor(),
            crate::mm::PhysAddr::from(eltprunner as usize).floor(),
        ) {
            crate::mm::frame_dealloc(ppn);
        }
    }
    ensure_ltp_compat_etc_files();
}
