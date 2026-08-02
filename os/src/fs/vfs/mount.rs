//! MountFS — VFS 挂载层
//!
//! 对标 DragonOS `kernel/src/filesystem/vfs/mount.rs` 的 `MountFS` / `MountFSInode`。
//!
//! 设计思想：
//! - `MountFS` 包装一个 `Arc<dyn FileSystem>`，同时维护子挂载点表
//! - `MountFSInode` 包装一个 `Arc<dyn IndexNode>`，实现 `IndexNode` trait，
//!   所有操作委托给 `inner_inode`，在路径解析时跨越挂载点边界
//! - 全局 `MountList` 管理所有挂载关系（路径 → 挂载点映射）
//!
//! 路径解析流程示例（"/mnt/ext4/file"）：
//!   根 MountFSInode.find("mnt")
//!     → inner_inode.find("mnt") → 返回 mnt inode
//!     → 检查 mountpoints 表：mnt 是挂载点 → 返回 ext4 的根 MountFSInode
//!       → ext4根.find("file") → 返回目标 inode

use crate::utils::error::SyscallErr;
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::fmt::Debug;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::{Mutex, MutexGuard};

use super::dentry_cache::DentryCache;
use super::{
    file::FileFlags,
    file_system::FileSystem,
    propagation::{
        propagate_mount, propagate_umount, register_peer, unregister_peer_mount,
        unregister_slave_mount, MountPropagation, PropagationType,
    },
    FilePrivateData, FileType, IndexNode, InodeId, InodeMode,
};

// ── MountFlags ──────────────────────────────────────────────────────────

bitflags! {
    /// 挂载标志，对标 Linux mount.h
    pub struct MountFlags: u32 {
        /// 只读挂载
        const RDONLY = 0x1;
        /// 忽略 suid/sgid
        const NOSUID = 0x2;
        /// 禁止设备特殊文件
        const NODEV = 0x4;
        /// 禁止执行
        const NOEXEC = 0x8;
        /// 同步写入
        const SYNCHRONOUS = 0x10;
        /// 重新挂载
        const REMOUNT = 0x20;
        /// 允许强制锁
        const MANDLOCK = 0x40;
        /// 目录修改同步
        const DIRSYNC = 0x80;
        /// 不跟随符号链接
        const NOSYMFOLLOW = 0x100;
        /// 不更新访问时间
        const NOATIME = 0x400;
        /// 不更新目录访问时间
        const NODIRATIME = 0x800;
        /// bind mount
        const BIND = 0x1000;
        /// 重新递归 bind mount
        const REC = 0x4000;
        /// 相对 atime 更新 (mount.h: MS_RELATIME = 1<<21)
        const RELATIME = 0x200000;
        /// 显式 strict atime（清空所有 atime 策略位；不持久存储）
        const STRICTATIME = 0x1000000;
    }
}

impl MountFlags {
    /// 返回可以保存在挂载实例上的属性位。
    ///
    /// `REMOUNT`、`BIND` 和 `REC` 只描述本次 mount 操作，不能被 bind/传播副本
    /// 当作长期挂载属性保存；`RDONLY` 等属性位必须随副本继承。
    pub fn persistent(self) -> Self {
        let operation_bits = (Self::REMOUNT | Self::BIND | Self::REC).bits();
        Self::from_bits_truncate(self.bits() & !operation_bits)
    }
}

/// statfs f_flags ST_* constants (Linux <uapi/linux/statfs.h>).
///
/// Most MS_* → ST_* values are identical; only the exceptions are documented.
/// - `NOSYMFOLLOW`: MS_NOSYMFOLLOW = 0x100 → ST_NOSYMFOLLOW = 0x2000
pub const ST_RDONLY: u64 = 0x0001;
pub const ST_NOSUID: u64 = 0x0002;
pub const ST_NODEV: u64 = 0x0004;
pub const ST_NOEXEC: u64 = 0x0008;
pub const ST_SYNCHRONOUS: u64 = 0x0010;
pub const ST_MANDLOCK: u64 = 0x0040;
pub const ST_NOATIME: u64 = 0x0400;
pub const ST_NODIRATIME: u64 = 0x0800;
pub const ST_RELATIME: u64 = 0x1000;
pub const ST_NOSYMFOLLOW: u64 = 0x2000;

/// Convert VFS internal `MountFlags` (MS_* bit assignments) to `statfs`
/// `f_flags` (ST_* constants).
///
/// Most flags use the same numeric value; the notable exception is
/// `NOSYMFOLLOW` (0x100 → 0x2000).
pub fn mount_flags_to_st_flags(mf: MountFlags) -> u64 {
    let mut st = 0u64;
    if mf.contains(MountFlags::RDONLY) {
        st |= ST_RDONLY;
    }
    if mf.contains(MountFlags::NOSUID) {
        st |= ST_NOSUID;
    }
    if mf.contains(MountFlags::NODEV) {
        st |= ST_NODEV;
    }
    if mf.contains(MountFlags::NOEXEC) {
        st |= ST_NOEXEC;
    }
    if mf.contains(MountFlags::SYNCHRONOUS) {
        st |= ST_SYNCHRONOUS;
    }
    if mf.contains(MountFlags::MANDLOCK) {
        st |= ST_MANDLOCK;
    }
    if mf.contains(MountFlags::DIRSYNC) {
        st |= 0x0080;
    } // same bit
    if mf.contains(MountFlags::NOSYMFOLLOW) {
        st |= ST_NOSYMFOLLOW;
    }
    if mf.contains(MountFlags::NOATIME) {
        st |= ST_NOATIME;
    }
    if mf.contains(MountFlags::NODIRATIME) {
        st |= ST_NODIRATIME;
    }
    if mf.contains(MountFlags::RELATIME) {
        st |= ST_RELATIME;
    }
    st
}

/// Canonicalize already-persisted atime state.
///
/// Strips `STRICTATIME` (must never be stored), ensures `NOATIME` implies
/// `NODIRATIME`, and resolves any other contradictory combination.
///
/// Unlike [`normalize_request`], this function NEVER applies a default —
/// a previously normalized "strictatime" (empty) state is returned as-is.
///
/// **Idempotent** — calling on already-canonical state is a no-op.
pub fn canonicalize_state(flags: MountFlags) -> MountFlags {
    let has_strictatime = flags.contains(MountFlags::STRICTATIME);
    let has_noatime = flags.contains(MountFlags::NOATIME);
    let has_nodiratime = flags.contains(MountFlags::NODIRATIME);
    let has_relatime = flags.contains(MountFlags::RELATIME);

    let atime_mask = MountFlags::NOATIME
        | MountFlags::NODIRATIME
        | MountFlags::RELATIME
        | MountFlags::STRICTATIME;

    let mut f = flags & !atime_mask;

    if has_strictatime {
        // STRICTATIME must never be stored.  If NODIRATIME was also given,
        // keep it as an orthogonal constraint; discard all other atime bits.
        if has_nodiratime {
            f |= MountFlags::NODIRATIME;
        }
        return f;
    }

    if has_noatime {
        f |= MountFlags::NOATIME;
        f |= MountFlags::NODIRATIME;
        return f;
    }

    if has_relatime {
        f |= MountFlags::RELATIME;
        if has_nodiratime {
            f |= MountFlags::NODIRATIME;
        }
        return f;
    }

    // NODIRATIME alone: preserve without adding a default atime policy.
    if has_nodiratime {
        f |= MountFlags::NODIRATIME;
        return f;
    }

    // No atime flags → strictatime (empty).  Return as-is.
    f
}

/// Normalize a new atime-policy **request** from a user syscall.
///
/// Applies Linux defaults and conflict resolution:
///
/// | Input | Normalized | Rationale |
/// |-------|-----------|-----------|
/// | `STRICTATIME` present | clear all atime bits | not a stored flag |
/// | `NOATIME` present | `NOATIME \| NODIRATIME` | NOATIME implies NODIRATIME |
/// | `RELATIME` present | `RELATIME` (+ `NODIRATIME` if given) | explicit relatime |
/// | `NODIRATIME` alone | `RELATIME \| NODIRATIME` | default atime is RELATIME |
/// | No atime flags | `RELATIME` | Linux default since 2.6.30 |
///
/// **Not idempotent** — intended for raw user input only.  Use
/// [`canonicalize_state`] for already-persisted state.
pub fn normalize_request(flags: MountFlags) -> MountFlags {
    let has_strictatime = flags.contains(MountFlags::STRICTATIME);
    let has_noatime = flags.contains(MountFlags::NOATIME);
    let has_relatime = flags.contains(MountFlags::RELATIME);
    let has_nodiratime = flags.contains(MountFlags::NODIRATIME);

    let atime_mask = MountFlags::NOATIME
        | MountFlags::NODIRATIME
        | MountFlags::RELATIME
        | MountFlags::STRICTATIME;

    let mut f = flags & !atime_mask;

    if has_strictatime {
        if has_nodiratime {
            f |= MountFlags::NODIRATIME;
        }
        return f;
    }

    if has_noatime {
        f |= MountFlags::NOATIME;
        f |= MountFlags::NODIRATIME;
        return f;
    }

    if has_relatime {
        f |= MountFlags::RELATIME;
        if has_nodiratime {
            f |= MountFlags::NODIRATIME;
        }
        return f;
    }

    // No explicit atime-policy flag at all.
    if has_nodiratime {
        f |= MountFlags::RELATIME;
        f |= MountFlags::NODIRATIME;
        return f;
    }

    // No atime flags → Linux default: RELATIME
    f |= MountFlags::RELATIME;
    f
}

// ── MountPath ────────────────────────────────────────────────────────────

/// 挂载路径，用于全局挂载表
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MountPath(pub String);

impl From<&str> for MountPath {
    fn from(value: &str) -> Self {
        MountPath(String::from(value))
    }
}

impl From<String> for MountPath {
    fn from(value: String) -> Self {
        MountPath(value)
    }
}

impl AsRef<str> for MountPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Ord for MountPath {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for MountPath {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Runtime gate for mount lifecycle diagnostics.
/// When true, key mount events (create/bind/umount/detach/drop) are
/// logged with stable identity pointers to prove/counter-prove
/// shared-backend premature teardown.
/// Controlled via `/sys/kernel/stats/mount_diag_on` (writable).
pub static MOUNT_LIFECYCLE_DIAG: AtomicBool = AtomicBool::new(false);

// ── MountFSInode ────────────────────────────────────────────────────────

/// Debug: lifetime counters for MountFS / MountFSInode
pub mod counters {
    use core::sync::atomic::AtomicUsize;
    pub static MOUNTFS_ALIVE: AtomicUsize = AtomicUsize::new(0);
    pub static MOUNTFSINODE_ALIVE: AtomicUsize = AtomicUsize::new(0);

