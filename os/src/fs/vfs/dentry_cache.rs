//! Dentry cache — bounded strong-reference cache for VFS lookups.
//!
//! Caches (parent_inode_id, name) → Arc<MountFSInode> mappings to avoid
//! repeated filesystem lookups for frequently accessed paths.
//!
//! Uses CLOCK approximate-LRU eviction: each entry has a `referenced` bit;
//! on hit the bit is set; eviction gives one second chance to referenced
//! entries before removing cold ones.
//!
//! Design rules:
//! - Only caches COVERED dentries (before mount-point overlay).
//!   Overlay is applied after cache retrieval.
//! - Never holds the cache lock while calling filesystem methods.
//! - Evicted Arcs are dropped outside the lock to avoid re-entrancy.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use super::mount::MountFSInode;

/// Maximum number of entries in the dentry cache.
/// Small enough not to break lifecycle tests (64-file lookup → delta ≤ 16),
/// large enough for typical working sets.
pub const DENTRY_CACHE_LIMIT: usize = 256;

/// Cache key: (parent inode id, child name).
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct DentryKey {
    pub parent_ino: usize,
    pub name: String,
}

struct CacheEntry {
    node: Arc<MountFSInode>,
    referenced: bool,
}

pub struct DentryCache {
    map: BTreeMap<DentryKey, CacheEntry>,
    order: VecDeque<DentryKey>,
}

impl fmt::Debug for DentryCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DentryCache")
            .field("len", &self.map.len())
            .field("limit", &DENTRY_CACHE_LIMIT)
            .finish()
    }
}

impl DentryCache {
    pub const fn new() -> Self {
        DentryCache {
            map: BTreeMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Look up a cached entry. On hit, sets `referenced = true`.
    pub fn get(&mut self, key: &DentryKey) -> Option<Arc<MountFSInode>> {
        let entry = self.map.get_mut(key)?;
        entry.referenced = true;
        Some(entry.node.clone())
    }

    /// Insert a new entry, or return the existing one if already present
    /// (handling concurrent duplicate insertions).
    ///
    /// Returns the canonical `Arc` and a list of evicted entries.
    /// The caller MUST drop evicted entries outside the cache lock.
    pub fn insert_or_get(
        &mut self,
        key: DentryKey,
        node: Arc<MountFSInode>,
    ) -> (Arc<MountFSInode>, Vec<Arc<MountFSInode>>) {
        if let Some(entry) = self.map.get_mut(&key) {
            entry.referenced = true;
            return (entry.node.clone(), Vec::new());
        }

        self.map.insert(
            key.clone(),
            CacheEntry {
                node: node.clone(),
                referenced: true,
            },
        );
        self.order.push_back(key);

        let mut evicted = Vec::new();
        self.evict_locked(&mut evicted);
        (node, evicted)
    }

    /// Invalidate a single cache entry. Returns the evicted `Arc` if present.
    pub fn invalidate(&mut self, key: &DentryKey) -> Option<Arc<MountFSInode>> {
        // Remove from order (O(n) for small n, acceptable for kernel)
        if let Some(pos) = self.order.iter().position(|k| *k == *key) {
            self.order.remove(pos);
        }
        self.map.remove(key).map(|entry| entry.node)
    }

    /// Invalidate ALL cached children of a given parent inode.
    /// Used when removing a directory (rmdir).
    pub fn clear_parent(&mut self, parent_ino: usize) {
        let keys_to_remove: Vec<DentryKey> = self
            .map
            .keys()
            .filter(|k| k.parent_ino == parent_ino)
            .cloned()
            .collect();
        for key in keys_to_remove {
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                self.order.remove(pos);
            }
            self.map.remove(&key);
        }
    }

/// Clear ALL entries. Used during umount.
    #[must_use]
    pub fn clear_all(&mut self) -> Vec<Arc<MountFSInode>> {
        self.order.clear();
        let evicted: Vec<Arc<MountFSInode>> = core::mem::take(&mut self.map)
            .into_iter()
            .map(|(_k, v)| v.node)
            .collect();
        self.map = alloc::collections::BTreeMap::new();
        evicted
    }

    /// CLOCK eviction: remove cold entries until within limit.
    fn evict_locked(&mut self, evicted: &mut Vec<Arc<MountFSInode>>) {
        while self.map.len() > DENTRY_CACHE_LIMIT {
            let Some(key) = self.order.pop_front() else {
                break;
            };

            let Some(entry) = self.map.get_mut(&key) else {
                continue;
            };

            if entry.referenced {
                // Give a second chance: clear referenced and move to back
                entry.referenced = false;
                self.order.push_back(key);
                continue;
            }

            // Cold entry: evict
            if let Some(entry) = self.map.remove(&key) {
                evicted.push(entry.node);
            }
        }
    }
}
