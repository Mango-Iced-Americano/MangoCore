pub mod dev;
pub mod eventfd;
pub mod eventpoll;
pub mod ext4;
pub mod fat32;
mod filesystem;
#[cfg(feature = "initramfs")]
pub mod initramfs;
pub mod iov;
mod layout;
mod page_cache;
pub mod pidfd;
pub use page_cache::{
    entries_global_stats, evict_all_clean_pages, flush_all_page_caches, registry_stats,
};
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
use core::sync::atomic::{AtomicBool, Ordering};
pub use dirent::Dirent;
use lazy_static::*;

/// 强制使用 ramfs，跳过块设备检测（用于 VFS 层调试 / legacy_block_root 模式）
static FORCE_RAMFS: AtomicBool = AtomicBool::new(false);

/// 在 BLOCK_DEVICE 初始化之前调用此函数，可跳过块设备检测，直接使用 ramfs 启动
pub fn force_ramfs() {
    FORCE_RAMFS.store(true, Ordering::Relaxed);
    crate::drivers::block::disable_block_device();
}

pub(crate) fn adapt_filesystem_device(
    block_device: Arc<dyn BlockDevice>,
    native_block_size: usize,
    read_only: bool,
) -> Arc<dyn BlockDevice> {
    use crate::drivers::block::partition::{BlockSizeAdapter, ReadOnlyBlockDevice};

    let mut device = block_device;
    if native_block_size != crate::hal::BLOCK_SZ {
        boot_trace!(
            "[fs] adapting native block size {} to platform block size {}",
            native_block_size,
            crate::hal::BLOCK_SZ
        );
        device = Arc::new(BlockSizeAdapter::new(device, native_block_size));
    }
    if read_only {
        device = Arc::new(ReadOnlyBlockDevice::new(device));
    }
    device
}

// ── VFS_ROOT：根据特性选择初始化策略 ──

