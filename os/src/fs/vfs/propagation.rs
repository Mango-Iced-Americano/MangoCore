//! Mount propagation management.
//!
//! Implements mount propagation semantics similar to Linux, supporting
//! shared, private, slave, and unbindable propagation types.
//!
//! Reference: DragonOS `kernel/src/process/namespace/propagation.rs`
//! Reference: https://www.kernel.org/doc/Documentation/filesystems/sharedsubtree.txt

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicU32, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use super::{InodeId, IndexNode, MountFS, MountFSInode};

// ============================================================================
// PropagationType
// ============================================================================

/// Defines the propagation type for mount points, controlling how mount events are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropagationType {
    /// Mount events do not propagate to or from this mount (default)
    Private,
    /// Mount events propagate bidirectionally with other mounts in the same peer group
    Shared,
    /// Mount events propagate from the master mount to this slave mount (one-way)
    Slave,
    /// Mount cannot be bind mounted and events do not propagate
    Unbindable,
}

// ============================================================================
// MountPropagation
// ============================================================================

/// Manages mount propagation state for a single mount point.
///
/// Tracks how mount events (mount, unmount) propagate between mount points
/// according to their propagation types.
#[derive(Debug)]
pub struct MountPropagation {
    prop_type: Mutex<PropagationType>,
    peer_group_id: Mutex<u32>,
}

impl MountPropagation {
    /// Create a new private propagation (default).
    pub fn new_private() -> Self {
        Self {
            prop_type: Mutex::new(PropagationType::Private),
            peer_group_id: Mutex::new(0),
        }
    }

    /// Create a new shared propagation with a newly allocated group ID.
    pub fn new_shared() -> Self {
        Self {
            prop_type: Mutex::new(PropagationType::Shared),
            peer_group_id: Mutex::new(allocate_group_id()),
        }
    }

    /// Create propagation with a specific group ID.
    pub fn new_shared_with_group(group_id: u32) -> Self {
        Self {
            prop_type: Mutex::new(PropagationType::Shared),
            peer_group_id: Mutex::new(group_id),
        }
    }

    // ── Accessors ────────────────────────────────────────────────────

    pub fn is_shared(&self) -> bool {
        *self.prop_type.lock() == PropagationType::Shared
    }
    pub fn is_slave(&self) -> bool {
        *self.prop_type.lock() == PropagationType::Slave
    }
    pub fn is_unbindable(&self) -> bool {
        *self.prop_type.lock() == PropagationType::Unbindable
    }
    pub fn is_private(&self) -> bool {
        *self.prop_type.lock() == PropagationType::Private
    }
    pub fn prop_type(&self) -> PropagationType {
        *self.prop_type.lock()
    }
    pub fn peer_group_id(&self) -> u32 {
        *self.peer_group_id.lock()
    }
    pub fn set_peer_group_id(&self, id: u32) {
        *self.peer_group_id.lock() = id;
    }

    // ── Mutation ────────────────────────────────────────────────────

    /// Change propagation type.
    ///
    /// If transitioning FROM shared, unregisters from the peer group first.
    /// If transitioning TO shared, allocates a new group ID if needed.
    pub fn set_type(&self, t: PropagationType) {
        let mut pt = self.prop_type.lock();
        if *pt == PropagationType::Shared && t != PropagationType::Shared {
            let gid = *self.peer_group_id.lock();
            if gid != 0 {
                unregister_peer(gid);
            }
        }
        *pt = t;
        if t == PropagationType::Shared && *self.peer_group_id.lock() == 0 {
            *self.peer_group_id.lock() = allocate_group_id();
        }
    }

    /// Set shared with a specific group ID (used for propagation).
    pub fn set_shared_with_group(&self, group_id: u32) {
        let mut pt = self.prop_type.lock();
        *pt = PropagationType::Shared;
        drop(pt);
        *self.peer_group_id.lock() = group_id;
    }
}

