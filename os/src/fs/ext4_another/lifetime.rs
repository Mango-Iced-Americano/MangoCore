use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::convert::TryFrom;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::fs::page_cache::PageCache;
use crate::utils::error::SyscallErr;

use super::errno::from_another;
use super::fs::Ext4FileSystem;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct InodeKey {
    fs_id: usize,
    inode_id: usize,
    generation: u32,
}

impl InodeKey {
    pub(crate) const fn new(fs_id: usize, inode_id: usize, generation: u32) -> Self {
        Self {
            fs_id,
            inode_id,
            generation,
        }
    }

    pub(crate) const fn inode_id(self) -> usize {
        self.inode_id
    }
}

pub(crate) struct InodeLifetime {
    pub(crate) logical_size: Arc<AtomicUsize>,
    page_cache: Mutex<Option<Weak<PageCache>>>,
    reclaim: Mutex<Option<another_ext4::InodeReclaimHandle>>,
    reclaim_error: Mutex<Option<SyscallErr>>,
    pins: AtomicUsize,
}

impl InodeLifetime {
    fn new(size: usize) -> Self {
        Self {
            logical_size: Arc::new(AtomicUsize::new(size)),
            page_cache: Mutex::new(None),
            reclaim: Mutex::new(None),
            reclaim_error: Mutex::new(None),
            pins: AtomicUsize::new(0),
        }
    }

    pub(crate) fn page_cache(&self) -> Option<Arc<PageCache>> {
        self.page_cache.lock().as_ref().and_then(Weak::upgrade)
    }

    pub(crate) fn install_page_cache(&self, cache: Arc<PageCache>) -> Arc<PageCache> {
        let mut page_cache = self.page_cache.lock();
        if let Some(existing) = page_cache.as_ref().and_then(Weak::upgrade) {
            return existing;
        }
        *page_cache = Some(Arc::downgrade(&cache));
        cache
    }

    pub(crate) fn pin(&self) {
        self.pins.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn unpin(&self) {
        let _ = self
            .pins
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pins| {
                pins.checked_sub(1)
            });
    }

    fn take_ready_reclaim(&self) -> Option<another_ext4::InodeReclaimHandle> {
        if self.pins.load(Ordering::Acquire) != 0 {
            return None;
        }
        self.reclaim.lock().take()
    }

    fn restore_reclaim(&self, handle: another_ext4::InodeReclaimHandle, error: SyscallErr) {
        *self.reclaim.lock() = Some(handle);
        *self.reclaim_error.lock() = Some(error);
    }
}

impl Ext4FileSystem {
    pub(crate) fn inode_key(&self, inode_id: u32) -> Result<InodeKey, SyscallErr> {
        let attr = self
            .inner()
            .getattr(inode_id)
            .map_err(|error| from_another(error.code()))?;
        let inode_id = usize::try_from(inode_id).map_err(|_| SyscallErr::EFBIG)?;
        Ok(InodeKey::new(self.fs_id(), inode_id, attr.generation))
    }

    pub(crate) fn lifetime(&self, key: InodeKey, size: usize) -> Arc<InodeLifetime> {
        let mut lifetimes = self.lifetimes.lock();
        let lifetime = lifetimes
            .entry(key)
            .or_insert_with(|| Arc::new(InodeLifetime::new(size)))
            .clone();
        lifetime.pin();
        lifetime
    }

    pub(crate) fn attach_reclaim(
        &self,
        key: InodeKey,
        handle: another_ext4::InodeReclaimHandle,
    ) -> Result<(), SyscallErr> {
        let inode_id = u32::try_from(key.inode_id()).map_err(|_| SyscallErr::EFBIG)?;
        if handle.inode_id() != inode_id || handle.generation() != key.generation {
            return Err(SyscallErr::EIO);
        }
        let lifetime = self
            .lifetimes
            .lock()
            .entry(key)
            .or_insert_with(|| Arc::new(InodeLifetime::new(0)))
            .clone();
        *lifetime.reclaim.lock() = Some(handle);
        Ok(())
    }

    pub(crate) fn sync_lifetimes(&self) -> Result<(), SyscallErr> {
        let caches: Vec<Arc<PageCache>> = self
            .lifetimes
            .lock()
            .values()
            .filter_map(|lifetime| lifetime.page_cache())
            .collect();
        for cache in caches {
            cache.writeback_all()?;
        }
        self.flush_device()?;
        self.drain_reclaims()
    }

    fn drain_reclaims(&self) -> Result<(), SyscallErr> {
        let ready: Vec<(
            InodeKey,
            Arc<InodeLifetime>,
            another_ext4::InodeReclaimHandle,
        )> = self
            .lifetimes
            .lock()
            .iter()
            .filter_map(|(key, lifetime)| {
                lifetime
                    .take_ready_reclaim()
                    .map(|handle| (*key, lifetime.clone(), handle))
            })
            .collect();
        let mut reclaimed = Vec::new();
        let mut failure = None;
        for (key, lifetime, handle) in ready {
            if failure.is_some() {
                if let Some(error) = failure {
                    lifetime.restore_reclaim(handle, error);
                }
                continue;
            }
            match self.inner().reclaim_inode(handle) {
                Ok(()) => reclaimed.push(key),
                Err(reclaim_failure) => {
                    let (error, handle) = reclaim_failure.into_parts();
                    let error = from_another(error.code());
                    lifetime.restore_reclaim(handle, error);
                    failure = Some(error);
                }
            }
        }
        if !reclaimed.is_empty() {
            let mut lifetimes = self.lifetimes.lock();
            for key in reclaimed {
                lifetimes.remove(&key);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
