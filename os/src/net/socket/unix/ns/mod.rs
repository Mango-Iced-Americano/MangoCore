//! Unix domain socket 抽象命名空间表。
//!
//! 提供基于 `BTreeMap<Arc<[u8]>, Weak<dyn Socket>>` 的全局抽象命名空间注册表，
//! 支持 `Abstract` 地址的创建、查找和删除。使用 `Weak` 引用避免循环引用。

use core::sync::atomic::AtomicU64;

use crate::net::{Endpoint, Socket};
use crate::utils::error::{SyscallErr, SyscallRet};
use alloc::collections::btree_map::BTreeMap;
use alloc::sync::{Arc, Weak};
use spin::Mutex;

/// Unix domain socket 路径最大长度（含末尾 NUL），对应 Linux `UNIX_PATH_MAX`。
pub const UNIX_PATH_MAX: usize = 108;
static Next_ID: AtomicU64 = AtomicU64::new(0);
/// 全局抽象命名空间注册表，用于 `Abstract` 地址的 `bind`/`connect`/`sendto` 查找。
pub static ABSTRACT_TABLE: UnixAbstractTable = UnixAbstractTable::new();

/// Unix 抽象命名空间注册表。
///
/// 内部 `BTreeMap` 键为抽象名称字节切片（不含前导 NUL），值为 `Weak<dyn Socket>`。
/// 使用 `Weak` 避免持有 socket 生命周期；`create` 时若存在 `dangling Weak`
/// 则允许覆盖。
pub struct UnixAbstractTable {
    sockets: Mutex<BTreeMap<Arc<[u8]>, Weak<dyn Socket>>>,
}

impl UnixAbstractTable {
    pub const fn new() -> Self {
        Self {
            sockets: Mutex::new(BTreeMap::new()),
        }
    }

    fn create(&self, name: Arc<[u8]>, socket: Arc<dyn Socket>) -> SyscallRet {
        if name.is_empty() || name.len() > UNIX_PATH_MAX {
            return Err(SyscallErr::EINVAL);
        }
        let mut table = self.sockets.lock();

        // 检查是否有存活的 Weak（升级成功意味着还有别的强引用，说明真在被用）
        if let Some(Some(_)) = table.get(&name).map(|w| w.upgrade()) {
            log::info!("[ABSTRACT_TABLE] EADDRINUSE for {:?}", name);
            return Err(SyscallErr::EADDRINUSE);
        }
        // 升级失败（dangling Weak）或没有条目 => 覆盖或插入
        table.insert(name, Arc::downgrade(&socket));
        Ok(0)
    }

    fn lookup(&self, name: &[u8]) -> Option<Arc<dyn Socket>> {
        let table = self.sockets.lock();
        let key = Arc::from(name);
        table.get(&key).and_then(|w| {
            let upgraded = w.upgrade();
            if upgraded.is_none() {
                log::info!("[ABSTRACT_TABLE] lookup found dangling Weak for {:?}", name);
            }
            upgraded
        })
    }

    fn remove(&self, name: Arc<[u8]>) {
        let mut table = self.sockets.lock();
        if table.remove(&name).is_some() {
            log::info!("[ABSTRACT_TABLE] removed {:?}", name);
        }
    }

    /// 以字节切片创建抽象命名空间条目（拷贝入 `Arc<[u8]>`）。
    ///
    /// # Errors
    ///
    /// - `EINVAL`：名称为空或超过 `UNIX_PATH_MAX`
    /// - `EADDRINUSE`：该名称已存在存活条目
    pub fn create_abstract_name_bytes(&self, name: &[u8], socket: Arc<dyn Socket>) -> SyscallRet {
        let name_arc = Arc::from(name);
        self.create(name_arc, socket)
    }

    /// 分配一个临时抽象名称并注册。
    ///
    /// 名称由 `Next_ID` 单调递增生成（`u64::to_be_bytes()`），保证唯一性。
    pub fn alloc_ephemeral_abstract_name(&self, socket: Arc<dyn Socket>) -> SyscallRet {
        let id = Next_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let name = id.to_be_bytes();
        self.create_abstract_name_bytes(&name, socket)
    }

    /// 按字节切片查找抽象命名空间中的 socket。
    ///
    /// 返回 `Weak::upgrade()` 结果：存活则返回 `Some(Arc)`，`dangling` 则返回 `None`。
    pub fn lookup_abstract_name_bytes(&self, name: &[u8]) -> Option<Arc<dyn Socket>> {
        self.lookup(name)
    }

    /// 按字节切片从抽象命名空间中移除条目。
    pub fn remove_abstract_name_bytes(&self, name: &[u8]) {
        let name_arc = Arc::from(name);
        self.remove(name_arc);
    }
}
