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
    /// Shared AND Slave: propagates with peers AND receives from master.
    /// Created by `--make-shared` on a Slave, or bind from SharedSlave source.
    SharedSlave,
    /// Mount cannot be bind mounted and events do not propagate
    Unbindable,
}

impl PropagationType {
    pub fn is_shared(self) -> bool {
        matches!(self, Self::Shared | Self::SharedSlave)
    }
    pub fn is_slave(self) -> bool {
        matches!(self, Self::Slave | Self::SharedSlave)
    }
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
        self.prop_type().is_shared()
    }
    pub fn is_slave(&self) -> bool {
        self.prop_type().is_slave()
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
    /// Preserves existing Slave state (SharedSlave remains SharedSlave).
    pub fn set_shared_with_group(&self, group_id: u32) {
        let mut pt = self.prop_type.lock();
        *pt = match *pt {
            PropagationType::Slave | PropagationType::SharedSlave => PropagationType::SharedSlave,
            _ => PropagationType::Shared,
        };
        drop(pt);
        *self.peer_group_id.lock() = group_id;
    }
}

/// Allocate a new unique peer group ID.
pub(crate) fn allocate_group_id() -> u32 {
    /// Global peer group ID counter. Starts from 1 (0 = invalid).
    static NEXT_ID: AtomicU32 = AtomicU32::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Set propagation state WITHOUT registering in global registries.
/// peer_gid: Some(id) enables Shared; master_gid: Some(id) enables Slave.
/// When both are Some, the mount becomes SharedSlave.
fn set_propagation_state_no_register(
    mnt_fs: &Arc<MountFS>,
    peer_gid: Option<u32>,
    master_gid: Option<u32>,
) {
    mnt_fs.propagation().set_peer_group_id(peer_gid.unwrap_or(0));
    mnt_fs.propagation().set_master_group_id(master_gid.unwrap_or(0));

    let prop_type = match (peer_gid, master_gid) {
        (Some(_), Some(_)) => PropagationType::SharedSlave,
        (Some(_), None) => PropagationType::Shared,
        (None, Some(_)) => PropagationType::Slave,
        (None, None) => PropagationType::Private,
    };
    mnt_fs.propagation().set_prop_type_value(prop_type);
}

/// Re-register based on current propagation state.
/// Must be called after propagation is complete to avoid self-peer loops.
pub fn register_current_propagation(mnt_fs: &Arc<MountFS>) {
    if mnt_fs.propagation().is_shared() {
        register_peer(mnt_fs);
    }
    if mnt_fs.propagation().is_slave() {
        let master = mnt_fs.propagation().master_group_id();
        if master != 0 {
            register_slave(mnt_fs, master);
        }
    }
}

/// Configure propagation state without registering. Caller must call
/// `register_current_propagation()` after propagation is complete.
pub fn configure_propagation_no_register(
    mnt_fs: &Arc<MountFS>,
    peer_gid: Option<u32>,
    master_gid: Option<u32>,
) {
    unregister_peer_mount(mnt_fs);
    unregister_slave_mount(mnt_fs);
    set_propagation_state_no_register(mnt_fs, peer_gid, master_gid);
}

/// Unified propagation state installer. Clears old registrations, sets
/// propagation type + peer group + master group, then registers.
/// peer_gid: Some(id) enables Shared; master_gid: Some(id) enables Slave.
/// When both are Some, the mount becomes SharedSlave.
pub fn install_propagation(
    mnt_fs: &Arc<MountFS>,
    peer_gid: Option<u32>,
    master_gid: Option<u32>,
) {
    unregister_peer_mount(mnt_fs);
    unregister_slave_mount(mnt_fs);
    set_propagation_state_no_register(mnt_fs, peer_gid, master_gid);
    register_current_propagation(mnt_fs);
}

/// Set a mount as Shared with a freshly allocated peer group ID.
/// Used when a mount event occurs under a shared parent — the new
/// child mount must form its own peer group, not join the parent's.
pub fn set_shared_new_group(mnt_fs: &Arc<MountFS>) {
    mnt_fs.propagation().set_shared_with_group(allocate_group_id());
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
        if peers.is_empty() {
            groups.remove(&gid);
        }
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
        if peers.is_empty() {
            groups.remove(&gid);
        }
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
        if slaves.is_empty() {
            groups.remove(&master_gid);
        }
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
/// Semantics:
///   make-shared on Slave          → SharedSlave (keeps master, new peer group)
///   make-shared on Shared         → Shared (new peer group)
///   make-shared on SharedSlave    → SharedSlave (keeps master, new peer group)
///   make-shared on Private        → Shared (new peer group)
///   make-slave on Shared          → Slave (master = old peer group)
///   make-slave on SharedSlave     → Slave (master = old master, drops peer)
///   make-slave on Slave           → Slave (master = new, caller sets before)
///   make-private / unbindable     → clear all
pub fn set_propagation_type(mfs: &Arc<MountFS>, t: PropagationType) {
    let prop = mfs.propagation();
    let old_type = prop.prop_type();
    let old_peer = prop.peer_group_id();
    let old_master = prop.master_group_id();

    match t {
        PropagationType::Shared => {
            // Preserve slave state: Slave+Shared = SharedSlave
            let master = if old_type.is_slave() { Some(old_master) } else { None };
            unregister_peer_mount(mfs);
            unregister_slave_mount(mfs);
            let new_peer = allocate_group_id();
            set_propagation_state_no_register(mfs, Some(new_peer), master);
            register_current_propagation(mfs);
        }
        PropagationType::Slave => {
            // Master = old peer group if old was Shared/SharedSlave
            let master = if old_type.is_shared() { old_peer }
                else { old_master };
            unregister_peer_mount(mfs);
            unregister_slave_mount(mfs);
            let new_master = if master != 0 { Some(master) } else { None };
            set_propagation_state_no_register(mfs, None, new_master);
            prop.set_master_group_id(new_master.unwrap_or(0));
            register_current_propagation(mfs);
        }
        PropagationType::Private | PropagationType::Unbindable => {
            unregister_peer_mount(mfs);
            unregister_slave_mount(mfs);
            set_propagation_state_no_register(mfs, None, None);
            if t == PropagationType::Unbindable {
                prop.set_prop_type_value(PropagationType::Unbindable);
            }
        }
        _ => {
            // SharedSlave — not reachable via external API, treat as Shared
            unregister_peer_mount(mfs);
            unregister_slave_mount(mfs);
            let new_peer = allocate_group_id();
            set_propagation_state_no_register(mfs, Some(new_peer), Some(old_master));
            register_current_propagation(mfs);
        }
    }
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
    const MAX_DEPTH: usize = 32;
    let mut visited: Vec<usize> = Vec::new();
    visited.push(Arc::as_ptr(source) as usize);
    propagate_mount_inner(source, mountpoint_id, new_child, child_name, &mut visited, MAX_DEPTH);
}

fn propagate_mount_inner(
    source: &Arc<MountFS>,
    mountpoint_id: InodeId,
    new_child: &Arc<MountFS>,
    child_name: &str,
    visited: &mut Vec<usize>,
    max_depth: usize,
) {
    if !source.propagation().is_shared() {
        return;
    }
    if visited.len() > max_depth {
        log::warn!("propagate_mount_inner: max depth {} exceeded", max_depth);
        return;
    }

    let source_child_group = new_child.propagation().peer_group_id();

    // Propagate to shared peers
    for peer in get_peers(source) {
        propagate_to_mount(&peer, mountpoint_id, new_child, child_name, source_child_group, false);
    }

    // Propagate to slaves. SharedSlave receivers forward to their own peers.
    let master_gid = source.propagation().peer_group_id();
    for slave in get_slaves(master_gid) {
        let slave_ptr = Arc::as_ptr(&slave) as usize;
        if visited.contains(&slave_ptr) {
            continue;
        }
        let created = propagate_to_mount(
            &slave, mountpoint_id, new_child, child_name, source_child_group, true,
        );
        // If slave is SharedSlave and a mount was created, recurse
        if let Some(ref created_mount) = created {
            if slave.propagation().is_shared() {
                visited.push(slave_ptr);
                propagate_mount_inner(
                    &slave, mountpoint_id, created_mount, child_name, visited, max_depth,
                );
            }
        }
    }
}

/// Internal: propagate a single mount event to one target (peer or slave).
/// Returns the created mount if successful.
fn propagate_to_mount(
    target: &Arc<MountFS>,
    mountpoint_id: InodeId,
    new_child: &Arc<MountFS>,
    child_name: &str,
    source_child_group: u32,
    as_slave: bool,
) -> Option<Arc<MountFS>> {
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
                finish_propagated_mount(&new_mount, target, source_child_group, as_slave);
                return Some(new_mount);
            }
            return None;
        }
    }

    // Fallback: find child directory matching child_name.
    if !child_name.is_empty() {
        if let Ok(inner_inode) = target_root.inner_inode.find(child_name) {
            if let Ok(md) = inner_inode.metadata() {
                let ino = md.inode_id;
                let mount_path = alloc::format!(
                    "{}/{}",
                    target.mount_path().unwrap_or_default(),
                    child_name
                );

                let new_mount = MountFS::new_with_root(
                    new_child.inner_filesystem(),
                    new_child.root_inner_inode(),
                    super::MountFlags::empty(),
                );
                let backref = MountFSInode::new(inner_inode, Arc::clone(target));
                new_mount.set_self_mountpoint(Some(backref));
                new_mount.set_mount_path(Some(mount_path.clone()));
                super::mount::MOUNT_LIST.insert(mount_path.as_str(), new_mount.clone(), Some(ino));

                // Use add_mount (not overmount_and_add) to avoid orphaning an
                // existing mount at this inode. If the inode is already occupied,
                // another peer already propagated here — skip gracefully.
                if target.add_mount(ino, new_mount.clone()).is_err() {
                    // Clean up MOUNT_LIST entry for failed propagation
                    super::mount::MOUNT_LIST.remove_fs(&new_mount);
                    new_mount.set_self_mountpoint(None);
                    return None;
                }

                finish_propagated_mount(&new_mount, target, source_child_group, as_slave);
                return Some(new_mount);
            }
        }
    }
    None
}