    pub fn mountfs_alive() -> usize {
        MOUNTFS_ALIVE.load(core::sync::atomic::Ordering::Relaxed)
    }
    pub fn mountfsinode_alive() -> usize {
        MOUNTFSINODE_ALIVE.load(core::sync::atomic::Ordering::Relaxed)
    }

    // VFS find diagnostic counters
    pub static FIND_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FIND_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FIND_SELF_OVERLAY: AtomicUsize = AtomicUsize::new(0);
    pub static FIND_DENTRY_HIT: AtomicUsize = AtomicUsize::new(0);
    pub static FIND_DENTRY_MISS: AtomicUsize = AtomicUsize::new(0);
    pub static FIND_LOCK_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FIND_INNER_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FIND_INSERT_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FIND_OVERLAY_TICKS: AtomicUsize = AtomicUsize::new(0);

    pub fn find_snapshot() -> (
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
    ) {
        let calls = FIND_CALLS.load(core::sync::atomic::Ordering::Relaxed);
        let ticks = FIND_TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let overlay = FIND_SELF_OVERLAY.load(core::sync::atomic::Ordering::Relaxed);
        let hit = FIND_DENTRY_HIT.load(core::sync::atomic::Ordering::Relaxed);
        let miss = FIND_DENTRY_MISS.load(core::sync::atomic::Ordering::Relaxed);
        let lock = FIND_LOCK_TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let inner = FIND_INNER_TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let insert = FIND_INSERT_TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let ov_ticks = FIND_OVERLAY_TICKS.load(core::sync::atomic::Ordering::Relaxed);
        (
            calls, ticks, overlay, hit, miss, lock, inner, insert, ov_ticks,
        )
    }

    // MountFSInode creation source counters
    pub static MFSI_FROM_FIND: AtomicUsize = AtomicUsize::new(0);
    pub static MFSI_FROM_OVERLAY: AtomicUsize = AtomicUsize::new(0);
    pub static MFSI_FROM_PARENT: AtomicUsize = AtomicUsize::new(0);
    pub static MFSI_FROM_ROOT: AtomicUsize = AtomicUsize::new(0);
    pub static MFSI_FROM_CREATE: AtomicUsize = AtomicUsize::new(0);
    pub static MFSI_FROM_BACKREF: AtomicUsize = AtomicUsize::new(0);

    pub fn creation_snapshot() -> (usize, usize, usize, usize, usize, usize) {
        (
            MFSI_FROM_FIND.load(core::sync::atomic::Ordering::Relaxed),
            MFSI_FROM_OVERLAY.load(core::sync::atomic::Ordering::Relaxed),
            MFSI_FROM_PARENT.load(core::sync::atomic::Ordering::Relaxed),
            MFSI_FROM_ROOT.load(core::sync::atomic::Ordering::Relaxed),
            MFSI_FROM_CREATE.load(core::sync::atomic::Ordering::Relaxed),
            MFSI_FROM_BACKREF.load(core::sync::atomic::Ordering::Relaxed),
        )
    }

    // Mount/bind performance counters
    pub static MOUNT_LIST_PROPAGATE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static MOUNT_LIST_PROPAGATE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static MOUNT_LIST_REMOVE_FS_SCAN: AtomicUsize = AtomicUsize::new(0);
    pub static MOUNT_LIST_REMOVE_FS_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static RBIND_SNAPSHOT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static RBIND_SNAPSHOT_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static RBIND_SNAPSHOT_ENTRIES: AtomicUsize = AtomicUsize::new(0);
    pub static RBIND_DIRENT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static RBIND_SEEN_SCAN: AtomicUsize = AtomicUsize::new(0);

    pub fn mount_perf_snapshot() -> (
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
    ) {
        (
            MOUNT_LIST_PROPAGATE_CALLS.load(core::sync::atomic::Ordering::Relaxed),
            MOUNT_LIST_PROPAGATE_CYCLES.load(core::sync::atomic::Ordering::Relaxed),
            MOUNT_LIST_REMOVE_FS_SCAN.load(core::sync::atomic::Ordering::Relaxed),
            MOUNT_LIST_REMOVE_FS_CALLS.load(core::sync::atomic::Ordering::Relaxed),
            RBIND_SNAPSHOT_CALLS.load(core::sync::atomic::Ordering::Relaxed),
            RBIND_SNAPSHOT_CYCLES.load(core::sync::atomic::Ordering::Relaxed),
            RBIND_SNAPSHOT_ENTRIES.load(core::sync::atomic::Ordering::Relaxed),
            RBIND_DIRENT_CALLS.load(core::sync::atomic::Ordering::Relaxed),
            RBIND_SEEN_SCAN.load(core::sync::atomic::Ordering::Relaxed),
        )
    }

    // Mount lifecycle observability counters.
    pub static MOUNT_LIFECYCLE_CREATE: AtomicUsize = AtomicUsize::new(0);
    pub static MOUNT_LIFECYCLE_UMOUNT: AtomicUsize = AtomicUsize::new(0);
    pub static MOUNT_LIFECYCLE_DETACH: AtomicUsize = AtomicUsize::new(0);
    pub static MOUNT_LIFECYCLE_DROP: AtomicUsize = AtomicUsize::new(0);

