//! VFS (Virtual File System) 抽象层
//!
//! 参照 DragonOS 的 VFS 设计，定义核心抽象：
//! - `IndexNode` trait：inode 级别操作（read_at/write_at/find/create/link/unlink/...）
//! - `File` 结构体：文件描述符级别（offset/flags/mode/read/write/lseek）
//! - `FileSystem` trait：具体文件系统接口（root_inode/info/name）
//! - `MountFS` / `MountFSInode`：挂载层（跨 FS 路径解析）
//!
//! 架构分层：
//! ```text
//! syscall 层
//!     ↓
//! File (文件描述符层：offset、flags、mode、read/write/lseek)
//!     ↓
//! IndexNode trait (inode 操作：read_at/write_at/find/create/link/unlink/...)
//!     ↓
//! MountFS/MountFSInode (挂载层：跨FS路径解析、挂载点管理)
//!     ↓
//! FileSystem trait (具体FS：root_inode/info/name/super_block)
//!     ↓
//! PageCache (页缓存：状态机、脏页追踪、回写)
//!     ↓
//! BlockDevice (块设备)
//! ```

pub mod dentry_cache;
pub mod event;
pub mod fasync;
pub mod fcntl;
pub mod file;
pub mod file_system;
pub mod index_node;
pub mod mount;
pub mod posix_lock;
pub mod propagation;

use crate::utils::error::SyscallErr;
use alloc::{string::String, sync::Arc, vec::Vec};
use core::any::Any;
use core::fmt::Debug;

pub use fasync::{set_file_fasync, FAsyncItem, FAsyncItems};
pub use fcntl::{
    FOwnerEx, FcntlCommand, PosixFlock, FD_CLOEXEC, F_OWNER_PGRP, F_OWNER_PID, F_OWNER_TID,
    F_RDLCK, F_SEAL_FUTURE_WRITE, F_SEAL_GROW, F_SEAL_SEAL, F_SEAL_SHRINK, F_SEAL_WRITE, F_UNLCK,
    F_WRLCK,
};
pub use file::{
    EventQueueHandle, FdTable, File, FileFlags, FileMode, FileOwner, FileOwnerSnapshot,
    FileOwnerTarget, PollWaitQueue, SeekFrom, STATUS_MASK,
};
pub use file_system::{FileSystem, FsInfo, SuperBlock};
pub use index_node::{CreateAttrs, IndexNode};
pub use mount::{
    canonicalize_state, drain_one_dying_lifecycle, normalize_request, BackendLifecycle, MountFS,
    MountFSInode, MountFlags, MountList, MountPath,
};
pub use propagation::{
    configure_propagation_no_register, get_slaves, install_propagation, propagate_mount,
    propagate_umount, register_current_propagation, register_peer, register_slave,
    set_propagation_type, unregister_peer_mount, unregister_slave_mount, MountPropagation,
    PropagationType,
};

use crate::drivers::block::BlockDevice;
use crate::timer::TimeSpec;

/// VFS 允许的最大路径长度
pub const MAX_PATHLEN: usize = 4096;

/// 单个文件名的最大长度
pub const NAME_MAX: usize = 255;

// renameat2 flags
pub const RENAME_NOREPLACE: u32 = 1;

// ── FileType ────────────────────────────────────────────────────────────

/// 文件类型枚举，对标 Linux 的 `S_IFMT` 字段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Dir,
    SymLink,
    CharDevice,
    BlockDevice,
    Socket,
    Pipe,
    /// Framebuffer 设备（归为 CharDevice）
    FramebufferDevice,
    /// KVM 设备（归为 CharDevice）
    KvmDevice,
}

impl FileType {
    /// 从 `InodeMode` 提取文件类型
    pub fn from_mode(mode: InodeMode) -> Self {
        match mode & InodeMode::S_IFMT {
            m if m == InodeMode::S_IFREG => FileType::File,
            m if m == InodeMode::S_IFDIR => FileType::Dir,
            m if m == InodeMode::S_IFLNK => FileType::SymLink,
            m if m == InodeMode::S_IFCHR => FileType::CharDevice,
            m if m == InodeMode::S_IFBLK => FileType::BlockDevice,
            m if m == InodeMode::S_IFSOCK => FileType::Socket,
            m if m == InodeMode::S_IFIFO => FileType::Pipe,
            _ => FileType::File, // fallback
        }
    }
}

// ── InodeMode ───────────────────────────────────────────────────────────

bitflags! {
    /// Inode 模式标志，对标 Linux stat.h 中的定义
    pub struct InodeMode: u32 {
        // 文件类型掩码
        const S_IFMT   = 0o170000;
        const S_IFSOCK = 0o140000;
        const S_IFLNK  = 0o120000;
        const S_IFREG  = 0o100000;
        const S_IFBLK  = 0o060000;
        const S_IFDIR  = 0o040000;
        const S_IFCHR  = 0o020000;
        const S_IFIFO  = 0o010000;

        // 权限位
        const S_ISUID  = 0o4000;
        const S_ISGID  = 0o2000;
        const S_ISVTX  = 0o1000;

        const S_IRUSR  = 0o0400;
        const S_IWUSR  = 0o0200;
        const S_IXUSR  = 0o0100;
        const S_IRGRP  = 0o0040;
        const S_IWGRP  = 0o0020;
        const S_IXGRP  = 0o0010;
        const S_IROTH  = 0o0004;
        const S_IWOTH  = 0o0002;
        const S_IXOTH  = 0o0001;

        // 组合
        const S_IRWXU  = 0o0700;
        const S_IRWXG  = 0o0070;
        const S_IRWXO  = 0o0007;
        const S_IRWXUGO = 0o0777;
        const S_IALLUGO = 0o7777;
    }
}

