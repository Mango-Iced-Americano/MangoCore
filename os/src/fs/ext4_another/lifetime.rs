use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::convert::TryFrom;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use crate::fs::{
    page_cache::PageCache,
    vfs::{FileType, InodeFlags, InodeId},
};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

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
    /// High-water mark published before PageCache accepts an extending write.
    /// Writeback can run from inside a cache write before `logical_size` is
    /// advanced, so the backend must treat this range as visible meanwhile.
    pub(crate) pending_write_end: Arc<AtomicUsize>,
    /// Packed seconds/nanoseconds published atomically by the write path.
    /// Dirty flags remain the publication guard for metadata readers.
    pub(crate) cached_mtime: AtomicU64,
    pub(crate) cached_ctime: AtomicU64,
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
    /// Namespace generation and complete directory snapshot shared by every
    /// VFS wrapper of the same ext4 inode generation.
    directory_generation: AtomicUsize,
    directory_snapshot: Mutex<Option<Arc<DirectorySnapshot>>>,
}

pub(crate) struct DirectorySnapshot {
    generation: usize,
    entries: Vec<(String, InodeId, FileType)>,
}

impl DirectorySnapshot {
    pub(crate) fn new(generation: usize, entries: Vec<(String, InodeId, FileType)>) -> Self {
        Self {
            generation,
            entries,
        }
    }

    pub(crate) fn find(&self, name: &str) -> Option<(InodeId, FileType)> {
        self.entries
            .iter()
            .find(|(entry_name, _, _)| entry_name == name)
            .map(|(_, inode_id, file_type)| (*inode_id, *file_type))
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect()
    }

    pub(crate) fn entries(&self) -> Vec<(String, InodeId, FileType)> {
        self.entries.clone()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CachedTimestamps {
    mtime: u64,
    ctime: u64,
    pub(crate) generation: usize,
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
            pending_write_end: Arc::new(AtomicUsize::new(0)),
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
            directory_generation: AtomicUsize::new(0),
            directory_snapshot: Mutex::new(None),
        }
    }

    pub(crate) fn directory_generation(&self) -> usize {
        self.directory_generation.load(Ordering::Acquire)
    }

    pub(crate) fn cached_directory_snapshot(&self) -> Option<Arc<DirectorySnapshot>> {
        let generation = self.directory_generation();
        self.directory_snapshot
            .lock()
            .as_ref()
            .filter(|snapshot| snapshot.generation == generation)
            .cloned()
    }

    pub(crate) fn publish_directory_snapshot(
        &self,
        generation: usize,
        snapshot: Arc<DirectorySnapshot>,
    ) -> Option<Arc<DirectorySnapshot>> {
        if self.directory_generation() != generation {
            return None;
        }
        let mut cached = self.directory_snapshot.lock();
        if self.directory_generation() != generation {
            return None;
        }
        if let Some(existing) = cached
            .as_ref()
            .filter(|existing| existing.generation == generation)
        {
            return Some(existing.clone());
        }
        *cached = Some(snapshot.clone());
        Some(snapshot)
    }

    pub(crate) fn invalidate_directory_snapshot(&self) {
        self.directory_generation.fetch_add(1, Ordering::AcqRel);
        self.directory_snapshot.lock().take();
    }

    pub(crate) fn cache_modified_time(&self, now: TimeSpec) {
        let packed = pack_timestamp(now);
        self.cached_ctime.store(packed, Ordering::Relaxed);
        self.cached_mtime.store(packed, Ordering::Relaxed);
        self.timestamp_generation.fetch_add(1, Ordering::Release);
        self.mtime_dirty.store(true, Ordering::Release);
        self.ctime_dirty.store(true, Ordering::Release);
    }