    pub fn lifecycle_snapshot() -> (usize, usize, usize, usize) {
        (
            MOUNT_LIFECYCLE_CREATE.load(core::sync::atomic::Ordering::Relaxed),
            MOUNT_LIFECYCLE_UMOUNT.load(core::sync::atomic::Ordering::Relaxed),
            MOUNT_LIFECYCLE_DETACH.load(core::sync::atomic::Ordering::Relaxed),
            MOUNT_LIFECYCLE_DROP.load(core::sync::atomic::Ordering::Relaxed),
        )
    }
}

/// Emit a mount lifecycle diagnostic event.
///
/// Logs stable identity information when `MOUNT_LIFECYCLE_DIAG` is enabled.
/// Uses pointer identity (`Arc::as_ptr`) — never path strings — for stable
/// object identification.  Backend identity is the `lifecycle` pointer
/// (`Arc::as_ptr`); shared backends show the same value.
///
/// **Gate**: `cfg!(feature = "perf_diag")` + runtime `MOUNT_LIFECYCLE_DIAG`.
/// This function is a no-op unless both conditions hold.
fn diag_mount_event(label: &str, mfs: &Arc<MountFS>) {
    // Double-gate: compile-time feature OFF → no codegen impact.
    #[cfg(feature = "perf_diag")]
    {
        if !MOUNT_LIFECYCLE_DIAG.load(core::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let self_ptr = Arc::as_ptr(mfs) as usize;
        let lc_count = mfs
            .lifecycle
            .packed
            .load(core::sync::atomic::Ordering::Relaxed)
            & ((1u64 << 30) - 1);
        let lc_state = mfs
            .lifecycle
            .packed
            .load(core::sync::atomic::Ordering::Relaxed)
            >> 62;
        let parent_info = mfs.self_mountpoint().map(|mp| {
            let parent_ptr = Arc::as_ptr(&mp.mount_fs) as usize;
            let ino = mp
                .inner_inode
                .metadata()
                .map(|m| alloc::format!("{:?}", m.inode_id))
                .unwrap_or_else(|_| alloc::string::String::from("?"));
            (parent_ptr, ino)
        });
        let flags = mfs.mount_flags();
        let prop = mfs.propagation();
        let path = mfs
            .mount_path()
            .unwrap_or_else(|| alloc::string::String::from("(nopath)"));

        log::warn!(
            "[mount_diag] {} mfs=0x{:x} lc_count={} lc_state={} path={} flags={:?} prop={:?} peer_gid={} master_gid={} parent_mfs={} parent_ino={}",
            label, self_ptr, lc_count, lc_state, path, flags,
            prop.prop_type(),
            prop.peer_group_id(), prop.master_group_id(),
            parent_info.as_ref().map(|(p,_)| alloc::format!("0x{:x}", p)).unwrap_or_else(|| alloc::string::String::from("none")),
            parent_info.as_ref().map(|(_,i)| i.as_str()).unwrap_or("none"),
        );
    }
    #[cfg(not(feature = "perf_diag"))]
    {
        let _ = (label, mfs);
    }
}

// ── Backend lifecycle ────────────────────────────────────────────────────

/// Lifecycle state machine for a shared filesystem backend.
///
/// Packed into an `AtomicU64`: `[state:2][refcount:30][_reserved:32]`.
///
/// * `Active`   — at least one `MountFS` holds a reference; `acquire()` succeeds.
/// * `Dying`    — last `MountFS` reference released; blocked for new acquisitions.
/// * `Dead`     — `on_umount()` has completed successfully; terminal.
///
/// Transition rules:
/// 1. `BackendLifecycle::new()`  → Active (count=0); NOT yet registered.
/// 2. `.acquire()`               → count+1; on first 0→1 transition, registered
///    into global list in an allocation-safe caller context.
/// 3. `.release()` (from Drop)   → count-1; CAS Active→Dying when count→0.
/// 4. `drain_one_dying()` (sched) → finds Dying entry, removes from registry,
///    and calls `on_umount()` outside ANY lock.  Success marks it Dead; failure
///    leaves it Dying and re-inserts it so a later scheduler tick can retry.
///
/// Count-0 lifecycles never enter the registry: if no MountFS ever acquires,
/// the lifecycle is dropped without leaking registry entries.
///
/// **No** `Arc::strong_count` is used for semantic decisions.
const LC_STATE_SHIFT: u64 = 62;
const LC_COUNT_MASK: u64 = (1u64 << 30) - 1;
const LC_STATE_ACTIVE: u64 = 0;
const LC_STATE_DYING: u64 = 1;
const LC_STATE_DEAD: u64 = 2;

/// Persistent linear registry of every **acquired** `BackendLifecycle`.
///
/// Entries are added lazily on the first `acquire()` (0→1), so count-0
/// lifecycles are never present.  Scanned by the scheduler to drain entries
/// that have reached Dying.  The registry holds one strong `Arc` per entry.
static LIFECYCLE_REGISTRY: Mutex<alloc::vec::Vec<Arc<BackendLifecycle>>> =
    Mutex::new(alloc::vec::Vec::new());

/// Lifecycle counter for diagnostic parity with the old `MOUNT_LIFECYCLE_*`
/// counters.  Incremented on `new`, `acquire`, `release→Dying`, and `drain`.
pub static LC_NEW: AtomicU64 = AtomicU64::new(0);
pub static LC_ACQUIRE: AtomicU64 = AtomicU64::new(0);
pub static LC_RELEASE_DYING: AtomicU64 = AtomicU64::new(0);
pub static LC_DRAIN: AtomicU64 = AtomicU64::new(0);
pub static LC_STALE_REGISTRY: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct BackendLifecycle {
    /// The concrete filesystem backend.
    fs: Arc<dyn FileSystem>,
    /// `[state:2][refcount:30][...]`
    packed: AtomicU64,
    /// True once this lifecycle has been pushed into LIFECYCLE_REGISTRY.
    /// Set atomically on the first 0→1 acquire transition so that count-0
    /// lifecycles (never acquired by any MountFS) are never registered.
    registered: AtomicBool,
}

impl BackendLifecycle {
    /// Create a new **Active** lifecycle with count=0.
    ///
    /// The lifecycle is NOT yet registered in the global list — registration
    /// is deferred until the first successful `0→1` `acquire()` call, which
    /// always happens in an allocation-safe MountFS constructor context.
    /// Count-0 lifecycles that are never acquired simply drop without leaking
    /// registry entries.
    pub fn new(fs: Arc<dyn FileSystem>) -> Arc<Self> {
        LC_NEW.fetch_add(1, Ordering::Relaxed);
        Arc::new(Self {
            fs,
            packed: AtomicU64::new(0), // Active(0), count=0
            registered: AtomicBool::new(false),
        })
    }

    /// Borrow the filesystem backend.
    #[inline]
    pub fn fs(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    /// Try to acquire a new MountFS reference.
    ///
    /// Returns `None` if the lifecycle is already Dying or Dead.
    /// On success the internal refcount is incremented atomically and
    /// the lifecycle `Arc` is cloned.
    ///
    /// On the first `0→1` transition, the lifecycle is registered in
    /// the global registry so the scheduler drain can find it later.
    /// This registration always happens in an allocation-safe MountFS
    /// constructor context.
    pub fn acquire(self: &Arc<Self>) -> Option<Arc<Self>> {
        loop {
            let old = self.packed.load(Ordering::Acquire);
            let state = old >> LC_STATE_SHIFT;
            if state != LC_STATE_ACTIVE {
                return None;
            }
            let count = old & LC_COUNT_MASK;
            // Overflow guard: 30 bits is ~1B refs.
            if count >= LC_COUNT_MASK {
                return None;
            }
            let new = old + 1; // state stays Active, count++
            match self
                .packed
                .compare_exchange_weak(old, new, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    LC_ACQUIRE.fetch_add(1, Ordering::Relaxed);
                    // Lazily register on first acquire (0→1).
                    // This guarantee: no count-0 lifecycle ever enters the registry.
                    if count == 0 && !self.registered.swap(true, Ordering::AcqRel) {
                        LIFECYCLE_REGISTRY.lock().push(self.clone());
                    }
                    return Some(self.clone());
                }
                Err(_) => continue,
            }
        }
    }

    /// Release a MountFS reference.  Called from `Drop for MountFS`.
    ///
    /// Returns `true` when the internal count reaches zero **and**
    /// the state transitions Active → Dying.
    pub fn release(&self) -> bool {
        loop {
            let old = self.packed.load(Ordering::Acquire);
            let state = old >> LC_STATE_SHIFT;
            let count = old & LC_COUNT_MASK;
            if count == 0 {
                // Underflow — already released too many times.
                return false;
            }
            if count == 1 {
                // Last reference: transition to Dying.
                let new = LC_STATE_DYING << LC_STATE_SHIFT;
                match self.packed.compare_exchange_weak(
                    old,
                    new,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        LC_RELEASE_DYING.fetch_add(1, Ordering::Relaxed);
                        return true;
                    }
                    Err(_) => continue,
                }
            } else {
                let new = (state << LC_STATE_SHIFT) | (count - 1);
                match self.packed.compare_exchange_weak(
                    old,
                    new,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return false,
                    Err(_) => continue,
                }
            }
        }
    }

    /// Query the lifecycle state (Active=0, Dying=1, Dead=2).
    #[inline]
    fn state(&self) -> u64 {
        self.packed.load(Ordering::Acquire) >> LC_STATE_SHIFT
    }

    #[inline]
    fn is_dying(&self) -> bool {
        self.state() == LC_STATE_DYING
    }
}

/// Drain **one** Dying lifecycle from the global registry.
///
/// Called from the scheduler loop (`run_tasks()`).
///
/// # Locking
///
/// The registry lock is held only long enough to remove the entry.
/// `on_umount()` is called **outside** any lock.  On success the entry is
/// marked Dead and dropped (which drops `Arc<dyn FileSystem>`).  On failure it
/// remains Dying and is re-inserted into the registry for a later retry; the
/// backend must stay alive because it may still be registered with lower-level
/// resources.
///
/// Returns `true` if work was done.
pub fn drain_one_dying_lifecycle() -> bool {
    let entry = {
        let mut reg = LIFECYCLE_REGISTRY.lock();
        if let Some(pos) = reg.iter().position(|lc| lc.is_dying()) {
            Some(reg.remove(pos))
        } else {
            None
        }
    };
    if let Some(lc) = entry {
        match lc.fs.on_umount() {
            Ok(()) => {
                // Dead is reserved for a backend whose teardown transaction
                // completed; only then may the lifecycle release its fs Arc.
                lc.packed
                    .store(LC_STATE_DEAD << LC_STATE_SHIFT, Ordering::Release);
                LC_DRAIN.fetch_add(1, Ordering::Relaxed);
                // `lc` drops here → Arc<BackendLifecycle> →
                // Arc<dyn FileSystem> released.
            }
            Err(error) => {
                log::error!(
                    "filesystem backend umount failed: {:?}; keeping lifecycle Dying for retry",
                    error
                );
                // The entry was removed before calling into the backend so no
                // registry lock was held across I/O.  Put the same strong Arc
                // back only after the callback returns; state remains Dying,
                // therefore acquire() stays blocked until teardown succeeds.
                LIFECYCLE_REGISTRY.lock().push(lc);
            }
        }
        true
    } else {
        false
    }
}

/// Commit and detach every registered filesystem backend before an orderly
/// machine shutdown.
///
/// PageCache writeback must run before this function.  The registry lock is
/// released before any backend I/O, and all backends are attempted even if
/// one fails so independent filesystems still get a durability boundary.
pub fn shutdown_all_backends() -> Result<(), SyscallErr> {
    let lifecycles: alloc::vec::Vec<_> = LIFECYCLE_REGISTRY
        .lock()
        .iter()
        .filter(|lifecycle| lifecycle.state() != LC_STATE_DEAD)
        .cloned()
        .collect();
    let mut first_error = None;

    for lifecycle in lifecycles {
        if let Err(error) = lifecycle.fs.on_umount() {
            log::error!(
                "filesystem backend shutdown failed: {:?}; continuing other backends",
                error
            );
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }

    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Return the number of stale (Dying + Dead) entries still in the registry.
/// Pure diagnostic; not used for any control-flow decision.
pub fn lc_stale_registry_count() -> u64 {
    let reg = LIFECYCLE_REGISTRY.lock();
    let mut n: u64 = 0;
    for lc in reg.iter() {
        if !lc.is_dying() && lc.state() != LC_STATE_DEAD {
            continue;
        }
        n += 1;
    }
    n
}

#[derive(Debug, Clone)]
struct PathHint {
    parent_ino: InodeId,
    name: String,
}

const PATH_HINT_LIMIT: usize = 512;

/// MountFSInode — 挂载感知的 inode 包装器
///
/// 包装内层 inode，所有 `IndexNode` 方法委托给 `inner_inode`。
/// 在 `find()` 中检查子挂载点表，实现跨文件系统路径解析。
#[derive(Debug)]
pub struct MountFSInode {
    /// 内层 inode
    pub inner_inode: Arc<dyn IndexNode>,
    /// 所属的 MountFS
    pub mount_fs: Arc<MountFS>,
    /// 指向自身的弱引用
    self_ref: Mutex<Weak<MountFSInode>>,
    /// Best-effort parent/name hint for physical cwd reconstruction.
    path_hint: Mutex<Option<PathHint>>,
}

impl MountFSInode {
    /// 创建新 MountFSInode
    pub fn new(inner_inode: Arc<dyn IndexNode>, mount_fs: Arc<MountFS>) -> Arc<Self> {
        counters::MOUNTFSINODE_ALIVE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        Arc::new_cyclic(|self_ref| MountFSInode {
            inner_inode,
            mount_fs,
            self_ref: Mutex::new(self_ref.clone()),
            path_hint: Mutex::new(None),
        })
    }

    /// 获取自身的强引用
    fn self_arc(&self) -> Arc<Self> {
        self.self_ref.lock().upgrade().unwrap()
    }

    /// 如果是 MountFSInode 包装，解包到内层 inode；否则原样返回
    pub fn unwrap_inode(inode: &Arc<dyn IndexNode>) -> Arc<dyn IndexNode> {
        if let Some(mnt) = inode.as_any_ref().downcast_ref::<MountFSInode>() {
            mnt.inner_inode.clone()
        } else {
            inode.clone()
        }
    }

    /// 检查挂载是否可写
    fn ensure_mount_writable(&self) -> Result<(), SyscallErr> {
        if self.mount_fs.mount_flags.load(Ordering::Acquire) & MountFlags::RDONLY.bits() != 0 {
            return Err(SyscallErr::EROFS);
        }
        Ok(())
    }

    fn remember_path_hint(&self, parent_ino: InodeId, name: &str) {
        if name == "." || name == ".." {
            return;
        }
        let hint = PathHint {
            parent_ino,
            name: String::from(name),
        };
        *self.path_hint.lock() = Some(hint.clone());
        if let Ok(md) = self.metadata() {
            self.mount_fs.remember_inode_path_hint(md.inode_id, hint);
        }
    }

    fn valid_path_hint_name(
        &self,
        parent: &Arc<MountFSInode>,
        parent_ino: InodeId,
        child_ino: InodeId,
    ) -> Option<String> {
        let hint = self
            .path_hint
            .lock()
            .clone()
            .or_else(|| self.mount_fs.lookup_inode_path_hint(child_ino))?;
        if hint.parent_ino != parent_ino {
            return None;
        }
        let child = parent.do_find(&hint.name).ok()?;
        let found_ino = child.metadata().map(|m| m.inode_id).ok();
        if found_ino == Some(child_ino) {
            Some(hint.name)
        } else {
            None
        }
    }

    /// 判断当前 inode 是否为挂载点根
    pub fn is_mountpoint_root(&self) -> bool {
        let Ok(cur_md) = self.inner_inode.metadata() else {
            return false;
        };
        let root_inner = self.mount_fs.root_inner_inode();
        let Ok(root_md) = root_inner.metadata() else {
            return false;
        };
        cur_md.inode_id == root_md.inode_id
    }

    /// 解析路径时，跨越挂载点边界
    ///
    /// 如果在当前 inode 的子挂载表中找到了匹配的 inode_id，
    /// 返回子文件系统的根 inode。限制穿透深度防止 mount tree 环路。
    fn overlaid_inode(self_inode: Arc<MountFSInode>) -> Arc<MountFSInode> {
        const MAX_OVERLAY: u32 = 32;
        let mut current = self_inode;
        for _ in 0..MAX_OVERLAY {
            let inode_id = match current.inner_inode.metadata() {
                Ok(md) => md.inode_id,
                Err(_) => return current,
            };
            let sub_mountfs = {
                let lock = current.mount_fs.mountpoints.lock();
                lock.get(&inode_id).cloned()
            };
            match sub_mountfs {
                Some(sub) => {
                    let root_inner = sub
                        .root_inner_inode
                        .clone()
                        .unwrap_or_else(|| sub.lifecycle.fs().root_inode());
                    let sub_arc = sub.self_ref.lock().upgrade().unwrap();
                    current = MountFSInode::new(root_inner, sub_arc);
                    counters::MFSI_FROM_OVERLAY.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                }
                None => return current,
            }
        }
        log::warn!(
            "overlaid_inode: max overlay depth {} reached, stopping",
            MAX_OVERLAY
        );
        current
    }

    /// 逐级查找子项（带挂载点交叉和 dentry 缓存）
    fn do_find(&self, name: &str) -> Result<Arc<MountFSInode>, SyscallErr> {
        counters::FIND_CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let parent_ino = self.inner_inode.metadata()?.inode_id;
        self.do_find_with_parent_ino(name, parent_ino)
    }

    /// Same as do_find, but caller supplies parent_ino to avoid a redundant
    /// inner_inode.metadata() call. Used by vfs_lookup which already has the
    /// metadata from its own directory-type check.
    pub(crate) fn do_find_with_parent_ino(
        &self,
        name: &str,
        parent_ino: usize,
    ) -> Result<Arc<MountFSInode>, SyscallErr> {
        // ".." must cross mount boundaries with correct semantics
        // (mount root → mountpoint's parent; global root → self).
        // This must run BEFORE self-overlay and dentry cache to avoid
        // escaping from a bind mount back to the source filesystem.
        if name == ".." {
            return self.lookup_dotdot();
        }
        // Self-overlay: if this inode itself is a mountpoint, redirect to
        // the mounted filesystem's root before looking up children.
        // Without this, find("test_file") on a covered mountpoint directory
        // would search the old (hidden) directory instead of the mounted FS.
        let self_arc = self.self_arc();
        let top = MountFSInode::overlaid_inode(self_arc.clone());
        if !Arc::ptr_eq(&top, &self_arc) {
            counters::FIND_SELF_OVERLAY.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            return top.do_find(name);
        }

        // Shortcut: skip dentry cache for dynamic filesystems (procfs)
        if self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            let inner_inode = self.inner_inode.find(name)?;
            let result =
                MountFSInode::overlaid_inode(MountFSInode::new(inner_inode, self.mount_fs.clone()));
            result.remember_path_hint(parent_ino, name);
            counters::MFSI_FROM_FIND.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            return Ok(result);
        }

        let key = super::dentry_cache::DentryKey {
            parent_ino,
            name: String::from(name),
        };

        // Check dentry cache — returns covered dentry
        if let Some(cached) = self.mount_fs.dentry_cache.lock().get(&key) {
            cached.remember_path_hint(parent_ino, name);
            return Ok(MountFSInode::overlaid_inode(cached));
        }

        // Cache miss: record generation before disk I/O
        if crate::fs::ext4::counters::counters_enabled() {
            crate::fs::ext4::counters::DENTRY_LOOKUP_COUNT
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            crate::fs::ext4::counters::DENTRY_CACHE_MISS
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        let gen_before = self
            .mount_fs
            .dentry_gen
            .load(core::sync::atomic::Ordering::Acquire);

        // Release cache lock, perform actual filesystem lookup
        let inner_inode = self.inner_inode.find(name)?;

        // Create covered dentry (before mount-point overlay)
        let covered = MountFSInode::new(inner_inode, self.mount_fs.clone());
        covered.remember_path_hint(parent_ino, name);
        counters::MFSI_FROM_FIND.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

        // Insert into cache — only if directory was not modified concurrently
        let gen_after = self
            .mount_fs
            .dentry_gen
            .load(core::sync::atomic::Ordering::Acquire);
        if gen_before == gen_after {
            let (entry, evicted) = {
                let mut cache = self.mount_fs.dentry_cache.lock();
                cache.insert_or_get(key, covered)
            };
            drop(evicted);
            Ok(MountFSInode::overlaid_inode(entry))
        } else {
            // Directory was modified (unlink/rename/etc.), don't cache stale dentry
            Ok(MountFSInode::overlaid_inode(covered))
        }
    }

    /// 查找父目录
    fn do_parent(&self) -> Result<Arc<MountFSInode>, SyscallErr> {
        if self.is_mountpoint_root() {
            if let Some(mountpoint) = self.mount_fs.self_mountpoint() {
                return mountpoint.do_parent();
            }
            return Ok(self.self_arc());
        }
        let parent_inner = self.inner_inode.find("..")?;
        let parent = MountFSInode::new(parent_inner, self.mount_fs.clone());
        counters::MFSI_FROM_PARENT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        Ok(MountFSInode::overlaid_inode(parent))
    }

    /// 挂载边界感知的 ".." 解析。
    ///
    /// 与 `do_parent()`（服务于 `absolute_path()` 路径重建）不同，
    /// 此方法用于路径查找场景（`vfs_lookup` 中的 `find("..")`）。
    ///
    /// 规则：
    /// - 如果是挂载点根且有父挂载点：跨越挂载边界，到父文件系统中挂载点的父目录
    /// - 如果是全局根（无父挂载点）：返回自身
    /// - 如果是普通目录：委托给 inner_inode.find("..")，结果包一层 overlay
    fn lookup_dotdot(&self) -> Result<Arc<MountFSInode>, SyscallErr> {
        if self.is_mountpoint_root() {
            if let Some(mountpoint) = self.mount_fs.self_mountpoint() {
                // Cross mount boundary: go to the mountpoint's parent directory
                // in the parent filesystem's VFS tree (Linux semantics).
                let parent_inner = mountpoint.inner_inode.find("..")?;
                let result = MountFSInode::new(parent_inner, mountpoint.mount_fs.clone());
                counters::MFSI_FROM_PARENT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                return Ok(MountFSInode::overlaid_inode(result));
            }
            // Global root: ".." is itself
            return Ok(self.self_arc());
        }

        // Non-mount-root: delegate to inner filesystem, overlay the result
        let inner_inode = self.inner_inode.find("..")?;
        let result = MountFSInode::new(inner_inode, self.mount_fs.clone());
        counters::MFSI_FROM_PARENT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        Ok(MountFSInode::overlaid_inode(result))
    }

    /// Create a new MountFS and attach it as a child of this inode's parent
    /// MountFS. When `do_propagate` is true and the parent is shared, the
    /// mount event is replicated to all peer mounts.
    ///
    /// # Locking
    ///
    /// 按序获取以下锁：
    /// 1. `self.inner_inode.metadata()`（读 inode 元数据）
    /// 2. `self.mount_fs.mountpoints.lock()`（通过 `add_mount`）
    /// 3. `MOUNT_LIST`（全局挂载表注册）
    /// 4. 如果父挂载 shared：`propagate_mount` 内部遍历 peer 列表并操作各 peer 的
    ///    `mountpoints`
    ///
    /// 调用者不得持有任何 MountFS 的 `mountpoints` 锁或 `self_mountpoint` 锁。
    pub(crate) fn mount_subtree_inner(
        &self,
        lifecycle: Arc<BackendLifecycle>,
        root_inner_inode: Arc<dyn IndexNode>,
        mount_flags: MountFlags,
        mount_path: Option<String>,
        do_propagate: bool,
    ) -> Result<Arc<MountFS>, SyscallErr> {
        let metadata = self.inner_inode.metadata()?;
        if metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let inode_id = metadata.inode_id;

        let new_mount_fs = MountFS::new_with_root(lifecycle, root_inner_inode, mount_flags);

        // If parent is shared, allocate a fresh child peer group for the
        // new mount. Linux semantics: mount events under shared parents
        // form their own peer group, not the parent's. Propagated clones
        // join this new group. Defer peer registration until AFTER
        // propagation to avoid self-peer loops.
        let parent_prop = self.mount_fs.propagation();
        let parent_shared = parent_prop.is_shared();
        if parent_shared {
            super::propagation::set_shared_new_group(&new_mount_fs);
        }

        let backref = MountFSInode::new(self.inner_inode.clone(), self.mount_fs.clone());
        counters::MFSI_FROM_BACKREF.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        new_mount_fs.set_self_mountpoint(Some(backref));

        self.mount_fs.add_mount(inode_id, new_mount_fs.clone())?;

        new_mount_fs.set_mount_path(mount_path);

        // Register in global mount list
        if let Some(ref path) = new_mount_fs.mount_path() {
            MOUNT_LIST.insert(path.as_str(), new_mount_fs.clone(), Some(inode_id));
        }

        // Propagate to peers if parent is shared (only from public API)
        if do_propagate && parent_shared {
            let mount_path_owned = new_mount_fs.mount_path();
            let child_name = mount_path_owned
                .as_ref()
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or("");
            propagate_mount(&self.mount_fs, inode_id, &new_mount_fs, child_name);
        }

        // Register in peer group AFTER propagation (prevent self-peer loop).
        // Only the public auto-propagating path may auto-register; manual callers
        // using do_propagate=false must set final propagation and register themselves.
        if do_propagate && parent_shared {
            register_peer(&new_mount_fs);
        }

        Ok(new_mount_fs)
    }

    /// Create a new MountFS rooted at `root_inner_inode` and attach it as a
    /// child of this MountFSInode's parent MountFS at this inode's position.
    pub fn mount_subtree(
        &self,
        lifecycle: Arc<BackendLifecycle>,
        root_inner_inode: Arc<dyn IndexNode>,
        mount_flags: MountFlags,
        mount_path: Option<String>,
    ) -> Result<Arc<MountFS>, SyscallErr> {
        self.mount_subtree_inner(lifecycle, root_inner_inode, mount_flags, mount_path, true)
    }
}

impl IndexNode for MountFSInode {
    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.inner_inode.read_at(offset, len, buf, data)
    }

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.write_at(offset, len, buf, data)
    }

    fn read_at_user(
        &self,
        offset: usize,
        len: usize,
        dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        self.inner_inode.read_at_user(offset, len, dst)
    }

    fn write_at_user(
        &self,
        offset: usize,
        len: usize,
        src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let writable_start = crate::task::perf::perf_memory_io_time_now();
        let writable = self.ensure_mount_writable();
        crate::task::perf::record_pwrite_mount_writable(
            crate::task::perf::perf_memory_io_time_now().wrapping_sub(writable_start),
        );
        writable?;
        self.inner_inode.write_at_user(offset, len, src)
    }

    fn supports_user_buffer_io(&self) -> bool {
        self.inner_inode.supports_user_buffer_io()
    }

    fn is_discard_write(&self) -> bool {
        self.inner_inode.is_discard_write()
    }

    fn discard_write_at(
        &self,
        offset: usize,
        len: usize,
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.discard_write_at(offset, len, data)
    }

    fn read_direct(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.inner_inode.read_direct(offset, len, buf, data)
    }

    fn write_direct(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.write_direct(offset, len, buf, data)
    }

    fn read_sync(&self, offset: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        self.inner_inode.read_sync(offset, buf)
    }

    fn write_sync(&self, offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.write_sync(offset, buf)
    }

    fn open(&self, data: MutexGuard<FilePrivateData>, flags: &FileFlags) -> Result<(), SyscallErr> {
        self.inner_inode.open(data, flags)
    }

    fn close(&self, data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        self.inner_inode.close(data)
    }

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.do_find(name)
            .map(|mnt_inode| mnt_inode as Arc<dyn IndexNode>)
    }

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        self.inner_inode.list()
    }

    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr> {
        let mut entries = self.inner_inode.list_dirents()?;

        // Overlay-correct d_ino: if a directory entry corresponds to a
        // mountpoint, the d_ino must reflect the mounted filesystem's
        // root inode, not the covered inode. Without this, stat(".")=12
        // in a bind-mounted /musl but getdents("/") reports d_ino=31
        // for "musl" → musl getcwd manual walk fails.
        let mountpoints = self.mount_fs.mountpoints.lock();
        for (_, ino, _) in entries.iter_mut() {
            if let Some(child_mfs) = mountpoints.get(ino) {
                let root_inner = child_mfs
                    .root_inner_inode
                    .clone()
                    .unwrap_or_else(|| child_mfs.lifecycle.fs().root_inode());
                if let Ok(md) = root_inner.metadata() {
                    *ino = md.inode_id;
                }
            }
        }
        Ok(entries)
    }

