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
    master_group_id: Mutex<u32>,
}

impl MountPropagation {
    /// Create a new private propagation (default).
    pub fn new_private() -> Self {
        Self {
            prop_type: Mutex::new(PropagationType::Private),
            peer_group_id: Mutex::new(0),
            master_group_id: Mutex::new(0),
        }
    }

    /// Create a new shared propagation with a newly allocated group ID.
    pub fn new_shared() -> Self {
        Self {
            prop_type: Mutex::new(PropagationType::Shared),
            peer_group_id: Mutex::new(allocate_group_id()),
            master_group_id: Mutex::new(0),
        }
    }

    /// Create propagation with a specific group ID.
    pub fn new_shared_with_group(group_id: u32) -> Self {
        Self {
            prop_type: Mutex::new(PropagationType::Shared),
            peer_group_id: Mutex::new(group_id),
            master_group_id: Mutex::new(0),
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
    pub fn master_group_id(&self) -> u32 {
        *self.master_group_id.lock()
    }
    pub fn set_peer_group_id(&self, id: u32) {
        *self.peer_group_id.lock() = id;
    }
    pub fn set_master_group_id(&self, id: u32) {
        *self.master_group_id.lock() = id;
    }

    /// Set the propagation type value WITHOUT managing registries.
    /// Use the free function `set_propagation_type()` for full lifecycle.
    pub(crate) fn set_prop_type_value(&self, t: PropagationType) {
        *self.prop_type.lock() = t;
    }

    /// Set shared with a specific group ID (used for propagation).
    /// Caller must handle peer registry (unregister old / register new).
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

    /// Global slave group registry: maps master group ID → weak references of
    /// slave mounts that receive propagation from that master.
    static ref SLAVE_GROUPS: Mutex<BTreeMap<u32, Vec<Weak<MountFS>>>> = Mutex::new(BTreeMap::new());
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
/// Only removes dead entries — does NOT remove the specific mount.
/// Use `unregister_peer_mount()` to remove a specific mount by identity.
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

/// Register a slave mount under a master group.
pub fn register_slave(mfs: &Arc<MountFS>, master_gid: u32) {
    if master_gid == 0 {
        return;
    }
    let mut groups = SLAVE_GROUPS.lock();
    let slaves = groups.entry(master_gid).or_default();
    slaves.retain(|w| {
        if let Some(m) = w.upgrade() {
            !Arc::ptr_eq(&m, mfs)
        } else {
            false
        }
    });
    slaves.push(Arc::downgrade(mfs));
}

/// Unregister a slave mount from its master group.
pub fn unregister_slave_mount(mfs: &Arc<MountFS>) {
    let master_gid = mfs.propagation().master_group_id();
    if master_gid == 0 {
        return;
    }
    let mut groups = SLAVE_GROUPS.lock();
    if let Some(slaves) = groups.get_mut(&master_gid) {
        slaves.retain(|w| w.upgrade().map_or(true, |a| !Arc::ptr_eq(&a, mfs)));
    }
}

/// Get all peers in a group, excluding the specified mount.
/// Filters to only Shared mounts with matching group_id.
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
                .filter(|a| a.propagation().is_shared() && a.propagation().peer_group_id() == gid)
                .collect()
        })
}

/// Get all slave mounts that receive propagation from a master group.
/// Cleans stale weak references on each lookup.
pub fn get_slaves(master_gid: u32) -> Vec<Arc<MountFS>> {
    if master_gid == 0 {
        return Vec::new();
    }
    let mut groups = SLAVE_GROUPS.lock();
    let slaves = groups.entry(master_gid).or_default();
    // Clean stale refs and filter to active Slave mounts with matching master
    slaves.retain(|w| {
        if let Some(m) = w.upgrade() {
            m.propagation().is_slave() && m.propagation().master_group_id() == master_gid
        } else {
            false
        }
    });
    slaves
        .iter()
        .filter_map(|w| w.upgrade())
        .collect()
}

// ============================================================================
// Owner-aware Propagation State Management
// ============================================================================

/// Change the propagation type of a mount, managing all registry transitions.
///
/// This is the canonical entry point for propagation type changes.
/// It handles:
/// - Shared → non-Shared: unregisters from PEER_GROUPS via unregister_peer_mount
/// - Slave → non-Slave: unregisters from SLAVE_GROUPS
/// - Non-Shared → Shared: allocates a new peer_group_id if needed
/// - Non-Slave → Slave: preserves master_group_id (caller must set it first)
pub fn set_propagation_type(mfs: &Arc<MountFS>, t: PropagationType) {
    let prop = mfs.propagation();
    let old_type = prop.prop_type();

    // Leaving Shared: remove from PEER_GROUPS and clear peer_group_id
    if old_type == PropagationType::Shared && t != PropagationType::Shared {
        unregister_peer_mount(mfs);
        prop.set_peer_group_id(0);
    }

    // Leaving Slave or retargeting Slave: unregister old master
    if old_type == PropagationType::Slave {
        let old_master = prop.master_group_id();
        if t != PropagationType::Slave {
            // Leaving Slave entirely
            unregister_slave_mount(mfs);
            prop.set_master_group_id(0);
        } else if old_master != prop.master_group_id() {
            // Slave→Slave with different master: retarget
            unregister_slave_mount(mfs);
        }
    }

    // Becoming Shared: allocate group ID if needed
    if t == PropagationType::Shared && prop.peer_group_id() == 0 {
        prop.set_peer_group_id(allocate_group_id());
    }

    prop.set_prop_type_value(t);
}