    pub(crate) fn dirty_timestamps(&self) -> Option<CachedTimestamps> {
        if !self.mtime_dirty.load(Ordering::Acquire) && !self.ctime_dirty.load(Ordering::Acquire) {
            return None;
        }
        Some(CachedTimestamps {
            mtime: self.cached_mtime.load(Ordering::Relaxed),
            ctime: self.cached_ctime.load(Ordering::Relaxed),
            generation: self.timestamp_generation.load(Ordering::Acquire),
        })
    }

    pub(crate) fn finish_timestamp_commit(&self, snapshot: CachedTimestamps) {
        if self
            .timestamp_generation
            .compare_exchange(snapshot.generation, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // Recheck after the CAS. A writer may have raced with the commit;
            // never clear the newer dirty snapshot as part of the old one.
            if self.timestamp_generation.load(Ordering::Acquire) == 0 {
                self.mtime_dirty.store(false, Ordering::Release);
                self.ctime_dirty.store(false, Ordering::Release);
            }
        }
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

    pub(crate) fn inode_flags(&self) -> InodeFlags {
        *self.inode_flags.lock()
    }

    pub(crate) fn set_inode_flags(&self, flags: InodeFlags) {
        *self.inode_flags.lock() = flags;
    }

    pub(crate) fn publish_pending_write_end(&self, end: usize) {
        self.pending_write_end.fetch_max(end, Ordering::AcqRel);
    }

    pub(crate) fn clear_pending_write_end(&self, end: usize) {
        let _ =
            self.pending_write_end
                .compare_exchange(end, 0, Ordering::AcqRel, Ordering::Acquire);
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
        if dirty_page_cache.is_some() {
            if !already_pinned {
                self.dirty_cache_pinned.store(true, Ordering::Release);
            }
            return;
        }
        if dirty_page_cache.is_none() {
            self.pin();
            *dirty_page_cache = Some(cache.clone());
            self.dirty_cache_pinned.store(true, Ordering::Release);
        }
    }

    pub(crate) fn release_dirty_page_cache(&self) {
        let mut dirty_page_cache = self.dirty_page_cache.lock();
        // The caller has already reset size_generation with a successful CAS.
        // Compare against the last released generation so a writer racing
        // after that CAS keeps the dirty cache pinned for the next sync.
        let generation = self.size_generation.load(Ordering::Acquire);
        if dirty_page_cache.is_some()
            && generation == self.last_release_generation.load(Ordering::Relaxed)
            && dirty_page_cache.take().is_some()
        {
            self.dirty_cache_pinned.store(false, Ordering::Release);
            self.unpin();
        }
    }

    /// Fast, filesystem-local predicate used to avoid replaying a clean
    /// instance during the compatibility registry pass of global sync(2).
    pub(crate) fn needs_sync(&self) -> bool {
        self.size_generation.load(Ordering::Acquire) != 0
            || self.dirty_timestamps().is_some()
            || self.dirty_cache_pinned.load(Ordering::Acquire)
            || self.reclaim.lock().is_some()
            || self.reclaim_error.lock().is_some()
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
    let seconds = u32::try_from(time.tv_sec).unwrap_or(u32::MAX);
    let nanoseconds = u32::try_from(time.tv_nsec).unwrap_or(u32::MAX);
    (u64::from(seconds) << 32) | u64::from(nanoseconds)
}

fn unpack_timestamp(timestamp: u64) -> TimeSpec {
    let seconds = usize::try_from(timestamp >> 32).unwrap_or(usize::MAX);
    let nanoseconds = u32::try_from(timestamp & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    TimeSpec {
        tv_sec: seconds,
        tv_nsec: usize::try_from(nanoseconds).unwrap_or(usize::MAX),
    }
}

impl Ext4FileSystem {
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

    /// Attach the exact generation selected by the namespace transaction.
    /// Looking up the replaced entry before rename/unlink races with another
    /// namespace mutation and can associate the one-shot handle with the
    /// wrong inode generation.
    pub(crate) fn attach_reclaim_handle(
        &self,
        handle: another_ext4::InodeReclaimHandle,
    ) -> Result<(), SyscallErr> {
        let inode_id = usize::try_from(handle.inode_id()).map_err(|_| SyscallErr::EFBIG)?;
        let key = InodeKey::new(self.fs_id(), inode_id, handle.generation());
        self.attach_reclaim(key, handle)
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
                    let cache = maybe_cache
                        .as_ref()
                        .cloned()
                        .or_else(|| lifetime.page_cache());
                    let mut commit_size = || {
                        let generation = lifetime.size_generation.load(Ordering::Acquire);
                        let timestamps = lifetime.dirty_timestamps();
                        if generation == 0 && timestamps.is_none() {
                            return Ok(());
                        }
                        let id = u32::try_from(key.inode_id()).map_err(|_| SyscallErr::EFBIG)?;
                        if generation != 0 {
                            let size = lifetime.logical_size.load(Ordering::Acquire);
                            self.run_metadata_operation(|| {
                                self.inner().commit_inode_size(id, size as u64, None)
                            })?;
                        }
                        if timestamps.is_some() {
                            if let Some(committed) =
                                self.commit_lifetime_timestamps(id, lifetime)?
                            {
                                committed_timestamps.push((lifetime.clone(), committed));
                            }
                        }
                        if generation != 0 {
                            committed_generations.push((lifetime.clone(), generation));
                        }
                        Ok(())
                    };
                    if let Some(cache) = cache {
                        cache.writeback_all_before_io_gate()?;
                        cache.with_io_gate(|| commit_size())?;
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
                if lifetime
                    .size_generation
                    .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
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
        // Validate the persistent chain once and consume a contiguous ready
        // prefix in head order. This turns the common final-sync case from one
        // full orphan walk per inode into one O(N) validation plus O(1) head
        // removals. Missing/open heads fall back to the generic safe path.
        let orphan_order = self
            .inner()
            .validated_orphan_reclaim_order()
            .map_err(|error| super::errno::from_another(error.code()))?;
        let orphan_rank: alloc::collections::BTreeMap<u32, usize> = orphan_order
            .iter()
            .copied()
            .enumerate()
            .map(|(rank, inode)| (inode, rank))
            .collect();
        let mut ready: Vec<(
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
        ready.sort_by_key(|(_, _, handle)| {
            orphan_rank
                .get(&handle.inode_id())
                .copied()
                .unwrap_or(usize::MAX)
        });
        let mut reclaimed = Vec::new();
        let mut failure = None;
        let mut expected_head = 0usize;
        for (key, lifetime, handle) in ready {
            if failure.is_some() {
                if let Some(error) = failure {
                    lifetime.restore_reclaim(handle, error);
                }
                continue;
            }
            let head_fast_path =
                orphan_order.get(expected_head).copied() == Some(handle.inode_id());
            let mut result = if head_fast_path {
                self.reclaim_inode_from_validated_head(handle)
            } else {
                self.reclaim_inode(handle)
            };
            if matches!(&result, Err((SyscallErr::EAGAIN, _))) {
                let (_, handle) = result.expect_err("checked reclaim result is an error");
                result = self.reclaim_inode(handle);
            }
            match result {
                Ok(()) => {
                    reclaimed.push(key);
                    if head_fast_path {
                        expected_head += 1;
                    }
                }
                Err((error, handle)) => {
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
    use alloc::{string::String, sync::Arc, vec};

    use super::{DirectorySnapshot, InodeLifetime};
    use crate::fs::vfs::FileType;
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

    #[test]
    fn stale_directory_scan_cannot_publish_after_invalidation() {
        let lifetime = InodeLifetime::new(0, TimeSpec::new(), TimeSpec::new());
        let generation = lifetime.directory_generation();
        let stale = Arc::new(DirectorySnapshot::new(
            generation,
            vec![(String::from("old"), 7, FileType::File)],
        ));

        lifetime.invalidate_directory_snapshot();

        assert!(lifetime
            .publish_directory_snapshot(generation, stale)
            .is_none());
        assert!(lifetime.cached_directory_snapshot().is_none());
    }
}