    fn create(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        // Self-overlay: if this inode is a mountpoint, redirect create to
        // the mounted filesystem's root.
        let self_arc = self.self_arc();
        let top = MountFSInode::overlaid_inode(self_arc.clone());
        if !Arc::ptr_eq(&top, &self_arc) {
            return top.create(name, file_type, mode);
        }

        self.ensure_mount_writable()?;
        self.mount_fs
            .dentry_gen
            .fetch_add(1, core::sync::atomic::Ordering::Release);
        let parent_ino = self.inner_inode.metadata().ok().map(|m| m.inode_id);
        let inner_inode = self.inner_inode.create(name, file_type, mode)?;
        let wrapper = MountFSInode::new(inner_inode, self.mount_fs.clone());
        if let Some(parent_ino) = parent_ino {
            wrapper.remember_path_hint(parent_ino, name);
        }
        counters::MFSI_FROM_CREATE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let (_, evicted) = {
                    let mut cache = self.mount_fs.dentry_cache.lock();
                    cache.insert_or_get(key, wrapper.clone())
                };
                drop(evicted);
            }
        }
        Ok(wrapper)
    }

    fn create_with_data(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
        data: usize,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs
            .dentry_gen
            .fetch_add(1, core::sync::atomic::Ordering::Release);
        let parent_ino = self.inner_inode.metadata().ok().map(|m| m.inode_id);
        let inner_inode = self
            .inner_inode
            .create_with_data(name, file_type, mode, data)?;
        let wrapper = MountFSInode::new(inner_inode, self.mount_fs.clone());
        if let Some(parent_ino) = parent_ino {
            wrapper.remember_path_hint(parent_ino, name);
        }
        counters::MFSI_FROM_CREATE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let (_, evicted) = self
                    .mount_fs
                    .dentry_cache
                    .lock()
                    .insert_or_get(key, wrapper.clone());
                drop(evicted);
            }
        }
        Ok(wrapper)
    }

    fn create_with_attrs(
        &self,
        name: &str,
        file_type: FileType,
        attrs: super::index_node::CreateAttrs,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs
            .dentry_gen
            .fetch_add(1, core::sync::atomic::Ordering::Release);
        let parent_ino = self.inner_inode.metadata().ok().map(|m| m.inode_id);
        let inner_inode = self.inner_inode.create_with_attrs(name, file_type, attrs)?;
        let wrapper = MountFSInode::new(inner_inode, self.mount_fs.clone());
        if let Some(parent_ino) = parent_ino {
            wrapper.remember_path_hint(parent_ino, name);
        }
        counters::MFSI_FROM_CREATE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let (_, evicted) = self
                    .mount_fs
                    .dentry_cache
                    .lock()
                    .insert_or_get(key, wrapper.clone());
                drop(evicted);
            }
        }
        Ok(wrapper)
    }

    fn symlink(&self, name: &str, target: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs
            .dentry_gen
            .fetch_add(1, core::sync::atomic::Ordering::Release);
        let parent_ino = self.inner_inode.metadata().ok().map(|m| m.inode_id);
        let inner_inode = self.inner_inode.symlink(name, target)?;
        let wrapper = MountFSInode::new(inner_inode, self.mount_fs.clone());
        if let Some(parent_ino) = parent_ino {
            wrapper.remember_path_hint(parent_ino, name);
        }
        counters::MFSI_FROM_CREATE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let (_, evicted) = self
                    .mount_fs
                    .dentry_cache
                    .lock()
                    .insert_or_get(key, wrapper.clone());
                drop(evicted);
            }
        }
        Ok(wrapper)
    }

    fn link(&self, name: &str, other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs
            .dentry_gen
            .fetch_add(1, core::sync::atomic::Ordering::Release);
        let other = MountFSInode::unwrap_inode(other);
        self.inner_inode.link(name, &other)?;
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let linked = MountFSInode::new(
                    self.inner_inode.find(name).unwrap_or(other),
                    self.mount_fs.clone(),
                );
                linked.remember_path_hint(parent_md.inode_id, name);
                let (_, evicted) = self.mount_fs.dentry_cache.lock().insert_or_get(key, linked);
                drop(evicted);
            }
        }
        Ok(())
    }

    fn rename(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn IndexNode>,
        new_name: &str,
        flags: u32,
    ) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs
            .dentry_gen
            .fetch_add(1, core::sync::atomic::Ordering::Release);

        // Also check destination parent mount is writable
        if let Some(new_mnt) = new_parent.as_any_ref().downcast_ref::<MountFSInode>() {
            new_mnt.ensure_mount_writable()?;
        }

        let renamed_ino = self
            .inner_inode
            .find(old_name)
            .ok()
            .and_then(|inode| inode.metadata().ok().map(|m| m.inode_id));

        let new_parent = MountFSInode::unwrap_inode(new_parent);
        self.inner_inode
            .rename(old_name, &new_parent, new_name, flags)?;
        if let Some(child_ino) = renamed_ino {
            if let Ok(new_parent_md) = new_parent.metadata() {
                self.mount_fs.remember_inode_path_hint(
                    child_ino,
                    PathHint {
                        parent_ino: new_parent_md.inode_id,
                        name: String::from(new_name),
                    },
                );
            } else {
                self.mount_fs.remove_inode_path_hint(child_ino);
            }
        }

        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let old_key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(old_name),
                };
                let old_evicted = {
                    let mut cache = self.mount_fs.dentry_cache.lock();
                    cache.invalidate(&old_key)
                };
                drop(old_evicted);

                if let Ok(new_parent_md) = new_parent.metadata() {
                    let new_key = super::dentry_cache::DentryKey {
                        parent_ino: new_parent_md.inode_id,
                        name: String::from(new_name),
                    };
                    let new_evicted = {
                        let mut cache = self.mount_fs.dentry_cache.lock();
                        cache.invalidate(&new_key)
                    };
                    drop(new_evicted);
                }
            }
        }
        Ok(())
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs
            .dentry_gen
            .fetch_add(1, core::sync::atomic::Ordering::Release);
        // 检查是否为挂载点
        let child_inode_id = if let Ok(inode) = self.inner_inode.find(name) {
            let inode_id = inode.metadata()?.inode_id;
            if self.mount_fs.mountpoints.lock().contains_key(&inode_id) {
                return Err(SyscallErr::EBUSY);
            }
            Some(inode_id)
        } else {
            None
        };
        self.inner_inode.unlink(name)?;
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let removed = {
                    let mut cache = self.mount_fs.dentry_cache.lock();
                    cache.invalidate(&key)
                };
                drop(removed);
            }
        }
        if let Some(child_ino) = child_inode_id {
            self.mount_fs.remove_inode_path_hint(child_ino);
        }
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs
            .dentry_gen
            .fetch_add(1, core::sync::atomic::Ordering::Release);
        // 检查是否为挂载点
        let child_inode_id = if let Ok(inode) = self.inner_inode.find(name) {
            let inode_id = inode.metadata()?.inode_id;
            if self.mount_fs.mountpoints.lock().contains_key(&inode_id) {
                return Err(SyscallErr::EBUSY);
            }
            Some(inode_id)
        } else {
            None
        };
        self.inner_inode.rmdir(name)?;
        if let Some(child_ino) = child_inode_id {
            self.mount_fs.remove_inode_path_hint(child_ino);
        }
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let removed = {
                    let mut cache = self.mount_fs.dentry_cache.lock();
                    cache.invalidate(&key)
                };
                drop(removed);
            }
            if let Some(child_ino) = child_inode_id {
                let evicted = {
                    let mut cache = self.mount_fs.dentry_cache.lock();
                    cache.clear_parent(child_ino)
                };
                drop(evicted);
            }
        }
        Ok(())
    }

    fn metadata(&self) -> Result<super::Metadata, SyscallErr> {
        self.inner_inode.metadata()
    }

    fn touch_modified(&self) {
        self.inner_inode.touch_modified();
    }

    fn set_metadata(&self, metadata: &super::Metadata) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.set_metadata(metadata)
    }

    fn setxattr(&self, name: &str, value: &[u8], flags: u32) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.setxattr(name, value, flags)
    }

    fn getxattr(&self, name: &str, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        self.inner_inode.getxattr(name, buf)
    }

    fn listxattr(&self, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        self.inner_inode.listxattr(buf)
    }

    fn removexattr(&self, name: &str) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.removexattr(name)
    }

    fn get_entry_name(&self, ino: InodeId) -> Result<String, SyscallErr> {
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let entries = {
                    let cache = self.mount_fs.dentry_cache.lock();
                    cache.entries_for_parent(parent_md.inode_id)
                };
                for (name, node) in entries {
                    if node.metadata().map(|m| m.inode_id).ok() == Some(ino) {
                        return Ok(name);
                    }
                }
            }
        }
        self.inner_inode.get_entry_name(ino)
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.resize(len)
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.truncate(len)
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.mount_fs.clone()
    }

    /// 卸载此 inode 下的文件系统。
    ///
    /// # Locking
    ///
    /// 获取 `self.mount_fs.mountpoints` 锁查找目标挂载，然后调用 `mounted.umount()`。
    /// `umount()` 内部释放 `self_mountpoint`（打破 MountFS → MountFSInode 循环引用），
    /// 并从全局 `MOUNT_LIST` 注销。
    ///
    /// # Errors
    ///
    /// 目标 inode 未挂载返回 `EINVAL`；挂载点仍有子挂载或打开文件返回 `EBUSY`。
    fn umount(&self) -> Result<Arc<MountFS>, SyscallErr> {
        if self.is_mountpoint_root() {
            self.mount_fs.umount()?;
            return Ok(self.mount_fs.clone());
        }

        let inode_id = self.inner_inode.metadata()?.inode_id;
        let mounted = {
            let mountpoints = self.mount_fs.mountpoints.lock();
            mountpoints.get(&inode_id).cloned()
        }
        .ok_or_else(|| {
            let parent_path = self.mount_fs.mount_path().unwrap_or_else(|| alloc::string::String::from("(nopath)"));
            log::warn!(
                "[umount] EINVAL: inode_id {:?} is NOT a mountpoint under '{}' (mountpoints count: {})",
                inode_id,
                parent_path,
                self.mount_fs.mountpoints.lock().len(),
            );
            SyscallErr::EINVAL
        })?;
        mounted.umount()?;
        Ok(mounted)
    }

    fn page_cache(&self) -> Option<Arc<super::super::page_cache::PageCache>> {
        self.inner_inode.page_cache()
    }

    fn ensure_page_cache(&self) -> Option<Arc<super::super::page_cache::PageCache>> {
        self.inner_inode.ensure_page_cache()
    }

    fn sync(&self) -> Result<(), SyscallErr> {
        self.inner_inode.sync()
    }

    fn datasync(&self) -> Result<(), SyscallErr> {
        self.inner_inode.datasync()
    }

    fn ioctl(
        &self,
        cmd: u32,
        data: usize,
        private_data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.inner_inode.ioctl(cmd, data, private_data)
    }

    fn poll(&self, private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        self.inner_inode.poll(private_data)
    }

    fn read_wait_queue(&self) -> Option<&spin::Mutex<crate::task::WaitQueue>> {
        self.inner_inode.read_wait_queue()
    }

    fn read_event_queue(&self) -> Option<&super::event::EventWaitQueue> {
        self.inner_inode.read_event_queue()
    }

    fn write_wait_queue(&self) -> Option<&spin::Mutex<crate::task::WaitQueue>> {
        self.inner_inode.write_wait_queue()
    }

    fn write_event_queue(&self) -> Option<&super::event::EventWaitQueue> {
        self.inner_inode.write_event_queue()
    }

    fn is_stream(&self) -> bool {
        self.inner_inode.is_stream()
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }

    fn absolute_path(&self) -> Result<String, SyscallErr> {
        let mut current = self.self_arc();
        let mut path_parts: Vec<String> = Vec::new();

        loop {
            if current.is_mountpoint_root() {
                if let Some(mountpoint) = current.mount_fs.self_mountpoint() {
                    current = mountpoint;
                    continue;
                }
                break;
            }

            let parent = current.do_parent()?;
            if Arc::ptr_eq(&parent, &current) {
                break;
            }

            // 在 parent 中查找 current 的名称
            let child_ino = current.metadata()?.inode_id;
            let parent_ino = parent.metadata()?.inode_id;
            let name =
                if let Some(name) = current.valid_path_hint_name(&parent, parent_ino, child_ino) {
                    name
                } else {
                    parent.get_entry_name(child_ino)?
                };
            path_parts.push(name);

            if path_parts.len() > 64 {
                return Err(SyscallErr::ELOOP);
            }

            current = parent;
        }

        path_parts.reverse();
        let mut absolute_path = String::with_capacity(
            path_parts.iter().map(|s| s.len()).sum::<usize>() + path_parts.len(),
        );
        for part in path_parts {
            absolute_path.push('/');
            absolute_path.push_str(&part);
        }
        if absolute_path.is_empty() {
            absolute_path.push('/');
        }
        Ok(absolute_path)
    }
}

