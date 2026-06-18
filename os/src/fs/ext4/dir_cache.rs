#![allow(unused)]

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

struct PerDirCache {
    version: u64,
    entries: BTreeMap<String, u32>,
    last_access: u64,
}

pub struct Ext4DirectoryLookupCache {
    dirs: Mutex<BTreeMap<u32, PerDirCache>>,
    dir_versions: Mutex<BTreeMap<u32, u64>>,
    max_dirs: usize,
    max_entries_per_dir: usize,
    global_tick: AtomicU64,
}

impl Ext4DirectoryLookupCache {
    pub fn new() -> Self {
        Self {
            dirs: Mutex::new(BTreeMap::new()),
            dir_versions: Mutex::new(BTreeMap::new()),
            max_dirs: 128,
            max_entries_per_dir: 1024,
            global_tick: AtomicU64::new(0),
        }
    }

    /// Lookup a name in a directory's cache. Returns Some(child_ino) if found
    /// and the cached version matches the passed-in version.
    /// Updates last_access tick on hit. Returns None on miss or version mismatch.
    pub fn lookup(&self, parent_ino: u32, name: &str, expected_version: u64) -> Option<u32> {
        let mut dirs = self.dirs.lock();
        let per_dir = dirs.get_mut(&parent_ino)?;

        if per_dir.version != expected_version {
            return None;
        }

        let child_ino = per_dir.entries.get(name).copied();
        per_dir.last_access = self.global_tick.fetch_add(1, Ordering::Relaxed);
        child_ino
    }

    /// Insert or update a name→ino entry. Creates PerDirCache for parent_ino if needed.
    /// Handles per-dir overflow inline (while holding dirs lock), then releases
    /// and calls evict_if_needed() for global-LRU eviction.
    pub fn insert(&self, parent_ino: u32, name: &str, child_ino: u32, version: u64) {
        let tick = self.global_tick.fetch_add(1, Ordering::Relaxed);

        {
            let mut dirs = self.dirs.lock();
            let per_dir = dirs.entry(parent_ino).or_insert_with(|| PerDirCache {
                version: 0,
                entries: BTreeMap::new(),
                last_access: tick,
            });
            // FIX1: if version changed since cache was populated, discard ALL stale
            // entries BEFORE inserting the new one. This prevents a single-point
            // insert() from promoting a stale full-index (with deleted names) to
            // the current version.
            if per_dir.version != version {
                per_dir.entries.clear();
                per_dir.version = 0;
            }
            per_dir.entries.insert(String::from(name), child_ino);
            per_dir.version = version;
            per_dir.last_access = tick;

            // Per-dir overflow: clear this directory's cache while we hold the lock
            if per_dir.entries.len() > self.max_entries_per_dir {
                per_dir.entries.clear();
                per_dir.version = 0;
            }
        }

        // Drop dirs before evict_if_needed, which locks dir_versions first then dirs
        self.evict_if_needed();
    }

    /// Remove a single entry from a directory's cache.
    pub fn invalidate_name(&self, parent_ino: u32, name: &str) {
        let mut dirs = self.dirs.lock();
        if let Some(per_dir) = dirs.get_mut(&parent_ino) {
            per_dir.entries.remove(name);
        }
    }

    /// Clear ALL entries for a directory (but keep the version tracking).
    pub fn invalidate_dir(&self, parent_ino: u32) {
        let mut dirs = self.dirs.lock();
        if let Some(per_dir) = dirs.get_mut(&parent_ino) {
            per_dir.entries.clear();
            per_dir.version = 0;
        }
    }

    /// Completely remove a directory from both dirs and dir_versions maps.
    /// Used when a directory is deleted (rmdir).
    pub fn remove_dir_cache(&self, parent_ino: u32) {
        // Lock dir_versions first, then dirs (consistent ordering)
        let mut dir_versions = self.dir_versions.lock();
        let mut dirs = self.dirs.lock();
        dirs.remove(&parent_ino);
        dir_versions.remove(&parent_ino);
    }

    /// Increment the version for a directory. Returns the NEW version.
    pub fn bump_version(&self, parent_ino: u32) -> u64 {
        let mut dir_versions = self.dir_versions.lock();
        let version = dir_versions.entry(parent_ino).or_insert(0);
        *version += 1;
        *version
    }

    /// Get the current version for a directory. Returns 0 if not yet tracked.
    pub fn current_version(&self, parent_ino: u32) -> u64 {
        let dir_versions = self.dir_versions.lock();
        dir_versions.get(&parent_ino).copied().unwrap_or(0)
    }

    /// Bulk insert: replace ALL entries for a directory with the given list.
    /// Used by lazy full-index when a large directory is scanned.
    pub fn build_full_index(&self, parent_ino: u32, entries: Vec<(String, u32)>, version: u64) {
        // FIX2: recheck version BEFORE installing — discard stale index if directory
        // was modified during the scan (version changed).
        {
            let dir_versions = self.dir_versions.lock();
            let current = dir_versions.get(&parent_ino).copied().unwrap_or(0);
            if current != version {
                // Directory was modified during scan — discard the stale index
                return;
            }
        }

        let tick = self.global_tick.fetch_add(1, Ordering::Relaxed);

        {
            let mut dirs = self.dirs.lock();
            let new_entries: BTreeMap<String, u32> = entries.into_iter().collect();
            let per_dir = dirs.entry(parent_ino).or_insert_with(|| PerDirCache {
                version: 0,
                entries: BTreeMap::new(),
                last_access: tick,
            });
            per_dir.entries = new_entries;
            per_dir.version = version;
            per_dir.last_access = tick;

            if per_dir.entries.len() > self.max_entries_per_dir {
                per_dir.entries.clear();
                per_dir.version = 0;
            }

            // Entry count counter: tracks entries in the most recent full-index build.
            // Not a live gauge — only updated on build_full_index, not on incremental inserts/removes.
            crate::fs::ext4::counters::DIR_CACHE_ENTRY_COUNT.store(per_dir.entries.len() as u64, core::sync::atomic::Ordering::Relaxed);
        }

        self.evict_if_needed();
    }

    /// LRU eviction: if dirs.len() > max_dirs, find the directory with smallest
    /// last_access and remove it completely (from both dirs and dir_versions).
    /// If a PerDirCache's entries.len() > max_entries_per_dir, clear that
    /// directory's entries (but keep the entry in dirs).
    pub fn evict_if_needed(&self) {
        // Lock dir_versions first, then dirs (consistent ordering prevents deadlock)
        let mut dir_versions = self.dir_versions.lock();
        let mut dirs = self.dirs.lock();

        // (a) Global cap: evict least-recently-used directory entirely
        if dirs.len() > self.max_dirs {
            let lru_ino = dirs
                .iter()
                .min_by_key(|(_, cache)| cache.last_access)
                .map(|(&ino, _)| ino);

            if let Some(ino) = lru_ino {
                dirs.remove(&ino);
                dir_versions.remove(&ino);
            }
        }

        // (b) Per-dir cap: clear overflowing directory caches (keep the dirs entry)
        let overflowing: Vec<u32> = dirs
            .iter()
            .filter(|(_, c)| c.entries.len() > self.max_entries_per_dir)
            .map(|(&ino, _)| ino)
            .collect();

        for ino in overflowing {
            if let Some(per_dir) = dirs.get_mut(&ino) {
                per_dir.entries.clear();
                per_dir.version = 0;
            }
        }
        // Reset entry count if all caches cleared
        if dirs.is_empty() {
            crate::fs::ext4::counters::DIR_CACHE_ENTRY_COUNT.store(0, core::sync::atomic::Ordering::Relaxed);
        }
    }
}