// ============================================================================
// Mount Propagation Functions
// ============================================================================

/// Propagate a mount event to all shared peers and slaves.
///
/// When a new mount is created under a shared mount point:
/// - Shared peers in the same group receive a shared replica
/// - Slave mounts receive a slave replica (receive-only)
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

    // Propagate to shared peers
    for peer in get_peers(source) {
        propagate_to_mount(&peer, mountpoint_id, new_child, child_name, source_child_group, false);
    }

    // Propagate to slaves (receive-only from this master group)
    let master_gid = source.propagation().peer_group_id();
    for slave in get_slaves(master_gid) {
        propagate_to_mount(&slave, mountpoint_id, new_child, child_name, source_child_group, true);
    }
}

/// Internal: propagate a single mount event to one target (peer or slave).
fn propagate_to_mount(
    target: &Arc<MountFS>,
    mountpoint_id: InodeId,
    new_child: &Arc<MountFS>,
    child_name: &str,
    source_child_group: u32,
    as_slave: bool,
) {
    let target_root = target.covered_root_inode();

    // Check root mount event — mountpoint_id matches target's root inner inode
    if let Ok(root_md) = target_root.inner_inode.metadata() {
        if root_md.inode_id == mountpoint_id {
            let mount_path = target.mount_path().unwrap_or_default();
            if let Ok(new_mount) = target_root.mount_subtree_inner(
                new_child.inner_filesystem(),
                new_child.root_inner_inode(),
                super::MountFlags::empty(),
                Some(mount_path),
                false,
            ) {
                finish_propagated_mount(&new_mount, source_child_group, as_slave);
            }
            return;
        }
    }

    // Fallback: find child directory matching child_name
    if !child_name.is_empty() {
        if let Ok(target_inode) = target_root.find(child_name) {
            if let Some(target_mfs_inode) =
                target_inode.as_any_ref().downcast_ref::<MountFSInode>()
            {
                let mount_path = alloc::format!(
                    "{}/{}",
                    target.mount_path().unwrap_or_default(),
                    child_name
                );
                if let Ok(new_mount) = target_mfs_inode.mount_subtree_inner(
                    new_child.inner_filesystem(),
                    new_child.root_inner_inode(),
                    super::MountFlags::empty(),
                    Some(mount_path),
                    false,
                ) {
                    finish_propagated_mount(&new_mount, source_child_group, as_slave);
                }
            }
        }
    }
}

/// Finish setting up a propagated mount's group membership.
fn finish_propagated_mount(new_mount: &Arc<MountFS>, source_child_group: u32, as_slave: bool) {
    if as_slave {
        new_mount.propagation().set_prop_type_value(PropagationType::Slave);
        new_mount.propagation().set_master_group_id(source_child_group);
        register_slave(new_mount, source_child_group);
    } else if source_child_group != 0 {
        new_mount.propagation().set_shared_with_group(source_child_group);
        register_peer(new_mount);
    }
}

/// Propagate an umount event to all shared peers and slaves.
///
/// Uses proper detach via umount_inner(false) instead of raw remove_mount
/// to ensure complete cleanup (MOUNT_LIST, peer/slave registry, caches).
pub fn propagate_umount(source: &Arc<MountFS>, mountpoint_id: InodeId) {
    if !source.propagation().is_shared() {
        return;
    }

    // Propagate to shared peers: find and detach the corresponding mount
    for peer in get_peers(source) {
        if let Some(child) = find_child_mount_by_id(&peer, mountpoint_id) {
            let _ = child.umount_inner(false);
        }
    }

    // Propagate to slaves
    let master_gid = source.propagation().peer_group_id();
    for slave in get_slaves(master_gid) {
        if let Some(child) = find_child_mount_by_id(&slave, mountpoint_id) {
            let _ = child.umount_inner(false);
        }
    }
}

/// Look up a child mount in a MountFS by its mountpoint inode_id.
fn find_child_mount_by_id(parent: &Arc<MountFS>, inode_id: InodeId) -> Option<Arc<MountFS>> {
    parent.mountpoints.lock().get(&inode_id).cloned()
}