impl MountFSInode {
    pub fn umount_force(&self) -> Result<Arc<MountFS>, SyscallErr> {
        if self.is_mountpoint_root() {
            self.mount_fs.umount_force()?;
            return Ok(self.mount_fs.clone());
        }
        let inode_id = self.inner_inode.metadata()?.inode_id;
        let mounted = {
            let mountpoints = self.mount_fs.mountpoints.lock();
            mountpoints.get(&inode_id).cloned()
        }
        .ok_or(SyscallErr::EINVAL)?;
        mounted.umount_force()?;
        Ok(mounted)
    }
}

impl Drop for MountFSInode {
    fn drop(&mut self) {
        counters::MOUNTFSINODE_ALIVE.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}

// ── MountFS ─────────────────────────────────────────────────────────────

/// MountFS — 挂载感知的文件系统包装器
///
/// 包装一个具体的 `FileSystem`，附加挂载点管理。
/// 对标 DragonOS `kernel/src/filesystem/vfs/mount.rs` 的 `MountFS`。
#[derive(Debug)]
pub struct MountFS {
    /// Shared backend lifecycle (owns `Arc<dyn FileSystem>`).
    pub lifecycle: Arc<BackendLifecycle>,
    /// 根 inode
    root_inner_inode: Option<Arc<dyn IndexNode>>,
    /// 子挂载点表: parent_inode_id → mounted fs
    pub mountpoints: Mutex<BTreeMap<InodeId, Arc<MountFS>>>,
    /// Bounded inode → parent/name hints for physical path reconstruction.
    path_hints: Mutex<BTreeMap<InodeId, PathHint>>,
    /// 自身挂载到父文件系统上的 inode（如果是根则 None）。
    /// DragonOS 存 Arc 而非 Weak——循环由 umount 时 take() 打破。
    self_mountpoint: Mutex<Option<Arc<MountFSInode>>>,
    /// 挂载标志
    mount_flags: AtomicU32,
    /// 挂载源
    mount_source: Mutex<Option<String>>,
    /// 挂载目标路径
    mount_path: Mutex<Option<String>>,
    /// 挂载传播状态
    propagation: MountPropagation,
    /// 指向自身的弱引用
    self_ref: Mutex<Weak<MountFS>>,
    /// Dentry cache: (parent_ino, name) → Arc<MountFSInode>
    pub dentry_cache: Mutex<DentryCache>,
    /// 目录版本号，任何目录修改（create/unlink/rmdir/rename）后递增。
    /// 用于检测并发修改，防止 find() 插入 stale dentry。
    pub dentry_gen: AtomicU64,
    /// 禁用 dentry cache 的动态文件系统（如 procfs）
    pub no_dentry_cache: AtomicBool,
    /// umount EBUSY 重试计数，连续 3 次 EBUSY 后第 4 次自动 force-detach
    umount_retry_count: AtomicU32,
    /// Count of files currently opened for write on this mount.
    /// Used to reject MS_REMOUNT to read-only while writers exist (EBUSY).
    pub writers: AtomicU32,
}

impl MountFS {
    /// Create a new MountFS wrapping a fresh BackendLifecycle.
    ///
    /// The lifecycle is **acquired** in this constructor so the returned
    /// `MountFS` holds one reference.  The caller must have pre-registered
    /// the lifecycle via `BackendLifecycle::new()`.
    pub fn new(lifecycle: Arc<BackendLifecycle>, mount_flags: MountFlags) -> Arc<Self> {
        let _ = lifecycle
            .acquire()
            .expect("BackendLifecycle::acquire failed on fresh lifecycle");
        counters::MOUNTFS_ALIVE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let mfs = Arc::new_cyclic(|self_ref| MountFS {
            root_inner_inode: None,
            lifecycle,
            mountpoints: Mutex::new(BTreeMap::new()),
            path_hints: Mutex::new(BTreeMap::new()),
            self_mountpoint: Mutex::new(None),
            mount_flags: AtomicU32::new(mount_flags.bits()),
            mount_source: Mutex::new(None),
            mount_path: Mutex::new(None),
            propagation: MountPropagation::new_private(),
            self_ref: Mutex::new(self_ref.clone()),
            dentry_cache: Mutex::new(DentryCache::new()),
            dentry_gen: AtomicU64::new(0),
            no_dentry_cache: AtomicBool::new(false),
            umount_retry_count: AtomicU32::new(0),
            writers: AtomicU32::new(0),
        });
        counters::MOUNT_LIFECYCLE_CREATE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        diag_mount_event("create", &mfs);
        mfs
    }