/// Finish setting up a propagated mount's group membership.
/// When as_slave=true and target_parent is shared, the clone becomes
/// SharedSlave so it can forward mount events to its own peers.
fn finish_propagated_mount(
    new_mount: &Arc<MountFS>,
    target_parent: &Arc<MountFS>,
    source_child_group: u32,
    as_slave: bool,
) {
    unregister_peer_mount(new_mount);
    unregister_slave_mount(new_mount);
    new_mount.propagation().set_peer_group_id(0);
    new_mount.propagation().set_master_group_id(0);

    if as_slave {
        let peer_gid = if target_parent.propagation().is_shared() {
            Some(allocate_group_id())
        } else {
            None
        };
        let master_gid = if source_child_group != 0 {
            Some(source_child_group)
        } else {
            None
        };
        set_propagation_state_no_register(new_mount, peer_gid, master_gid);
        register_current_propagation(new_mount);
    } else if source_child_group != 0 {
        new_mount
            .propagation()
            .set_shared_with_group(source_child_group);
        register_peer(new_mount);
    } else {
        new_mount
            .propagation()
            .set_prop_type_value(PropagationType::Private);
    }
}

/// Propagate an umount event to all shared peers and slaves.
///
/// DragonOS-style: lookup by InodeId only; silently skip if the child mount
/// is not found on a peer (it may not have been propagated there yet).
pub fn propagate_umount(source: &Arc<MountFS>, mountpoint_id: InodeId) {
    const MAX_DEPTH: usize = 32;
    let mut visited: Vec<usize> = Vec::new();
    visited.push(Arc::as_ptr(source) as usize);
    propagate_umount_inner(source, mountpoint_id, &mut visited, MAX_DEPTH);
}