/// Allocate a new unique peer group ID.
fn allocate_group_id() -> u32 {
    /// Global peer group ID counter. Starts from 1 (0 = invalid).
    static NEXT_ID: AtomicU32 = AtomicU32::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

// ============================================================================
// Peer Group Registry
// ============================================================================

lazy_static! {
    /// Global peer group registry: maps group ID → weak references of mounts.
    /// Weak references are used to avoid preventing mount cleanup.
    static ref PEER_GROUPS: Mutex<BTreeMap<u32, Vec<Weak<MountFS>>>> = Mutex::new(BTreeMap::new());
}

/// Register a mount in a peer group.
///
/// Cleans up stale references and checks for duplicates before adding.
pub fn register_peer(mfs: &Arc<MountFS>) {
    let gid = mfs.propagation().peer_group_id();
    if gid == 0 {
        return;
    }
    let mut groups = PEER_GROUPS.lock();
    let peers = groups.entry(gid).or_default();
    // Clean stale and check duplicates
    peers.retain(|w| {
        if let Some(m) = w.upgrade() {
            !Arc::ptr_eq(&m, mfs)
        } else {
            false
        }
    });
    peers.push(Arc::downgrade(mfs));
}

/// Unregister a mount from its peer group by group ID.
fn unregister_peer(gid: u32) {
    if gid == 0 {
        return;
    }
    let mut groups = PEER_GROUPS.lock();
    if let Some(peers) = groups.get_mut(&gid) {
        peers.retain(|w| w.upgrade().is_some());
    }
}

/// Unregister a specific mount from its peer group.
pub fn unregister_peer_mount(mfs: &Arc<MountFS>) {
    let gid = mfs.propagation().peer_group_id();
    if gid == 0 {
        return;
    }
    let mut groups = PEER_GROUPS.lock();
    if let Some(peers) = groups.get_mut(&gid) {
        peers.retain(|w| w.upgrade().map_or(true, |a| !Arc::ptr_eq(&a, mfs)));
    }
}

/// Get all peers in a group, excluding the specified mount.
pub fn get_peers(mfs: &Arc<MountFS>) -> Vec<Arc<MountFS>> {
    let gid = mfs.propagation().peer_group_id();
    if gid == 0 {
        return Vec::new();
    }
    let groups = PEER_GROUPS.lock();
    groups
        .get(&gid)
        .map_or(Vec::new(), |peers| {
            peers
                .iter()
                .filter_map(|w| w.upgrade())
                .filter(|a| !Arc::ptr_eq(a, mfs))
                .collect()
        })
}

// ============================================================================
// Mount Propagation Functions
// ============================================================================

/// Propagate a mount event to all peers.
///
/// When a new mount is created under a shared mount point, this function
/// propagates the mount to all peers in the same group.
pub fn propagate_mount(
    source: &Arc<MountFS>,
    mountpoint_id: InodeId,
    new_child: &Arc<MountFS>,
    child_name: &str,
) {
    if !source.propagation().is_shared() {
        return;
    }

    let source_child_group = new_child.propagation().peer_group_id();

    for peer in get_peers(source) {
        let peer_root = peer.covered_root_inode();

        // Check if this is a root mount event — mountpoint_id matches the
        // peer's root inner inode (e.g., bind-mounting directly onto a
        // shared mount's root). Propagate to the peer root itself.
        if let Ok(root_md) = peer_root.inner_inode.metadata() {
            if root_md.inode_id == mountpoint_id {
                let mount_path = peer.mount_path().unwrap_or_default();
                if let Ok(new_mount) = peer_root.mount_subtree_inner(
                    new_child.inner_filesystem(),
                    new_child.root_inner_inode(),
                    super::MountFlags::empty(),
                    Some(mount_path),
                    false,
                ) {
                    if source_child_group != 0 {
                        new_mount
                            .propagation()
                            .set_shared_with_group(source_child_group);
                        register_peer(&new_mount);
                    }
                }
                continue;
            }
        }

        // Fallback: find a child directory matching child_name in the peer
        if !child_name.is_empty() {
            if let Ok(target_inode) = peer_root.find(child_name) {
                if let Some(target_mfs_inode) =
                    target_inode.as_any_ref().downcast_ref::<MountFSInode>()
                {
                    let mount_path = alloc::format!(
                        "{}/{}",
                        peer.mount_path().unwrap_or_default(),
                        child_name
                    );
                    if let Ok(new_mount) = target_mfs_inode.mount_subtree_inner(
                        new_child.inner_filesystem(),
                        new_child.root_inner_inode(),
                        super::MountFlags::empty(),
                        Some(mount_path),
                        false,
                    ) {
                        if source_child_group != 0 {
                            new_mount
                                .propagation()
                                .set_shared_with_group(source_child_group);
                            register_peer(&new_mount);
                        }
                    }
                }
            }
        }
    }
}

/// Propagate an umount event to all peers.
///
/// When a mount is unmounted from a shared mount point, this function
/// propagates the umount to all peers in the same group.
pub fn propagate_umount(source: &Arc<MountFS>, mountpoint_id: InodeId) {
    if !source.propagation().is_shared() {
        return;
    }
    for peer in get_peers(source) {
        peer.remove_mount(mountpoint_id);
    }
}