    /// 创建以指定 inode 为根的 MountFS（用于 bind mount）
    pub fn new_with_root(
        lifecycle: Arc<BackendLifecycle>,
        root_inner_inode: Arc<dyn IndexNode>,
        mount_flags: MountFlags,
    ) -> Arc<Self> {
        let _ = lifecycle
            .acquire()
            .expect("BackendLifecycle::acquire failed on fresh lifecycle");
        counters::MOUNTFS_ALIVE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let mfs = Arc::new_cyclic(|self_ref| MountFS {
            root_inner_inode: Some(root_inner_inode),
            lifecycle,
            mountpoints: Mutex::new(BTreeMap::new()),
            path_hints: Mutex::new(BTreeMap::new()),
            self_mountpoint: Mutex::new(None),
            mount_flags: AtomicU32::new(mount_flags.bits()),
            mount_source: Mutex::new(None),
            mount_path: Mutex::new(None),
            propagation: MountPropagation::new_private(),
            self_ref: Mutex::new(self_ref.clone()),
            dentry_cache: Mutex::new(DentryCache::new()),
            dentry_gen: AtomicU64::new(0),
            no_dentry_cache: AtomicBool::new(false),
            umount_retry_count: AtomicU32::new(0),
            writers: AtomicU32::new(0),
        });
        counters::MOUNT_LIFECYCLE_CREATE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        diag_mount_event("create-with-root", &mfs);
        mfs
    }

    fn remember_inode_path_hint(&self, child_ino: InodeId, hint: PathHint) {
        let mut hints = self.path_hints.lock();
        if hints.len() >= PATH_HINT_LIMIT && !hints.contains_key(&child_ino) {
            if let Some(old_ino) = hints.keys().next().cloned() {
                hints.remove(&old_ino);
            }
        }
        hints.insert(child_ino, hint);
    }

    fn lookup_inode_path_hint(&self, child_ino: InodeId) -> Option<PathHint> {
        self.path_hints.lock().get(&child_ino).cloned()
    }

    fn remove_inode_path_hint(&self, child_ino: InodeId) {
        self.path_hints.lock().remove(&child_ino);
    }

    /// 获取挂载点根 inode（穿过子挂载表找最底层）
    pub fn mountpoint_root_inode(&self) -> Arc<MountFSInode> {
        let root_inner = self
            .root_inner_inode
            .clone()
            .unwrap_or_else(|| self.lifecycle.fs().root_inode());

        let self_arc = self.self_ref.lock().upgrade().unwrap();
        let root_mount_inode = MountFSInode::new(root_inner, self_arc);
        counters::MFSI_FROM_ROOT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        MountFSInode::overlaid_inode(root_mount_inode)
    }