/// 非 initramfs 模式：传统块设备检测，失败时 fallback 到 ramfs
#[cfg(not(feature = "initramfs"))]
lazy_static! {
    pub static ref VFS_ROOT: Arc<self::vfs::MountFS> = {
        let detected = if FORCE_RAMFS.load(Ordering::Relaxed) {
            None
        } else {
            self::filesystem::detect_fs_layout(&crate::drivers::BLOCK_DEVICE)
        };
        let mfs = match detected {
            Some(detected) if detected.fs_type == self::filesystem::FS_Type::Fat32 => {
                let device = adapt_filesystem_device(
                    crate::drivers::BLOCK_DEVICE.clone(),
                    detected.block_size,
                    false,
                );
                let efs = self::fat32::EasyFileSystem::open(device);
                self::vfs::MountFS::new(efs, self::vfs::MountFlags::empty())
            }
            Some(detected) if detected.fs_type == self::filesystem::FS_Type::Ext4 => {
                let device = adapt_filesystem_device(
                    crate::drivers::BLOCK_DEVICE.clone(),
                    detected.block_size,
                    false,
                );
                let ext4 = self::ext4::ext4fs::Ext4FileSystem::open_ext4rs(device);
                self::vfs::MountFS::new(ext4, self::vfs::MountFlags::empty())
            }
            _ => {
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
                boot_trace!(
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
        let dev_inode_id = dev_inode
            .metadata()
            .expect("dev_inode metadata failed")
            .inode_id;
        let devfs_mnt = self::vfs::MountFS::new(devfs, self::vfs::MountFlags::empty());
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
            root.create(
                "proc",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o555),
            )
            .expect("failed to create /proc")
        });
        let proc_inode_id = proc_inode
            .metadata()
            .expect("proc_inode metadata failed")
            .inode_id;
        let procfs = crate::fs::procfs::ProcFS::new();
        crate::fs::procfs::files::register_all(procfs.root())
            .expect("procfs: failed to register root entries");
        let procfs_mnt = self::vfs::MountFS::new(procfs, self::vfs::MountFlags::empty());
        procfs_mnt
            .no_dentry_cache
            .store(true, core::sync::atomic::Ordering::Relaxed);
        procfs_mnt.set_mount_path(Some(alloc::string::String::from("/proc")));
        if let Some(proc_mfsi) = proc_inode
            .as_any_ref()
            .downcast_ref::<self::vfs::MountFSInode>()
        {
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
            root.create(
                "sys",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o555),
            )
            .expect("failed to create /sys")
        });
        let sys_inode_id = sys_inode
            .metadata()
            .expect("sys_inode metadata failed")
            .inode_id;
        let sysfs = crate::fs::sysfs::SysFS::new();
        crate::fs::sysfs::files::register_all(sysfs.root())
            .expect("sysfs: failed to register root entries");
        let sysfs_mnt = self::vfs::MountFS::new(sysfs, self::vfs::MountFlags::empty());
        sysfs_mnt
            .no_dentry_cache
            .store(true, core::sync::atomic::Ordering::Relaxed);
        sysfs_mnt.set_mount_path(Some(alloc::string::String::from("/sys")));
        if let Some(sys_mfsi) = sys_inode
            .as_any_ref()
            .downcast_ref::<self::vfs::MountFSInode>()
        {
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
            root.create(
                "tmp",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o1777),
            )
            .expect("failed to create /tmp")
        });
        let tmp_inode_id = tmp_inode
            .metadata()
            .expect("tmp_inode metadata failed")
            .inode_id;
        let tmpfs = crate::fs::tmpfs::TmpFS::new(); // unlimited for /tmp
                                                    // 设置挂载后的根 inode 为 01777（sticky bit + 全局可读写）
        if let Ok(mut meta) = tmpfs.root_inode().metadata() {
            meta.mode = self::vfs::InodeMode::from_bits_truncate(0o1777);
            tmpfs.root_inode().set_metadata(&meta).ok();
        }
        let tmpfs_mnt = self::vfs::MountFS::new(tmpfs, self::vfs::MountFlags::empty());
        tmpfs_mnt.set_mount_path(Some(alloc::string::String::from("/tmp")));
        if let Some(tmp_mfsi) = tmp_inode
            .as_any_ref()
            .downcast_ref::<self::vfs::MountFSInode>()
        {
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
            root.create(
                "mnt",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o755),
            )
            .expect("failed to create /mnt")
        });
    }
    // ── /run — 运行时文件 ──
    {
        root.find("run").unwrap_or_else(|_| {
            root.create(
                "run",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o755),
            )
            .expect("failed to create /run")
        });
    }
    // ── /var/tmp — 临时文件备选 ──
    {
        root.find("var").unwrap_or_else(|_| {
            root.create(
                "var",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o755),
            )
            .expect("failed to create /var")
        });
        let var = root.find("var").unwrap();
        var.find("tmp").unwrap_or_else(|_| {
            var.create(
                "tmp",
                self::vfs::FileType::Dir,
                self::vfs::InodeMode::from_bits_truncate(0o1777),
            )
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
    mount_block_fs_with_flags(
        parent_mfs,
        block_device,
        mount_point,
        label,
        self::vfs::MountFlags::empty(),
    )
}

pub fn mount_block_fs_with_flags(
    parent_mfs: &Arc<self::vfs::MountFS>,
    block_device: &Arc<dyn BlockDevice>,
    mount_point: &str,
    label: &str,
    mount_flags: self::vfs::MountFlags,
) -> Option<Arc<self::vfs::MountFS>> {
    let detected = match self::filesystem::detect_fs_layout(block_device) {
        Some(detected) => detected,
        None => {
            println!(
                "[kernel] mount_block_fs: {} — no filesystem detected on block device",
                label
            );
            return None;
        }
    };
    let fs_device = adapt_filesystem_device(
        block_device.clone(),
        detected.block_size,
        mount_flags.contains(self::vfs::MountFlags::RDONLY),
    );

    let fs_type = detected.fs_type;
    let mfs = match fs_type {
        self::filesystem::FS_Type::Ext4 => {
            let ext4 = self::ext4::ext4fs::Ext4FileSystem::open_ext4rs(fs_device);
            self::vfs::MountFS::new(ext4, mount_flags)
        }
        self::filesystem::FS_Type::Fat32 => {
            let efs = self::fat32::EasyFileSystem::open(fs_device);
            self::vfs::MountFS::new(efs, mount_flags)
        }
        self::filesystem::FS_Type::Null => unreachable!(),
    };

    match mfs.mountpoint_root_inode().list() {
        Ok(entries) => boot_trace!(
            "[fs] {} root directory readable: {} entries",
            label,
            entries.len()
        ),
        Err(error) => {
            println!(
                "[kernel] mount_block_fs: {} root directory read failed: {:?}",
                label, error
            );
            return None;
        }
    }

    let root = parent_mfs.mountpoint_root_inode();
    let mount_inode = root.find(mount_point).unwrap_or_else(|_| {
        root.create(
            mount_point,
            self::vfs::FileType::Dir,
            self::vfs::InodeMode::from_bits_truncate(0o755),
        )
        .expect("failed to create mount point")
    });
    let inode_id = mount_inode
        .metadata()
        .expect("mount_inode metadata failed")
        .inode_id;

    let mount_path = if mount_point.starts_with('/') {
        alloc::string::String::from(mount_point)
    } else {
        alloc::format!("/{}", mount_point)
    };
    mfs.set_mount_path(Some(mount_path.clone()));
    mfs.set_mount_source(Some(mount_path));

    if let Some(mfsi) = mount_inode
        .as_any_ref()
        .downcast_ref::<self::vfs::MountFSInode>()
    {
        let backref = self::vfs::MountFSInode::new(mfsi.inner_inode.clone(), mfsi.mount_fs.clone());
        mfs.set_self_mountpoint(Some(backref));
    } else {
        println!("[kernel] WARNING: mount_block_fs {} — mount_inode not a MountFSInode, self_mountpoint NOT set", label);
    }

    if let Err(e) = parent_mfs.add_mount(inode_id, mfs.clone()) {
        println!(
            "[kernel] mount_block_fs: failed to mount {} at {}: {:?}",
            label, mount_point, e
        );
        return None;
    }

    boot_trace!(
        "[kernel] {} ({:?}) mounted at {} flags={:?}",
        label,
        fs_type,
        mount_point,
        mount_flags
    );
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

/// 注册启动块设备节点到当前 devfs。
/// 无需单独调用 — 由 mount_boot_block_devices 间接调用。
pub fn register_block_device_nodes() {
    // 保留空函数体供未来 devfs 重构使用；当前块设备节点已在
    // mount_common_filesystems 中注册（使用 block_devices() 安全探测）。
}

fn register_block_node(name: &str, device: Arc<dyn BlockDevice>, minor: u64, read_only: bool) {
    let inode = crate::fs::dev::block::BlockDevInode::new_with_read_only(
        device,
        minor,
        String::from(name),
        read_only,
    );
    if let Err(error) = crate::fs::dev::DEV_FS.add_dev(name, inode) {
        println!("[block] failed to register /dev/{}: {:?}", name, error);
    }
}

#[derive(Clone)]
struct MountCandidate {
    device: Arc<dyn BlockDevice>,
    source: String,
    partno: Option<u8>,
    partition_type: Option<u8>,
    fs_type: self::filesystem::FS_Type,
}

fn discover_mount_devices(
    raw: &Arc<dyn BlockDevice>,
    base_name: &str,
    raw_minor: u64,
    raw_alias: Option<(&str, u64)>,
    partition_alias_base: Option<&str>,
    read_only: bool,
) -> alloc::vec::Vec<MountCandidate> {
    use crate::drivers::block::partition::{probe_mbr, MbrProbe, PartitionBlockDevice};

    register_block_node(base_name, raw.clone(), raw_minor, read_only);
    if let Some((alias, minor)) = raw_alias {
        register_block_node(alias, raw.clone(), minor, read_only);
    }

    let raw_fs = self::filesystem::detect_fs(raw);
    if raw_fs != self::filesystem::FS_Type::Null {
        boot_trace!("[block] /dev/{} contains raw {:?}", base_name, raw_fs);
        return alloc::vec![MountCandidate {
            device: raw.clone(),
            source: alloc::format!("/dev/{}", base_name),
            partno: None,
            partition_type: None,
            fs_type: raw_fs,
        }];
    }

    match probe_mbr(raw) {
        MbrProbe::Partitions(partitions) => {
            let mut candidates = alloc::vec::Vec::new();
            for partition in partitions {
                let part_device = Arc::new(PartitionBlockDevice::new(
                    raw.clone(),
                    partition.start_lba,
                    partition.sectors,
                )) as Arc<dyn BlockDevice>;
                let name = alloc::format!("{}{}", base_name, partition.partno);
                let minor = raw_minor
                    .saturating_mul(16)
                    .saturating_add(partition.partno as u64);
                register_block_node(&name, part_device.clone(), minor, read_only);
                if let Some(alias_base) = partition_alias_base {
                    let alias = alloc::format!("{}{}", alias_base, partition.partno);
                    register_block_node(
                        &alias,
                        part_device.clone(),
                        100 + raw_minor.saturating_mul(16) + partition.partno as u64,
                        read_only,
                    );
                }

                let fs_type = self::filesystem::detect_fs(&part_device);
                boot_trace!(
                    "[mbr] /dev/{} type={:#04x} start_lba={} sectors={} size={}MiB fs={:?}",
                    name,
                    partition.type_code,
                    partition.start_lba,
                    partition.sectors,
                    partition.size_bytes() / (1024 * 1024),
                    fs_type
                );
                if fs_type != self::filesystem::FS_Type::Null {
                    candidates.push(MountCandidate {
                        device: part_device,
                        source: alloc::format!("/dev/{}", name),
                        partno: Some(partition.partno),
                        partition_type: Some(partition.type_code),
                        fs_type,
                    });
                }
            }
            candidates
        }
        MbrProbe::NoMbr => {
            println!(
                "[block] /dev/{} has neither a supported filesystem nor an MBR",
                base_name
            );
            alloc::vec::Vec::new()
        }
        MbrProbe::Unsupported => {
            println!(
                "[block] /dev/{} has an unsupported partition table",
                base_name
            );
            alloc::vec::Vec::new()
        }
    }
}

fn mount_boot_block_devices_with_flags(mount_flags: self::vfs::MountFlags, writable_scratch: bool) {
    let root = vfs_root();
    let devices = crate::drivers::block::block_devices();
    let read_only = mount_flags.contains(self::vfs::MountFlags::RDONLY);
    let mut same_disk_tools = None;
    let mut same_disk_scratch = None;

    match devices[0].as_ref() {
        Some(raw) => {
            #[cfg(feature = "board_2k1000")]
            let candidates =
                discover_mount_devices(raw, "sda", 0, Some(("vda", 100)), Some("vda"), read_only);
            #[cfg(not(feature = "board_2k1000"))]
            let candidates = discover_mount_devices(raw, "vda", 0, None, None, read_only);

            // 实板只有一块 SATA SSD。P2 必须保留给官方测试固定使用的
            // `/dev/vda2` FAT32 暂存盘，因此完整镜像把工具集放在 P3。
            // QEMU 的第二块独立工具盘仍有更高优先级。
            same_disk_tools = candidates
                .iter()
                .find(|candidate| candidate.partno == Some(3))
                .cloned();
            if writable_scratch {
                same_disk_scratch = candidates
                    .iter()
                    .find(|candidate| {
                        candidate.partno == Some(2)
                            && candidate.partition_type == Some(0x0c)
                            && candidate.fs_type == self::filesystem::FS_Type::Fat32
                    })
                    .cloned();
            }

            match candidates.first() {
                Some(candidate) => {
                    if mount_block_fs_with_flags(
                        &root,
                        &candidate.device,
                        "sdcard",
                        &alloc::format!("official fs ({})", candidate.source),
                        mount_flags,
                    )
                    .is_none()
                    {
                        println!("[initramfs] official fs mount failed, leaving /sdcard empty");
                    }
                }
                None => println!("[initramfs] no mountable official fs, leaving /sdcard empty"),
            }
        }
        None => println!("[initramfs] official fs (x0) not found, skipping /sdcard mount"),
    }

    if writable_scratch {
        match same_disk_scratch {
            Some(candidate) => {
                if mount_block_fs_with_flags(
                    &root,
                    &candidate.device,
                    "scratch",
                    &alloc::format!("writable scratch ({})", candidate.source),
                    self::vfs::MountFlags::empty(),
                )
                .is_none()
                {
                    panic!("2K1000 writable P2 scratch mount failed");
                }
            }
            None => panic!("2K1000 writable P2 FAT32 scratch partition not found"),
        }
    }

    let separate_tools = devices[1].as_ref().and_then(|raw| {
        discover_mount_devices(raw, "vdb", 1, None, Some("vda"), read_only)
            .into_iter()
            .next()
    });
    let use_same_disk_tools = separate_tools.is_none() && same_disk_tools.is_some();

    if use_same_disk_tools {
        if let Some(candidate) = same_disk_tools.as_ref() {
            boot_trace!(
                "[initramfs] tools disk (x1) not found; using {} from x0",
                candidate.source
            );
        }
    }
    let tools_candidate = separate_tools.or(same_disk_tools);
    match tools_candidate {
        Some(candidate) => {
            if mount_block_fs_with_flags(
                &root,
                &candidate.device,
                "tools",
                &alloc::format!("tools fs ({})", candidate.source),
                mount_flags,
            )
            .is_none()
            {
                println!("[initramfs] tools fs mount failed, leaving /tools empty");
            }
        }
        None => println!("[initramfs] no mountable tools fs, leaving /tools empty"),
    }
}

/// 探测启动块设备并以读写方式挂载识别出的文件系统。
pub fn mount_boot_block_devices() {
    mount_boot_block_devices_with_flags(self::vfs::MountFlags::empty(), false);
}

/// 实板首次文件系统验收路径：注册设备，但只读挂载文件系统。
pub fn mount_boot_block_devices_read_only() {
    mount_boot_block_devices_with_flags(self::vfs::MountFlags::RDONLY, false);
}

/// Staged 2K1000 migration policy: keep P1/P3 and all block device nodes
/// read-only, while mounting the validated P2 FAT32 partition at `/scratch`.
#[cfg(all(feature = "board_2k1000", feature = "sata_scratch_rw"))]
pub fn mount_boot_block_devices_with_writable_scratch() {
    mount_boot_block_devices_with_flags(self::vfs::MountFlags::RDONLY, true);
}

#[cfg(all(feature = "board_2k1000", feature = "sata_fs_write_probe"))]
fn board_scratch_fat32_root() -> Result<Arc<dyn self::vfs::IndexNode>, &'static str> {
    use self::vfs::FileSystem;
    use crate::drivers::block::partition::{probe_mbr, MbrProbe, PartitionBlockDevice};

    let raw = crate::drivers::block::block_devices()[0]
        .as_ref()
        .cloned()
        .ok_or("SATA device 0 is absent")?;
    let partitions = match probe_mbr(&raw) {
        MbrProbe::Partitions(partitions) => partitions,
        _ => return Err("expected MBR is absent or unsupported"),
    };
    let partition = partitions
        .into_iter()
        .find(|partition| partition.partno == 2 && partition.type_code == 0x0c)
        .ok_or("P2 is not the expected FAT32 LBA partition")?;
    let partition_device = Arc::new(PartitionBlockDevice::new(
        raw,
        partition.start_lba,
        partition.sectors,
    )) as Arc<dyn BlockDevice>;
    let detected = self::filesystem::detect_fs_layout(&partition_device)
        .ok_or("P2 filesystem was not detected")?;
    if detected.fs_type != self::filesystem::FS_Type::Fat32 {
        return Err("P2 is not FAT32");
    }
    let fs_device = adapt_filesystem_device(partition_device, detected.block_size, false);
    let filesystem = self::fat32::EasyFileSystem::open(fs_device);
    Ok(filesystem.root_inode())
}

/// Exercise FAT32 metadata and data persistence on the dedicated P2 scratch
/// partition without exposing a writable device node to userspace.
#[cfg(all(feature = "board_2k1000", feature = "sata_fs_write_probe"))]
pub fn run_board_scratch_write_probe() {
    use self::vfs::{FilePrivateData, FileType, InodeMode};

    macro_rules! probe_fatal {
        ($fmt:literal $(, $arg:expr)* $(,)?) => {
            panic!(concat!("[sata-fs-write-probe] FATAL: ", $fmt), $($arg),*)
        };
    }

    const DIR_NAME: &str = "MANGO_RW_PROBE";
    const FILE_NAME: &str = "PAYLOAD.BIN";
    const PAYLOAD_LEN: usize = 6144;

    println!("[sata-fs-write-probe] begin (P2 FAT32 only)");
    let root = match board_scratch_fat32_root() {
        Ok(root) => root,
        Err(error) => probe_fatal!("REFUSED: {}", error),
    };
    if root.find(DIR_NAME).is_ok() {
        probe_fatal!("REFUSED: stale probe directory exists");
    }
    let directory = match root.mkdir(DIR_NAME, InodeMode::from_bits_truncate(0o755)) {
        Ok(directory) => directory,
        Err(error) => probe_fatal!("mkdir failed: {:?}", error),
    };
    let file = match directory.create(
        FILE_NAME,
        FileType::File,
        InodeMode::from_bits_truncate(0o644),
    ) {
        Ok(file) => file,
        Err(error) => probe_fatal!("create failed: {:?}", error),
    };
    let mut expected = alloc::vec![0u8; PAYLOAD_LEN];
    for (index, byte) in expected.iter_mut().enumerate() {
        *byte = 0x6d ^ (index as u8).wrapping_mul(0x3d) ^ ((index >> 8) as u8);
    }
    let private = spin::Mutex::new(FilePrivateData::Unused);
    match file.write_at(0, expected.len(), &expected, private.lock()) {
        Ok(written) if written == expected.len() => {}
        Ok(written) => probe_fatal!("short write: {} != {}", written, expected.len()),
        Err(error) => probe_fatal!("write failed: {:?}", error),
    }
    crate::fs::page_cache::flush_all_page_caches();
    drop(file);
    drop(directory);
    drop(root);

    let reopened_root = match board_scratch_fat32_root() {
        Ok(root) => root,
        Err(error) => probe_fatal!("reopen failed: {}", error),
    };
    let reopened_dir = match reopened_root.find(DIR_NAME) {
        Ok(directory) => directory,
        Err(error) => probe_fatal!("persisted directory missing: {:?}", error),
    };
    let reopened_file = match reopened_dir.find(FILE_NAME) {
        Ok(file) => file,
        Err(error) => probe_fatal!("persisted file missing: {:?}", error),
    };
    let mut actual = alloc::vec![0u8; PAYLOAD_LEN];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    match reopened_file.read_at(0, actual.len(), &mut actual, private.lock()) {
        Ok(read) if read == actual.len() && actual == expected => {}
        Ok(read) => probe_fatal!("persisted data mismatch: read={}", read),
        Err(error) => probe_fatal!("persisted read failed: {:?}", error),
    }
    drop(reopened_file);
    if let Err(error) = reopened_dir.unlink(FILE_NAME) {
        probe_fatal!("unlink failed: {:?}", error);
    }
    drop(reopened_dir);
    if let Err(error) = reopened_root.rmdir(DIR_NAME) {
        probe_fatal!("rmdir failed: {:?}", error);
    }
    crate::fs::page_cache::flush_all_page_caches();
    drop(reopened_root);

    let final_root = match board_scratch_fat32_root() {
        Ok(root) => root,
        Err(error) => probe_fatal!("final reopen failed: {}", error),
    };
    if final_root.find(DIR_NAME).is_ok() {
        probe_fatal!("cleanup was not persistent");
    }
    println!("[sata-fs-write-probe] PASS: create/write/flush/reopen/read/unlink/rmdir persisted");
}

/// 主动初始化 initramfs VFS_ROOT（触发 lazy_static）。
/// 在 `mm::init()` 之后、`drivers::init_net_device()` 之前调用。
#[cfg(feature = "initramfs")]
pub fn initramfs_init() {
    let _root = VFS_ROOT.clone();
    boot_trace!("[initramfs] VFS_ROOT initialized (ramfs + cpio unpack + dev/proc/tmp)");
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

/// Return fully copied embedded payload pages to the physical-frame allocator.
///
/// The end symbol may point into a partial final page. Matching the historical
/// behavior, that page remains part of the kernel image and only the complete
/// pages in `[start, end)` are released.
///
/// # Safety
///
/// `start` and `end` must delimit one linker-owned payload. The caller must
/// have completed every read through those symbols before calling this helper.
unsafe fn reclaim_preload_payload_pages(start: usize, end: usize) {
    assert!(start <= end, "preload payload symbols are reversed");
    assert_eq!(
        start % crate::config::PAGE_SIZE,
        0,
        "preload payload start is not page-aligned"
    );
    let start_ppn = crate::mm::PhysAddr::from(start).floor();
    let end_ppn = crate::mm::PhysAddr::from(end).floor();
    // Safety: upheld by this helper's caller; the partial trailing page is not
    // included because `end` is rounded down.
    unsafe { crate::mm::frame_reclaim_linker_range(start_ppn, end_ppn) }
        .unwrap_or_else(|reason| panic!("failed to reclaim preload payload pages: {}", reason));
}

#[allow(unused)]
pub fn flush_preload() {
    macro_rules! preload_trace {
        ($($arg:tt)*) => {
            #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
            println!("[bringup][preload] {}", format_args!($($arg)*));
        };
    }

    // Safety (linker-symbol `from_raw_parts`): every `s<name>` / `e<name>`
    // pair below is a linker-defined symbol pair placed by the linker script.
    // The address range `[s<name>, e<name>)` contains the raw bytes of an
    // embedded ELF payload, fully initialised at link time.  `from_raw_parts`
    // creates a `&[u8]` over this range; the slice is used immediately inside
    // the `f.write()` call and never retained.  The physical frames are
    // explicitly registered with the frame allocator after the final write.
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
    boot_trace!(
        "sinitproc: {:X}, einitproc: {:X}, sbash: {:X}, ebash: {:X}, sbusybox: {:X}, ebusybox: {:X}, sosconfig: {:X}, eosconfig: {:X}",
        sinitproc as usize, einitproc as usize, sbash as usize, ebash as usize, sbusybox as usize, ebusybox as usize,
        sosconfig as usize, eosconfig as usize,
    );
    preload_trace!("01 create /initproc");
    let initproc = create_or_open_file("initproc").unwrap();
    let initproc_len = einitproc as usize - sinitproc as usize;
    preload_trace!("02 /initproc opened, writing {} bytes", initproc_len);
    let written = initproc
        .write(
            // Safety: see block comment above — linker-symbol range validity.
            unsafe { core::slice::from_raw_parts(sinitproc as *const u8, initproc_len) },
        )
        .unwrap();
    preload_trace!("03 /initproc write complete: {} bytes", written);
    log::debug!(
        "[kernel] flush_preload: initproc write len={} => written={} size_after={}",
        initproc_len,
        written,
        file_size("initproc").unwrap_or(0)
    );
    // Safety: `/initproc` now owns a copy and this is the final linker-symbol read.
    unsafe { reclaim_preload_payload_pages(sinitproc as usize, einitproc as usize) };
    preload_trace!("04 /initproc embedded frames released");
    // bash/busybox/os_test.conf/fs_test: 失败不阻塞启动
    preload_trace!("05 install /bash");
    let _ = create_or_open_file("bash").map(|f| {
        let _ = f.write(unsafe {
            core::slice::from_raw_parts(sbash as *const u8, ebash as usize - sbash as usize)
        });
    });
    // Safety: `/bash` now owns a copy and this is the final linker-symbol read.
    unsafe { reclaim_preload_payload_pages(sbash as usize, ebash as usize) };
    preload_trace!("06 /bash installed and embedded frames released");
    preload_trace!("07 install /busybox and /bin/busybox");
    let _ = create_or_open_file("busybox").map(|f| {
        let _ = f.write(unsafe {
            core::slice::from_raw_parts(
                sbusybox as *const u8,
                ebusybox as usize - sbusybox as usize,
            )
        });
    });
    // /bin/busybox must be written before the embedded pages are reclaimed.
    {
        let _ = vfs_lookup_parent("/bin/busybox")
            .or_else(|_| {
                let root = vfs_root().mountpoint_root_inode();
                let _ = root.create(
                    "bin",
                    self::vfs::FileType::Dir,
                    self::vfs::InodeMode::S_IRWXUGO,
                );
                vfs_lookup_parent("/bin/busybox")
            })
            .map(|(parent, _)| {
                if parent.find("busybox").is_err() {
                    let _ = parent.create(
                        "busybox",
                        self::vfs::FileType::File,
                        self::vfs::InodeMode::S_IRWXUGO,
                    );
                }
            });
        let _ = create_or_open_file("/bin/busybox").map(|f| {
            let _ = f.write(unsafe {
                core::slice::from_raw_parts(
                    sbusybox as *const u8,
                    ebusybox as usize - sbusybox as usize,
                )
            });
        });
    }
    // Safety: both busybox copies are complete and this is the final symbol read.
    unsafe { reclaim_preload_payload_pages(sbusybox as usize, ebusybox as usize) };
    preload_trace!("08 busybox copies installed and embedded frames released");
    preload_trace!("09 install /os_test.conf");
    match file_size("os_test.conf") {
        Ok(size) => {
            log::info!(
                "[kernel] flush_preload: keep existing /os_test.conf size={}",
                size
            );
        }
        Err(_) => {
            let _ = create_or_open_file("os_test.conf").map(|f| {
                let _ = f.write(unsafe {
                    core::slice::from_raw_parts(
                        sosconfig as *const u8,
                        eosconfig as usize - sosconfig as usize,
                    )
                });
            });
        }
    }
    // Safety: the runtime config is already present or copied; no later symbol read exists.
    unsafe { reclaim_preload_payload_pages(sosconfig as usize, eosconfig as usize) };
    preload_trace!("10 /os_test.conf ready and embedded frames released");
    #[cfg(feature = "board_core_test")]
    {
        // Runtime marker consumed by initproc. Keeping the focus selection out
        // of the SSD config lets a diagnostic kernel boot without rewriting P1.
        let _ = create_or_open_file("board_core_test");
    }
    #[cfg(feature = "cpython_test")]
    {
        // Select the isolated CPython group without changing the persistent
        // os_test.conf carried by the official test image.
        let _ = create_or_open_file("cpython_test");
    }
    #[cfg(feature = "apk_test")]
    {
        // Run the package-manager gate entirely from the writable initramfs;
        // the marker never changes the SSD-backed test configuration.
        let _ = create_or_open_file("apk_test");
    }
    #[cfg(feature = "board_shell")]
    {
        // Keep the shell selection ephemeral so booting this image never
        // rewrites the test configuration stored on the SSD.
        let _ = create_or_open_file("board_shell");
    }
    {
        preload_trace!("11 install /fs_test");
        let _ = create_or_open_file("fs_test").map(|f| {
            let _ = f.write(unsafe {
                core::slice::from_raw_parts(
                    sfstest as *const u8,
                    efstest as usize - sfstest as usize,
                )
            });
        });
        // Safety: `/fs_test` now owns a copy and this is the final symbol read.
        unsafe { reclaim_preload_payload_pages(sfstest as usize, efstest as usize) };
        preload_trace!("12 /fs_test installed and embedded frames released");
    }
    {
        preload_trace!("13 install /ltp_proto_compat.so");
        let _ = vfs_lookup_absolute("ltp_proto_compat.so")
            .and_then(|inode| inode.resize(0).map_err(|e| e as isize));
        let _ = create_or_open_file("ltp_proto_compat.so").map(|f| {
            let _ = f.write(unsafe {
                core::slice::from_raw_parts(
                    sltpcompat as *const u8,
                    eltpcompat as usize - sltpcompat as usize,
                )
            });
        });
        // Safety: the compatibility library was copied and is no longer read in place.
        unsafe { reclaim_preload_payload_pages(sltpcompat as usize, eltpcompat as usize) };
        preload_trace!("14 /ltp_proto_compat.so installed and embedded frames released");
    }
    {
        preload_trace!("15 install /ltprunner");
        let _ = vfs_lookup_absolute("ltprunner")
            .and_then(|inode| inode.resize(0).map_err(|e| e as isize));
        let _ = create_or_open_file("ltprunner").map(|f| {
            let _ = f.write(unsafe {
                core::slice::from_raw_parts(
                    sltprunner as *const u8,
                    eltprunner as usize - sltprunner as usize,
                )
            });
        });
        // Safety: `/ltprunner` now owns a copy and this is the final symbol read.
        unsafe { reclaim_preload_payload_pages(sltprunner as usize, eltprunner as usize) };
        preload_trace!("16 /ltprunner installed and embedded frames released");
    }
    preload_trace!("17 install compatibility files under /etc");
    ensure_ltp_compat_etc_files();
    preload_trace!("18 all preload payloads installed");
}
