use core::sync::atomic::AtomicU64;

use crate::net::{Endpoint, Socket};
use crate::utils::error::{SyscallErr, SyscallRet};
use alloc::collections::btree_map::BTreeMap;
use alloc::sync::{Arc, Weak};
use spin::Mutex;

// #[derive(Debug)]
// pub struct AbstractHandle {
//     /// 抽象命名空间中的名字（不含前导 NUL）
//     pub name: Arc<[u8]>,
// }

pub const UNIX_PATH_MAX: usize = 108;
static Next_ID: AtomicU64 = AtomicU64::new(0);
pub static ABSTRACT_TABLE: UnixAbstractTable = UnixAbstractTable::new();

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

    pub fn create_abstract_name_bytes(&self, name: &[u8], socket: Arc<dyn Socket>) -> SyscallRet {
        let name_arc = Arc::from(name);
        self.create(name_arc, socket)
    }

    pub fn alloc_ephemeral_abstract_name(&self, socket: Arc<dyn Socket>) -> SyscallRet {
        let id = Next_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let name = id.to_be_bytes();
        self.create_abstract_name_bytes(&name, socket)
    }

    pub fn lookup_abstract_name_bytes(&self, name: &[u8]) -> Option<Arc<dyn Socket>> {
        self.lookup(name)
    }

    pub fn remove_abstract_name_bytes(&self, name: &[u8]) {
        let name_arc = Arc::from(name);
        self.remove(name_arc);
    }
}