    /// 获取"被覆盖的"根 inode — 不穿透子挂载 overlay。
    /// 用于 propagation peer 定位，避免 mount 被注册到错误的 MountFS 层。
    pub fn covered_root_inode(&self) -> Arc<MountFSInode> {
        let root_inner = self
            .root_inner_inode
            .clone()
            .unwrap_or_else(|| self.lifecycle.fs().root_inode());
        let self_arc = self.self_ref.lock().upgrade().unwrap();
        let inode = MountFSInode::new(root_inner, self_arc);
        counters::MFSI_FROM_ROOT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        inode
    }

    /// 添加子挂载点。
    ///
    /// # Locking
    ///
    /// 获取 `self.mountpoints` 锁。如果 `inode_id` 已挂载返回 `EEXIST`。
    /// 调用者不得在持锁时调用可能触发 mount 传播的函数（避免 ABBA 死锁）。
    pub fn add_mount(&self, inode_id: InodeId, mount_fs: Arc<MountFS>) -> Result<(), SyscallErr> {
        let mut mountpoints = self.mountpoints.lock();
        if mountpoints.contains_key(&inode_id) {
            return Err(SyscallErr::EEXIST);
        }
        mountpoints.insert(inode_id, mount_fs);
        Ok(())
    }

    /// Replace any existing mount at inode_id, or add if none exists.
    /// Old mount is fully detached so its parent backref and cached children
    /// cannot keep the covered subtree alive after overmount.
    /// Returns the old mount (None if no existing mount).
    pub fn overmount_and_add(
        &self,
        inode_id: InodeId,
        mount_fs: Arc<MountFS>,
    ) -> Option<Arc<MountFS>> {
        let mut mps = self.mountpoints.lock();
        let old = mps.remove(&inode_id);
        if let Some(ref old_mfs) = old {
            drop(mps);
            unregister_peer_mount(old_mfs);
            unregister_slave_mount(old_mfs);
            MOUNT_LIST.remove_fs(old_mfs);
            old_mfs.set_self_mountpoint(None);
            mps = self.mountpoints.lock();
        }
        mps.insert(inode_id, mount_fs);
        old
    }

    /// 移除子挂载点
    pub fn remove_mount(&self, inode_id: InodeId) -> Option<Arc<MountFS>> {
        self.mountpoints.lock().remove(&inode_id)
    }

    /// Debug: dump mount state for diagnosing EBUSY/EINVAL on umount.
    /// Does NOT panic — all reads are fallible via if-let/ok() chains.
    pub fn dump_mount_state(self: &Arc<Self>, reason: &str) {
        use alloc::string::ToString;
        use log::warn;

        let path = self.mount_path().unwrap_or_else(|| "(none)".to_string());
        let source = self.mount_source().unwrap_or_else(|| "(none)".to_string());
        let flags = self.mount_flags();
        let prop = self.propagation();
        let prop_type = prop.prop_type();
        let peer_gid = prop.peer_group_id();
        let master_gid = prop.master_group_id();
        let self_ptr = Arc::as_ptr(self) as usize;

        warn!(
            "--- MountFS::dump_mount_state (reason: {}) --- self=0x{:x} path={} source={} flags={:?} prop={:?} peer_gid={} master_gid={}",
            reason, self_ptr, path, source, flags, prop_type, peer_gid, master_gid
        );

        // self_mountpoint info
        if let Some(mp) = self.self_mountpoint() {
            if let Ok(md) = mp.inner_inode.metadata() {
                let parent_path = mp
                    .mount_fs
                    .mount_path()
                    .unwrap_or_else(|| "(nopath)".to_string());
                let parent_ptr = Arc::as_ptr(&mp.mount_fs) as usize;
                warn!(
                    "  self_mountpoint: parent_inode_id={:?} parent_fs_name={} parent=0x{:x} parent_path={}",
                    md.inode_id, mp.mount_fs.name(), parent_ptr, parent_path
                );
                // Check if parent's mountpoints table actually has an entry for us
                {
                    let parent_mps = mp.mount_fs.mountpoints.lock();
                    let parent_has_us = parent_mps
                        .get(&md.inode_id)
                        .map(|child| Arc::ptr_eq(child, self));
                    warn!(
                        "  parent.mountpoints[inode_id={:?}].ptr_eq(self) = {:?} (parent has {} entries)",
                        md.inode_id, parent_has_us, parent_mps.len()
                    );
                    // List ALL parent entries for debugging
                    if !parent_mps.is_empty() {
                        warn!("  parent mounts table:");
                        for (&ino, child) in parent_mps.iter() {
                            let child_ptr = Arc::as_ptr(child) as usize;
                            let child_path =
                                child.mount_path().unwrap_or_else(|| "(nopath)".to_string());
                            let is_us = Arc::ptr_eq(child, self);
                            warn!(
                                "    ino={:?} child=0x{:x} path={} is_self={}",
                                ino, child_ptr, child_path, is_us
                            );
                        }
                    }
                }
            } else {
                warn!("  self_mountpoint: present but metadata failed");
            }
        } else {
            warn!("  self_mountpoint: None (global root or detached)");
        }

        // absolute_path from self_mountpoint
        if let Some(mp) = self.self_mountpoint() {
            match mp.absolute_path() {
                Ok(abs) => warn!("  absolute_path: {}", abs),
                Err(_) => warn!("  absolute_path: FAILED (mount tree walk error)"),
            }
        }

        // Children in mountpoints table
        {
            let mps = self.mountpoints.lock();
            warn!("  children: count={}", mps.len());
            for (ino, child) in mps.iter() {
                let child_ptr = Arc::as_ptr(child) as usize;
                let child_path = child.mount_path().unwrap_or_else(|| "(nopath)".to_string());
                let child_source = child
                    .mount_source()
                    .unwrap_or_else(|| "(nosrc)".to_string());
                let is_self = Arc::ptr_eq(child, self);
                warn!(
                    "    ino={:?} child=0x{:x} path={} source={} self_ref={}",
                    ino, child_ptr, child_path, child_source, is_self
                );
            }
        }

        // Peer group / slave group info
        if peer_gid != 0 {
            let peers = super::propagation::get_peers(self);
            warn!("  peer_group({}): {} active peers", peer_gid, peers.len());
            for p in &peers {
                let p_path = p.mount_path().unwrap_or_else(|| "(nopath)".to_string());
                warn!("    peer path={}", p_path);
            }
        }
        if master_gid != 0 {
            let slaves = super::propagation::get_slaves(master_gid);
            warn!(
                "  slave_group(master={}): {} active slaves",
                master_gid,
                slaves.len()
            );
        }

        // Dump full MOUNT_LIST (global perspective — matches /proc/mounts)
        {
            let snapshot = MOUNT_LIST.snapshot();
            warn!("  MOUNT_LIST (global): {} entries", snapshot.len());
            for (p, mfs, ino) in &snapshot {
                let mfs_ptr = Arc::as_ptr(mfs) as usize;
                let is_self = Arc::ptr_eq(mfs, self);
                let m_path = mfs.mount_path().unwrap_or_else(|| "(nopath)".to_string());
                warn!(
                    "    path={} mfs=0x{:x} mfs_path={} ino={:?} is_self={}",
                    p, mfs_ptr, m_path, ino, is_self
                );
            }
        }

        warn!("--- end MountFS::dump_mount_state ---");
    }

    /// 卸载当前文件系统（内部版本）。
    /// 当 do_propagate=false 时跳过传播步骤，避免递归传播。
    /// 当 force=true 时递归 detach 子挂载后再 detach self；
    /// 当 force=false 且子挂载存在时返回 EBUSY（保留 Linux 语义）。
    ///
    /// DragonOS phase order: children check → detach from parent → propagate → cleanup self.
    pub fn umount_inner(
        self: &Arc<Self>,
        do_propagate: bool,
        force: bool,
    ) -> Result<(), SyscallErr> {
        // Phase 1: check children
        {
            let mountpoints = self.mountpoints.lock();
            if !force && !mountpoints.is_empty() {
                drop(mountpoints);
                return Err(SyscallErr::EBUSY);
            }
            if force {
                let children: Vec<Arc<MountFS>> = mountpoints.values().cloned().collect();
                drop(mountpoints);
                for child in children.iter().rev() {
                    let _ = child.detach_recursive_inner(false);
                }
            }
        }

        // Phase 2: get parent edge & detach from parent mountpoints
        let (ref parent_mfs, inode_id) = self.parent_edge()?;
        parent_mfs.remove_mount(inode_id);

        // Phase 3: propagate to peers/slaves (BEFORE finishing self cleanup)
        if do_propagate {
            propagate_umount(parent_mfs, inode_id, self);
        }

        // Phase 4: cleanup self
        counters::MOUNT_LIFECYCLE_UMOUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        diag_mount_event("pre-umount", self);
        self.finish_umount_cleanup();
        Ok(())
    }

    /// Extract (parent MountFS, mountpoint InodeId) from self_mountpoint backref.
    /// Returns EINVAL if self_mountpoint is None (root mount should not be
    /// detached this way).
    fn parent_edge(self: &Arc<Self>) -> Result<(Arc<MountFS>, InodeId), SyscallErr> {
        let mp = self.self_mountpoint().ok_or(SyscallErr::EINVAL)?;
        let md = mp.inner_inode.metadata()?;
        Ok((mp.mount_fs.clone(), md.inode_id))
    }

    /// Final cleanup after detach: unregister from peer/slave groups, remove
    /// from global MOUNT_LIST, clear backref and children, flush caches.
    fn finish_umount_cleanup(self: &Arc<Self>) {
        unregister_peer_mount(self);
        unregister_slave_mount(self);
        MOUNT_LIST.remove_fs(self);
        self.self_mountpoint.lock().take();
        self.mountpoints.lock().clear();
        let evicted = {
            let mut cache = self.dentry_cache.lock();
            cache.clear_all()
        };
        drop(evicted);
        // on_umount() is now driven by the BackendLifecycle drain in
        // the scheduler — see drain_one_dying_lifecycle().
        // Removing it here ensures sibling bind MountFS instances
        // sharing the same backend are not prematurely torn down.
    }

    /// DragonOS-style narrow cleanup for propagation. Removes self from
    /// parent mountpoints, recursively cleans subtree without on_umount
    /// (clones share BackendLifecycle — only the last Drop triggers drain).
    pub(crate) fn umount_at_peer(self: &Arc<Self>) {
        if let Some(mp) = self.self_mountpoint() {
            if let Ok(md) = mp.inner_inode.metadata() {
                mp.mount_fs.remove_mount(md.inode_id);
            }
        }
        self.finish_propagated_cleanup();
    }

