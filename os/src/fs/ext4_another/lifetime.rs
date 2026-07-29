use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::convert::TryFrom;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// Timestamp updates are published by the write path and persisted only at
    /// a filesystem durability boundary.
    pub(crate) cached_mtime: AtomicU64,
    pub(crate) cached_ctime: AtomicU64,
    pub(crate) mtime_dirty: AtomicBool,
    pub(crate) ctime_dirty: AtomicBool,
    timestamp_generation: AtomicUsize,
    inode_flags: Mutex<InodeFlags>,
    page_cache: Mutex<Option<Weak<PageCache>>>,
    /// Keeps dirty data reachable across independently constructed VFS inodes
    /// until the inode size and data have reached a sync boundary.
    dirty_page_cache: Mutex<Option<Arc<PageCache>>>,
    /// Fast-path publication for repeated writes to an already retained cache.
    dirty_cache_pinned: AtomicBool,
    /// The generation observed after the successful release transaction reset.
    /// A concurrent writer advances `size_generation`, preventing unpinning.
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
    mtime: u64,
    ctime: u64,
    generation: usize,
}

impl CachedTimestamps {
    pub(crate) fn mtime(self) -> TimeSpec {
        unpack_timestamp(self.mtime)
    }

    pub(crate) fn ctime(self) -> TimeSpec {
        unpack_timestamp(self.ctime)
    }
}

