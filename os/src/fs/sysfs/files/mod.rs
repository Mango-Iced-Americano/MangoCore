use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use log::warn;

use crate::fs::sysfs::SysInode;
use crate::fs::vfs::InodeMode;
use crate::net::iface::Iface;
use crate::net::net_core::current_netns;
use crate::utils::error::SyscallErr;

pub mod diag;

pub fn register_all(root: &Arc<SysInode>) -> Result<(), SyscallErr> {
    let class_dir = root.add_dir_inner("class", InodeMode::from_bits_truncate(0o555))?;
    let net_dir = class_dir.add_dir_inner("net", InodeMode::from_bits_truncate(0o555))?;
    net_dir.set_hooks(net_class_find_hook, net_class_list_hook);
    warn!("[sysfs] register_all: /sys/class/net hooks installed");
    root.add_dir("block", InodeMode::from_bits_truncate(0o555))?;
    warn!("[sysfs] register_all: /sys/block created");

    #[cfg(feature = "perf_diag")]
    {
        let kernel_dir = root.add_dir_inner("kernel", InodeMode::from_bits_truncate(0o555))?;
        diag::register_all(&kernel_dir)?;
        warn!("[sysfs] register_all: /sys/kernel/stats and /sys/kernel/tracing created");
    }

    Ok(())
}

fn devices_all_ns() -> Vec<(String, Arc<dyn Iface>)> {
    let mut seen_ns = BTreeSet::new();
    let mut seen_dev = BTreeSet::new();
    let mut result = Vec::new();

    // Current namespace first
    {
        let ns = current_netns();
        let id = ns.id;
        seen_ns.insert(id);
        let list = ns.device_list.lock();
        for iface in list.values() {
            let nic = iface.nic_id();
            if seen_dev.insert(nic) {
                result.push((iface.iface_name(), iface.clone()));
            }
        }
    }

    // Copy Arc<NetNamespace> list under registry lock, release, then iterate
    let ns_list: Vec<Arc<crate::task::NetNamespace>> = {
        let reg = crate::task::net_namespace::ns_registry().lock();
        reg.values()
            .filter(|ns| seen_ns.insert(ns.id))
            .cloned()
            .collect()
    };
    for ns in ns_list {
        let list = ns.device_list.lock();
        for iface in list.values() {
            let nic = iface.nic_id();
            if seen_dev.insert(nic) {
                result.push((iface.iface_name(), iface.clone()));
            }
        }
    }

    let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
    warn!(
        "[sysfs] devices_all_ns() => {} devices: {:?}",
        result.len(),
        names
    );
    result
}

fn net_class_find_hook(inode: &SysInode, name: &str) -> Option<Arc<dyn crate::fs::vfs::IndexNode>> {
    warn!("[sysfs] net_class_find_hook: looking up '{}'", name);
    let all = devices_all_ns();
    let iface = all
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, i)| i.clone())?;
    warn!(
        "[sysfs] net_class_find_hook: FOUND '{}', creating dir",
        name
    );
    Some(create_iface_dir(inode, iface))
}

fn net_class_list_hook(_inode: &SysInode) -> Vec<String> {
    let all = devices_all_ns();
    let names: Vec<String> = all.into_iter().map(|(name, _)| name).collect();
    warn!(
        "[sysfs] net_class_list_hook: returning {} entries: {:?}",
        names.len(),
        names
    );
    names
}

fn create_iface_dir(
    parent: &SysInode,
    iface: Arc<dyn Iface>,
) -> Arc<dyn crate::fs::vfs::IndexNode> {
    let (parent_weak, fs_weak) = {
        let data = parent.inner.lock();
        (data.self_ref.clone(), data.fs.clone())
    };

    let iface_dir =
        SysInode::new_dir_wired(parent_weak, fs_weak, InodeMode::from_bits_truncate(0o555));

    let mac = iface.mac();
    let addr_str = format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
    );
    iface_dir
        .add_file_owned("address", InodeMode::from_bits_truncate(0o444), addr_str)
        .expect("sysfs: failed to create address file");

    iface_dir
        .add_file_owned(
            "mtu",
            InodeMode::from_bits_truncate(0o444),
            format!("{}\n", iface.mtu()),
        )
        .expect("sysfs: failed to create mtu file");

    iface_dir as Arc<dyn crate::fs::vfs::IndexNode>
}
