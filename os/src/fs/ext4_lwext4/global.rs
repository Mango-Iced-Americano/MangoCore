//! lwext4 C 库的跨实例串行化门。
//!
//! lwext4 保存设备注册表、挂载表、块缓存、journal 和 orphan list 等全局状态。
//! 每个 `Ext4FileSystem` 自己的 `Mutex<Ext4BlockWrapper>`（`fs.lw`）只能串行化
//! 单个挂载实例；不同 CPU 仍可能经两把实例锁同时进入 C 全局表。因此所有公开 C
//! 入口除实例锁外，还必须先取得进程级唯一的 `LWEXT4_GLOBAL`。
//!
//! # 锁序
//!
//! 全局门位于 Rust PageCache 层与实例/节点锁之间：
//!
//! ```text
//! PageCache op_gate → PageEntry.data → LWEXT4_GLOBAL → fs.lw
//!     → Ext4InodeState.handle / cached_meta / paths / inode_states
//! ```
//!
//! 约束如下：
//! - rename/unlink/create/mkdir/rmdir/symlink/link/mknod 等 namespace 修改
//!   必须在获取全局门前完成 PageCache flush；flush 内的 writeback 会经 `with_file`
//!   自行进入全局门，不能与外层修改段形成不可重入的二次加锁。
//! - `LWEXT4_GLOBAL` 是不可重入的 `spin::Mutex`，不得跨越可缺页的用户访问、
//!   IPI/TLB ack、上下文切换或 `OUTPUT_LOCK`；门内只做 kernel bounce I/O 和短小
//!   的 Rust 状态提交。
//! - `blockdev.rs` 中的 C→Rust 设备回调运行在已经持门的 C 调用内部，绝不能反向
//!   再取得该门。
//!
//! `probe_inode_meta_locked`、`validate_path_locked`、`Ext4InodeState::with_file`
//! 和 `FileGuard::new`/`file_close` 等 locked helper 只允许在已持门时调用，不能
//! 自己再次加锁；全局门只由它们的公开入口获取。

use spin::Mutex;

/// 串行化全部 lwext4 C 入口的进程级全局门。
static LWEXT4_GLOBAL: Mutex<()> = Mutex::new(());

/// 在持有 lwext4 全局门时执行 `f`。
///
/// C 入口包装器必须依次取得本门、实例 `fs.lw`、inode 状态锁。
pub(crate) fn with_lwext4_global<R>(f: impl FnOnce() -> R) -> R {
    let _g = LWEXT4_GLOBAL.lock();
    f()
}
