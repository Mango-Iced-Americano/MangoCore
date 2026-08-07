use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::convert::TryFrom;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::fs::{page_cache::PageCache, vfs::InodeFlags};
use crate::timer::TimeSpec;
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
    cached_times: Mutex<Option<(TimeSpec, TimeSpec)>>,
    mtime_dirty: AtomicBool,
    ctime_dirty: AtomicBool,
    timestamp_generation: AtomicUsize,
    inode_flags: Mutex<InodeFlags>,
    page_cache: Mutex<Option<Weak<PageCache>>>,
    /// Keep dirty cache data reachable until a successful data+metadata sync.
    /// This closes the write → reopen window where a new VFS inode could
    /// otherwise recreate a cache from the old on-disk inode size.
    dirty_page_cache: Mutex<Option<Arc<PageCache>>>,
    /// Fast-path hint for the common case where the dirty cache is already
    /// retained. The mutex remains authoritative so a concurrent writer cannot
    /// miss a release and leave the cache unpinned.
    dirty_cache_pinned: AtomicBool,
    /// Generation associated with the last successful dirty-cache release.
    last_release_generation: AtomicUsize,
    reclaim: Mutex<Option<another_ext4::InodeReclaimHandle>>,
    reclaim_error: Mutex<Option<SyscallErr>>,
    pins: AtomicUsize,
    /// Incremented after each page-cache copy; reset only if a size commit
    /// observes the same generation throughout its transaction.
    pub(crate) size_generation: AtomicUsize,
}

#[derive(Clone, Copy)]
pub(crate) struct CachedTimestamps {
    pub(crate) mtime: TimeSpec,
    pub(crate) ctime: TimeSpec,
    pub(crate) generation: usize,
}

impl InodeLifetime {
    fn new(size: usize) -> Self {
        Self {
            logical_size: Arc::new(AtomicUsize::new(size)),
            cached_times: Mutex::new(None),
            mtime_dirty: AtomicBool::new(false),
            ctime_dirty: AtomicBool::new(false),
            timestamp_generation: AtomicUsize::new(0),
            inode_flags: Mutex::new(InodeFlags::empty()),
            page_cache: Mutex::new(None),
            dirty_page_cache: Mutex::new(None),
            dirty_cache_pinned: AtomicBool::new(false),
            last_release_generation: AtomicUsize::new(0),
            reclaim: Mutex::new(None),
            reclaim_error: Mutex::new(None),
            pins: AtomicUsize::new(0),
            size_generation: AtomicUsize::new(0),
        }
    }