impl InodeLifetime {
    fn new(size: usize, mtime: TimeSpec, ctime: TimeSpec) -> Self {
        Self {
            logical_size: Arc::new(AtomicUsize::new(size)),
            cached_mtime: AtomicU64::new(pack_timestamp(mtime)),
            cached_ctime: AtomicU64::new(pack_timestamp(ctime)),
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

    pub(crate) fn cache_modified_time(&self, time: TimeSpec) {
        let cached = pack_timestamp(time);
        self.cached_ctime.store(cached, Ordering::Relaxed);
        self.cached_mtime.store(cached, Ordering::Relaxed);
        self.timestamp_generation.fetch_add(1, Ordering::Release);
        self.ctime_dirty.store(true, Ordering::Release);
        self.mtime_dirty.store(true, Ordering::Release);
    }

    pub(crate) fn cached_mtime(&self) -> Option<TimeSpec> {
        self.mtime_dirty
            .load(Ordering::Acquire)
            .then(|| unpack_timestamp(self.cached_mtime.load(Ordering::Relaxed)))
    }

    pub(crate) fn cached_ctime(&self) -> Option<TimeSpec> {
        self.ctime_dirty
            .load(Ordering::Acquire)
            .then(|| unpack_timestamp(self.cached_ctime.load(Ordering::Relaxed)))
    }

    pub(crate) fn dirty_timestamps(&self) -> Option<CachedTimestamps> {
        if !self.mtime_dirty.load(Ordering::Acquire)
            && !self.ctime_dirty.load(Ordering::Acquire)
        {
            return None;
        }
        Some(CachedTimestamps {
            mtime: self.cached_mtime.load(Ordering::Relaxed),
            ctime: self.cached_ctime.load(Ordering::Relaxed),
            generation: self.timestamp_generation.load(Ordering::Acquire),
        })
    }

    /// Mark a timestamp snapshot durable only when no write superseded it.
    pub(crate) fn finish_timestamp_commit(&self, snapshot: CachedTimestamps) {
        self.mtime_dirty.swap(false, Ordering::AcqRel);
        self.ctime_dirty.swap(false, Ordering::AcqRel);
        if self.timestamp_generation.load(Ordering::Acquire) != snapshot.generation {
            self.mtime_dirty.store(true, Ordering::Release);
            self.ctime_dirty.store(true, Ordering::Release);
        }
    }

    pub(crate) fn page_cache(&self) -> Option<Arc<PageCache>> {
        self.dirty_page_cache
            .lock()
            .clone()
            .or_else(|| self.page_cache.lock().as_ref().and_then(Weak::upgrade))
    }

    pub(crate) fn inode_flags(&self) -> InodeFlags {
        *self.inode_flags.lock()
    }

    pub(crate) fn set_inode_flags(&self, flags: InodeFlags) {
        *self.inode_flags.lock() = flags;
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

    /// Retain a cache that contains data newer than the on-disk inode size.
    /// This closes the write → reopen window in which a new VFS inode would
    /// otherwise allocate a cache and read the old EOF from another_ext4.
    pub(crate) fn retain_dirty_page_cache(&self, cache: &Arc<PageCache>) {
        if self.dirty_cache_pinned.load(Ordering::Acquire) {
            return;
        }
        let mut dirty_page_cache = self.dirty_page_cache.lock();
        if dirty_page_cache.is_none() {
            self.pin();
            *dirty_page_cache = Some(cache.clone());
            self.dirty_cache_pinned.store(true, Ordering::Release);
        }
    }

    /// Release the temporary dirty-cache pin after a successful data and size
    /// sync. The weak cache reference still permits live VFS inodes to reuse it.
    pub(crate) fn release_dirty_page_cache(&self) {
        let mut dirty_page_cache = self.dirty_page_cache.lock();
        let generation = self.size_generation.load(Ordering::Acquire);
        if dirty_page_cache.is_some()
            && generation == self.last_release_generation.load(Ordering::Relaxed)
        {
            *dirty_page_cache = None;
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

fn pack_timestamp(time: TimeSpec) -> u64 {
    let seconds = match u32::try_from(time.tv_sec) {
        Ok(seconds) => seconds,
        Err(_) => u32::MAX,
    };
    let nanoseconds = match u32::try_from(time.tv_nsec) {
        Ok(nanoseconds) => nanoseconds,
        Err(_) => u32::MAX,
    };
    (u64::from(seconds) << 32) | u64::from(nanoseconds)
}

fn unpack_timestamp(timestamp: u64) -> TimeSpec {
    let seconds = match usize::try_from(timestamp >> 32) {
        Ok(seconds) => seconds,
        Err(_) => usize::MAX,
    };
    let nanoseconds = match u32::try_from(timestamp & u64::from(u32::MAX)) {
        Ok(nanoseconds) => nanoseconds,
        Err(_) => u32::MAX,
    };
    TimeSpec {
        tv_sec: seconds,
        tv_nsec: match usize::try_from(nanoseconds) {
            Ok(nanoseconds) => nanoseconds,
            Err(_) => usize::MAX,
        },
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

    pub(crate) fn lifetime(
        &self,
        key: InodeKey,
        size: usize,
        mtime: TimeSpec,
        ctime: TimeSpec,
    ) -> Arc<InodeLifetime> {
        let mut lifetimes = self.lifetimes.lock();
        let lifetime = lifetimes
            .entry(key)
            .or_insert_with(|| Arc::new(InodeLifetime::new(size, mtime, ctime)))
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
            .or_insert_with(|| Arc::new(InodeLifetime::new(0, TimeSpec::new(), TimeSpec::new())))
            .clone();
        *lifetime.reclaim.lock() = Some(handle);
        Ok(())
    }

    pub(crate) fn sync_lifetimes(&self) -> Result<(), SyscallErr> {
        // Snapshot lifetime ownership only. Each inode's generation is loaded
        // after its I/O gate is held, so resize cannot race its size commit.
        let all: Vec<(Option<Arc<PageCache>>, InodeKey, Arc<InodeLifetime>)> = self
            .lifetimes
            .lock()
            .iter()
            .map(|(key, lifetime)| {
                (lifetime.page_cache(), *key, lifetime.clone())
            })
            .collect();
        let mut committed_generations = Vec::new();
        let mut committed_timestamps = Vec::new();
        let mut flush_succeeded = false;
        let result = Self::complete_lifetime_sync(
            || {
                for (maybe_cache, key, lifetime) in &all {
                    // A concurrent resize installs its cache before publishing
                    // a nonzero generation. Re-read it here rather than using
                    // only the pre-sync snapshot, so that generation's commit
                    // remains serialized with the resize.
                    let cache = maybe_cache
                        .as_ref()
                        .cloned()
                        .or_else(|| lifetime.page_cache());
                    let inode_id = u32::try_from(key.inode_id()).map_err(|_| SyscallErr::EFBIG)?;
                    let mut commit_inode = || {
                        let generation = lifetime.size_generation.load(Ordering::Acquire);
                        if generation != 0 {
                            let size = lifetime.logical_size.load(Ordering::Acquire);
                            self.inner()
                                .commit_inode_size(inode_id, size as u64, None)
                                .map_err(|error| from_another(error.code()))?;
                            committed_generations.push((lifetime.clone(), generation));
                        }
                        if let Some(timestamps) =
                            self.commit_lifetime_timestamps(inode_id, lifetime)?
                        {
                            committed_timestamps.push((lifetime.clone(), timestamps));
                        }
                        Ok(())
                    };
                    if let Some(cache) = cache {
                        cache.writeback_all()?;
                        commit_inode()?;
                    } else {
                        commit_inode()?;
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
                    lifetime.release_dirty_page_cache();
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

#[cfg(test)]
mod tests {
    use super::InodeLifetime;
    use crate::timer::TimeSpec;

    #[test]
    fn cached_timestamps_remain_visible_until_a_successful_flush() {
        // Given: a clean inode lifetime and a completed write timestamp update.
        let lifetime = InodeLifetime::new(0, TimeSpec::new(), TimeSpec::new());
        let modified = TimeSpec {
            tv_sec: 123,
            tv_nsec: 456,
        };
        lifetime.cache_modified_time(modified);

        // When: sync snapshots the timestamp update but the device flush has not completed.
        let snapshot = lifetime
            .dirty_timestamps()
            .expect("a write must mark timestamps dirty");

        // Then: stat-facing cache remains current until the persistence barrier succeeds.
        assert_eq!(lifetime.cached_mtime(), Some(modified));
        assert_eq!(lifetime.cached_ctime(), Some(modified));
        lifetime.finish_timestamp_commit(snapshot);
        assert_eq!(lifetime.cached_mtime(), None);
        assert_eq!(lifetime.cached_ctime(), None);
    }

}