fn propagate_umount_inner(
    source: &Arc<MountFS>,
    mountpoint_id: InodeId,
    visited: &mut Vec<usize>,
    max_depth: usize,
) {
    if !source.propagation().is_shared() {
        return;
    }
    if visited.len() > max_depth {
        log::warn!("propagate_umount_inner: max depth {} exceeded", max_depth);
        return;
    }

    for peer in get_peers(source) {
        // DragonOS umount_at_peer: remove child by InodeId + narrow cleanup.
        // Silently skip if not found (peer may not have received propagation).
        if let Some(child) = find_child_mount_by_id(&peer, mountpoint_id) {
            child.umount_at_peer();
        }
    }

    let master_gid = source.propagation().peer_group_id();
    for slave in get_slaves(master_gid) {
        let slave_ptr = Arc::as_ptr(&slave) as usize;
        if visited.contains(&slave_ptr) {
            continue;
        }
        if let Some(child) = find_child_mount_by_id(&slave, mountpoint_id) {
            child.umount_at_peer();
        }
        if slave.propagation().is_shared() {
            visited.push(slave_ptr);
            propagate_umount_inner(&slave, mountpoint_id, visited, max_depth);
        }
    }
}

/// Look up a child mount in a MountFS by its mountpoint inode_id.
fn find_child_mount_by_id(parent: &Arc<MountFS>, inode_id: InodeId) -> Option<Arc<MountFS>> {
    parent.mountpoints.lock().get(&inode_id).cloned()
}