    pub(crate) fn cache_modified_time(&self, now: TimeSpec) {
        *self.cached_times.lock() = Some((now, now));
        self.mtime_dirty.store(true, Ordering::Release);
        self.ctime_dirty.store(true, Ordering::Release);
        self.timestamp_generation.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn dirty_timestamps(&self) -> Option<CachedTimestamps> {
        if !self.mtime_dirty.load(Ordering::Acquire)
            && !self.ctime_dirty.load(Ordering::Acquire)
        {
            return None;
        }
        let times = *self.cached_times.lock();
        times.map(|(mtime, ctime)| CachedTimestamps {
            mtime,
            ctime,
            generation: self.timestamp_generation.load(Ordering::Acquire),
        })
    }

    pub(crate) fn finish_timestamp_commit(&self, snapshot: CachedTimestamps) {
        if self
            .timestamp_generation
            .compare_exchange(
                snapshot.generation,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
        )
            .is_ok()
        {
            // Recheck the generation while holding the timestamp mutex.  A
            // writer may have raced with the CAS and installed a newer pair;
            // never clear that newer snapshot as part of the old commit.
            let mut cached = self.cached_times.lock();
            if self.timestamp_generation.load(Ordering::Acquire) == 0 {
                *cached = None;
                self.mtime_dirty.store(false, Ordering::Release);
                self.ctime_dirty.store(false, Ordering::Release);
            }
        }
    }

    pub(crate) fn cached_times(&self) -> Option<(TimeSpec, TimeSpec)> {
        *self.cached_times.lock()
    }

    pub(crate) fn inode_flags(&self) -> InodeFlags {
        *self.inode_flags.lock()
    }

    pub(crate) fn set_inode_flags(&self, flags: InodeFlags) {
        *self.inode_flags.lock() = flags;
    }

    pub(crate) fn page_cache(&self) -> Option<Arc<PageCache>> {
        self.dirty_page_cache
            .lock()
            .clone()
            .or_else(|| self.page_cache.lock().as_ref().and_then(Weak::upgrade))
    }

    pub(crate) fn install_page_cache(&self, cache: Arc<PageCache>) -> Arc<PageCache> {
        if let Some(existing) = self.page_cache() {
            return existing;
        }
        let mut page_cache = self.page_cache.lock();
        if let Some(existing) = page_cache.as_ref().and_then(Weak::upgrade) {
            return existing;
        }
        *page_cache = Some(Arc::downgrade(&cache));
        cache
    }

    pub(crate) fn retain_dirty_page_cache(&self, cache: &Arc<PageCache>) {
        // The atomic is only a hint. Always take the mutex before deciding
        // that the cache is already retained; a release may have raced with a
        // writer that advanced size_generation before reaching this method.
        let already_pinned = self.dirty_cache_pinned.load(Ordering::Acquire);
        let mut dirty_page_cache = self.dirty_page_cache.lock();
        if already_pinned && dirty_page_cache.is_some() {
            return;
        }
        if dirty_page_cache.is_none() {
            self.pin();
            *dirty_page_cache = Some(cache.clone());
            self.dirty_cache_pinned.store(true, Ordering::Release);
        }
    }

    pub(crate) fn release_dirty_page_cache(&self, committed_generation: usize) {
        let mut dirty_page_cache = self.dirty_page_cache.lock();
        self.last_release_generation
            .store(committed_generation, Ordering::Relaxed);
        // The mutex synchronizes with retain_dirty_page_cache. A writer that
        // advanced the generation while this sync was completing will either
        // make this check fail or repin immediately after the release.
        if self.size_generation.load(Ordering::Acquire) == 0
            && dirty_page_cache.take().is_some()
        {
            self.dirty_cache_pinned.store(false, Ordering::Release);
            self.unpin();
        }
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
        let all: Vec<(Option<Arc<PageCache>>, InodeKey, Arc<InodeLifetime>)> = self
            .lifetimes
            .lock()
            .iter()
            .map(|(key, lifetime)| (lifetime.page_cache(), *key, lifetime.clone()))
            .collect();
        let mut committed_generations = Vec::new();
        let mut committed_timestamps = Vec::new();
        let mut flush_succeeded = false;
        let result = Self::complete_lifetime_sync(
            || {
                for (maybe_cache, key, lifetime) in &all {
                    let cache = maybe_cache.as_ref().cloned().or_else(|| lifetime.page_cache());
                    let mut commit_size = || {
                        let generation = lifetime.size_generation.load(Ordering::Acquire);
                        let timestamps = lifetime.dirty_timestamps();
                        if generation == 0 && timestamps.is_none() {
                            return Ok(());
                        }
                        let id = u32::try_from(key.inode_id()).map_err(|_| SyscallErr::EFBIG)?;
                        if generation != 0 {
                            let size = lifetime.logical_size.load(Ordering::Acquire);
                            self.inner()
                                .commit_inode_size(id, size as u64, None)
                                .map_err(|error| from_another(error.code()))?;
                        }
                        if timestamps.is_some() {
                            if let Some(committed) = self.commit_lifetime_timestamps(id, lifetime)? {
                                committed_timestamps.push((lifetime.clone(), committed));
                            }
                        }
                        if generation != 0 {
                            committed_generations.push((lifetime.clone(), generation));
                        }
                        Ok(())
                    };
                    if let Some(cache) = cache {
                        cache.with_io_gate(|| {
                            cache.writeback_all_with_io_gate_held()?;
                            commit_size()
                        })?;
                    } else {
                        commit_size()?;
                    }
                }
                Ok(())
            },
            || self.drain_reclaims(),
            || {
                let result = self.flush_device();
                flush_succeeded = result.is_ok();
                result
            },
        );
        if flush_succeeded {
            for (lifetime, generation) in committed_generations {
                if lifetime.size_generation.compare_exchange(
                    generation,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ).is_ok() {
                    lifetime.release_dirty_page_cache(generation);
                }
            }
            for (lifetime, timestamps) in committed_timestamps {
                lifetime.finish_timestamp_commit(timestamps);
            }
        }
        result
    }

    /// Completes writeback/reclaim phases and their mandatory final device barrier.
    ///
    /// The helper keeps the error-precedence contract directly testable without
    /// constructing vendor reclaim handles in an in-kernel fixture.
    pub(crate) fn complete_lifetime_sync<W, R, F>(
        writeback: W,
        reclaim: R,
        flush: F,
    ) -> Result<(), SyscallErr>
    where
        W: FnOnce() -> Result<(), SyscallErr>,
        R: FnOnce() -> Result<(), SyscallErr>,
        F: FnOnce() -> Result<(), SyscallErr>,
    {
        let first_error = match writeback() {
            Ok(()) => reclaim().err(),
            Err(error) => Some(error),
        };
        let flush_result = flush();
        match first_error {
            Some(error) => Err(error),
            None => flush_result,
        }
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