impl From<FileType> for InodeMode {
    fn from(val: FileType) -> Self {
        match val {
            FileType::File => InodeMode::S_IFREG,
            FileType::Dir => InodeMode::S_IFDIR,
            FileType::BlockDevice => InodeMode::S_IFBLK,
            FileType::CharDevice => InodeMode::S_IFCHR,
            FileType::SymLink => InodeMode::S_IFLNK,
            FileType::Socket => InodeMode::S_IFSOCK,
            FileType::Pipe => InodeMode::S_IFIFO,
            FileType::FramebufferDevice => InodeMode::S_IFCHR,
            FileType::KvmDevice => InodeMode::S_IFCHR,
        }
    }
}

// ── InodeFlags ──────────────────────────────────────────────────────────

bitflags! {
    /// Inode 标志位
    pub struct InodeFlags: u32 {
        /// 文件以追加模式打开
        const S_APPEND = 0x0100;
        /// 不可变文件
        const S_IMMUTABLE = 0x0200;
    }
}

// ── Metadata ────────────────────────────────────────────────────────────

/// 文件元数据，对标 Linux 的 `struct kstat`
#[derive(Debug, Clone)]
pub struct Metadata {
    /// 所在设备号
    pub dev_id: usize,
    /// inode 号
    pub inode_id: InodeId,
    /// 文件大小（字节）
    pub size: i64,
    /// 块大小
    pub blk_size: usize,
    /// 占用的块数量
    pub blocks: usize,
    /// 最后访问时间
    pub atime: TimeSpec,
    /// 最后修改时间
    pub mtime: TimeSpec,
    /// 最后状态改变时间
    pub ctime: TimeSpec,
    /// 文件类型
    pub file_type: FileType,
    /// 文件模式
    pub mode: InodeMode,
    /// inode 标志
    pub flags: InodeFlags,
    /// 硬链接数
    pub nlinks: u64,
    /// 所属用户 ID
    pub uid: u32,
    /// 所属组 ID
    pub gid: u32,
    /// 原始设备号（对于设备文件）
    pub raw_dev: u64,
}

impl Metadata {
    pub fn new(file_type: FileType, mode: InodeMode) -> Self {
        Metadata {
            dev_id: 0,
            inode_id: generate_inode_id(),
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type,
            mode,
            flags: InodeFlags::empty(),
            nlinks: 1,
            uid: 0,
            gid: 0,
            raw_dev: 0,
        }
    }
}

// ── InodeId ─────────────────────────────────────────────────────────────

/// Inode ID 类型
pub type InodeId = usize;

use core::sync::atomic::{AtomicUsize, Ordering};

static NEXT_INODE_ID: AtomicUsize = AtomicUsize::new(1);

/// 分配一个新的 inode ID
pub fn generate_inode_id() -> InodeId {
    NEXT_INODE_ID.fetch_add(1, Ordering::Relaxed)
}

// ── FilePrivateData ─────────────────────────────────────────────────────

/// 文件私有数据枚举
/// 对标 DragonOS 的 `FilePrivateData`，用于文件系统特定的私有数据
#[derive(Debug)]
pub enum FilePrivateData {
    /// 未使用 / 默认
    Unused,
    /// memfd sealing state shared by duplicated file descriptions.
    Memfd {
        seals: Arc<core::sync::atomic::AtomicUsize>,
    },
    /// 管道文件私有信息
    #[allow(dead_code)]
    Pipe,
    /// Socket 创建时使用
    #[allow(dead_code)]
    SocketCreate,
    /// PTY master — internal state shared with slave
    PtyMaster {
        inner: alloc::sync::Arc<crate::fs::dev::pty::PtyInner>,
    },
    /// Per-open procfs text snapshot.
    ProcText { content: Arc<String> },
    /// Sequential read-ahead state (per-open-file description).
    /// Used by PageCache to detect sequential reads and batch-prefetch pages.
    Readahead {
        ra_state: alloc::sync::Arc<spin::Mutex<crate::fs::page_cache::RaState>>,
    },
}

impl Clone for FilePrivateData {
    fn clone(&self) -> Self {
        match self {
            Self::Unused => Self::Unused,
            Self::Memfd { seals } => Self::Memfd {
                seals: seals.clone(),
            },
            Self::Pipe => Self::Pipe,
            Self::SocketCreate => Self::SocketCreate,
            Self::PtyMaster { inner } => Self::PtyMaster {
                inner: inner.clone(),
            },
            Self::ProcText { content } => Self::ProcText {
                content: content.clone(),
            },
            Self::Readahead { ra_state } => Self::Readahead {
                ra_state: ra_state.clone(),
            },
        }
    }
}

impl Default for FilePrivateData {
    fn default() -> Self {
        FilePrivateData::Unused
    }
}