    /// Recursive cleanup for propagation clones: unwind child mounts,
    /// unregister from peer/slave groups, clear backrefs and caches.
    /// Does NOT call on_umount() — only the scheduler drain triggers
    /// fs-level teardown when the last MountFS reference drops.
    fn finish_propagated_cleanup(self: &Arc<Self>) {
        // Recurse into children first
        let children: Vec<Arc<MountFS>> = {
            let mps = self.mountpoints.lock();
            mps.values().cloned().collect()
        };
        for child in children.iter().rev() {
            child.finish_propagated_cleanup();
        }
        unregister_peer_mount(self);
        unregister_slave_mount(self);
        MOUNT_LIST.remove_fs(self);
        self.self_mountpoint.lock().take();
        self.mountpoints.lock().clear();
        let evicted = {
            let mut cache = self.dentry_cache.lock();
            cache.clear_all()
        };
        drop(evicted);
    }

    /// 卸载当前文件系统
    pub fn umount(self: &Arc<Self>) -> Result<(), SyscallErr> {
        self.umount_inner(true, false)
    }

    /// 强制卸载（MNT_DETACH），跳过子挂载检查
    pub fn umount_force(self: &Arc<Self>) -> Result<(), SyscallErr> {
        self.umount_inner(true, true)
    }

    /// Lazily detach this mount and all submounts from the visible mount tree.
    ///
    /// This implements the part of Linux `MNT_DETACH` that LTP cleanup relies
    /// on: remove the subtree from mount lookup immediately, then let normal
    /// `Arc` lifetime rules release objects once outstanding cwd/fd refs go
    /// away.
    pub fn detach_recursive(self: &Arc<Self>) -> Result<(), SyscallErr> {
        self.detach_recursive_inner(true)
    }

    pub(crate) fn detach_recursive_inner(
        self: &Arc<Self>,
        do_propagate: bool,
    ) -> Result<(), SyscallErr> {
        if self.self_mountpoint.lock().is_none() {
            return Err(SyscallErr::EINVAL);
        }

        let propagation_target = if do_propagate {
            self.self_mountpoint.lock().clone().and_then(|mp| {
                mp.inner_inode
                    .metadata()
                    .ok()
                    .map(|md| (mp.mount_fs.clone(), md.inode_id))
            })
        } else {
            None
        };

        let children: Vec<Arc<MountFS>> = {
            let mountpoints = self.mountpoints.lock();
            mountpoints.values().cloned().collect()
        };
        for child in children.iter().rev() {
            let _ = child.detach_recursive_inner(false);
        }

        // Remove self from parent mountpoints BEFORE propagation and cleanup
        let parent_info = self.parent_edge()?;
        let (ref parent_mfs, inode_id) = parent_info;
        parent_mfs.remove_mount(inode_id);

        if do_propagate {
            propagate_umount(parent_mfs, inode_id, self);
        }

        counters::MOUNT_LIFECYCLE_DETACH.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        diag_mount_event("pre-detach", self);
        self.finish_umount_cleanup();
        Ok(())
    }

    // ── 属性访问 ───────────────────────────────────────────────────

    pub fn inner_filesystem(&self) -> Arc<dyn FileSystem> {
        self.lifecycle.fs().clone()
    }

    pub fn root_inner_inode(&self) -> Arc<dyn IndexNode> {
        self.root_inner_inode
            .clone()
            .unwrap_or_else(|| self.lifecycle.fs().root_inode())
    }

    pub fn mount_flags(&self) -> MountFlags {
        MountFlags::from_bits_truncate(self.mount_flags.load(Ordering::Acquire))
    }

    pub fn set_mount_flags(&self, flags: MountFlags) {
        self.mount_flags.store(flags.bits(), Ordering::Release);
    }

    pub fn has_writers(&self) -> bool {
        self.writers.load(Ordering::Relaxed) > 0
    }

    pub fn inc_writers(&self) {
        self.writers.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_writers(&self) {
        self.writers.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn self_mountpoint(&self) -> Option<Arc<MountFSInode>> {
        self.self_mountpoint.lock().clone()
    }

    pub fn set_self_mountpoint(&self, mp: Option<Arc<MountFSInode>>) {
        *self.self_mountpoint.lock() = mp;
    }

    pub fn mount_source(&self) -> Option<String> {
        self.mount_source.lock().clone()
    }

    pub fn set_mount_source(&self, source: Option<String>) {
        *self.mount_source.lock() = source;
    }

    pub fn mount_path(&self) -> Option<String> {
        self.mount_path.lock().clone()
    }

    pub fn propagation(&self) -> &MountPropagation {
        &self.propagation
    }

    pub fn set_mount_path(&self, path: Option<String>) {
        *self.mount_path.lock() = path;
    }
}

impl Drop for MountFS {
    fn drop(&mut self) {
        counters::MOUNT_LIFECYCLE_DROP.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        // Release one MountFS reference from the shared backend lifecycle.
        // If this was the last reference, CAS the state Active→Dying.
        // The scheduler drain will then call on_umount() outside any lock.
        //
        // No logging, no locking, no allocation, no cache/callback cleanup:
        // all of that is handled in the explicit normal/lazy/propagated
        // cleanup paths (finish_umount_cleanup, finish_propagated_cleanup).
        self.lifecycle.release();
        counters::MOUNTFS_ALIVE.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}

impl FileSystem for MountFS {
    fn identity_key(&self) -> usize {
        self.lifecycle.fs().identity_key()
    }

    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.mountpoint_root_inode()
    }

    fn info(&self) -> super::file_system::FsInfo {
        self.lifecycle.fs().info()
    }

    fn name(&self) -> &str {
        self.lifecycle.fs().name()
    }

    fn super_block(&self) -> super::file_system::SuperBlock {
        let mut sb = self.lifecycle.fs().super_block();
        sb.flags = mount_flags_to_st_flags(self.mount_flags());
        sb
    }

    fn statfs(
        &self,
        inode: &Arc<dyn IndexNode>,
    ) -> Result<super::file_system::SuperBlock, SyscallErr> {
        // Unwrap MountFSInode to reach the inner filesystem's statfs,
        // then OR in the mount-level flags (converted to ST_* ABI) so
        // that statfs(2) reports ST_RDONLY / ST_NOSYMFOLLOW etc.
        let fs = self.lifecycle.fs();
        let mut sb = if let Some(mfsi) = inode.as_any_ref().downcast_ref::<MountFSInode>() {
            fs.statfs(&mfsi.inner_inode)?
        } else {
            fs.statfs(inode)?
        };
        sb.flags |= mount_flags_to_st_flags(self.mount_flags());
        Ok(sb)
    }

    fn support_readahead(&self) -> bool {
        self.lifecycle.fs().support_readahead()
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }
}

// ── MountList ────────────────────────────────────────────────────────────

/// 全局挂载列表
///
/// 管理所有挂载关系（路径 → MountFS），用于路径到挂载点的解析。
#[derive(Debug)]
pub struct MountList {
    /// 挂载路径 → (挂载记录列表，支持 stackable mounts)
    mounts: Mutex<BTreeMap<Arc<MountPath>, Vec<MountRecord>>>,
}

/// 单条挂载记录
#[derive(Debug, Clone)]
struct MountRecord {
    /// 挂载的文件系统
    fs: Arc<MountFS>,
    /// 挂载目标的 inode ID
    ino: Option<InodeId>,
}

impl MountList {
    /// 创建空挂载列表
    pub const fn new() -> Self {
        MountList {
            mounts: Mutex::new(BTreeMap::new()),
        }
    }

    /// 添加挂载点
    pub fn insert<T: Into<MountPath>>(&self, path: T, fs: Arc<MountFS>, ino: Option<InodeId>) {
        let mut inner = self.mounts.lock();
        let path: Arc<MountPath> = Arc::new(path.into());
        let entry = inner.entry(path).or_default();
        entry.push(MountRecord { fs, ino });
    }

    /// 按路径查找挂载点
    /// 返回 `(MountPath, 剩余路径, 挂载的 MountFS)`
    pub fn lookup<T: AsRef<str>>(&self, path: T) -> Option<(Arc<MountPath>, String, Arc<MountFS>)> {
        let inner = self.mounts.lock();
        for (key, stack) in inner.iter().rev() {
            let strkey: &str = &key.0;
            if let Some(rest) = path.as_ref().strip_prefix(strkey) {
                if rest.is_empty() || rest.starts_with('/') {
                    if let Some(rec) = stack.last() {
                        let rest_trimmed = rest.trim_start_matches('/');
                        return Some((key.clone(), rest_trimmed.to_string(), rec.fs.clone()));
                    }
                }
            }
        }
        None
    }

    /// 按路径移除挂载
    pub fn remove<T: Into<MountPath>>(&self, path: T) -> Option<Arc<MountFS>> {
        let mut inner = self.mounts.lock();
        let path: MountPath = path.into();
        if let Some(stack) = inner.get_mut(&path) {
            if let Some(rec) = stack.pop() {
                if stack.is_empty() {
                    inner.remove(&path);
                }
                return Some(rec.fs);
            }
        }
        None
    }

    /// Debug: snapshot for mount state dump.
    /// Returns Vec<(path, fs, ino)> sorted by path for deterministic output.
    pub fn snapshot(&self) -> Vec<(String, Arc<MountFS>, Option<InodeId>)> {
        let inner = self.mounts.lock();
        let mut result: Vec<(String, Arc<MountFS>, Option<InodeId>)> = Vec::new();
        for (path, stack) in inner.iter() {
            for rec in stack.iter() {
                result.push((path.0.clone(), rec.fs.clone(), rec.ino));
            }
        }
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    /// Remove one exact mount record by object identity.
    ///
    /// Bind/move/propagation operations may make `absolute_path()` differ from
    /// the path that was recorded at insertion time.  Scanning by `Arc`
    /// identity avoids leaving a strong reference in the global mount list.
    pub fn remove_fs(&self, fs: &Arc<MountFS>) -> Option<Arc<MountFS>> {
        let mut inner = self.mounts.lock();
        let len = inner.len();
        let mut empty_path: Option<Arc<MountPath>> = None;
        let mut removed: Option<Arc<MountFS>> = None;

        for (path, stack) in inner.iter_mut() {
            if let Some(pos) = stack.iter().rposition(|rec| Arc::ptr_eq(&rec.fs, fs)) {
                let rec = stack.remove(pos);
                removed = Some(rec.fs);
                if stack.is_empty() {
                    empty_path = Some(path.clone());
                }
                break;
            }
        }

        if let Some(path) = empty_path {
            inner.remove(&path);
        }

        counters::MOUNT_LIST_REMOVE_FS_CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        counters::MOUNT_LIST_REMOVE_FS_SCAN.fetch_add(len, core::sync::atomic::Ordering::Relaxed);

        removed
    }
}

// ── Global MountList ─────────────────────────────────────────────────────

lazy_static! {
    /// 全局挂载列表，所有通过 mount_subtree 创建的挂载均在此注册。
    pub static ref MOUNT_LIST: MountList = MountList::new();
}
