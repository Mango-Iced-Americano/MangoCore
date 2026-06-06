//! Minimal /sys/class/net/<ifname>/ materialiser — uses raw Inode ops on
//! the root MountFS (same approach as boot-time /dev creation in fs/mod.rs).

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;

use crate::net::iface::Iface;

pub fn register(iface: &Arc<dyn Iface>) {
    let name = iface.iface_name();
    let mfs = crate::fs::VFS_ROOT.clone();
    let root: Arc<dyn crate::fs::vfs::IndexNode> = mfs.mountpoint_root_inode();

    // Exactly the same pattern as mount_common_filesystems() in fs/mod.rs
    let sys = get_or_create_dir(&root, "sys");
    let class = get_or_create_dir(&sys, "class");
    let net = get_or_create_dir(&class, "net");
    let ifdir = get_or_create_dir(&net, &name);

    create_and_fill(&ifdir, "address", &format_mac(iface.mac()));
    create_and_fill(&ifdir, "mtu", &format!("{}\n", iface.mtu()));
}

fn format_mac(mac: [u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])
}

fn get_or_create_dir(parent: &Arc<dyn crate::fs::vfs::IndexNode>, name: &str)
    -> Arc<dyn crate::fs::vfs::IndexNode>
{
    // Same as: root.find("dev").unwrap_or_else(|_| root.create(...))
    // from fs/mod.rs:123-125
    parent.find(name).unwrap_or_else(|_| {
        parent.create(
            name,
            crate::fs::vfs::FileType::Dir,
            crate::fs::vfs::InodeMode::from_bits_truncate(0o755),
        ).unwrap_or_else(|e| {
            log::warn!("[sysfs] create dir {} failed: {:?}", name, e);
            parent.clone()
        })
    })
}

fn create_and_fill(dir: &Arc<dyn crate::fs::vfs::IndexNode>, name: &str, content: &str) {
    let file = dir.find(name).unwrap_or_else(|_| {
        dir.create(
            name,
            crate::fs::vfs::FileType::File,
            crate::fs::vfs::InodeMode::from_bits_truncate(0o444),
        ).unwrap_or_else(|e| {
            log::warn!("[sysfs] create file {} failed: {:?}", name, e);
            dir.clone()
        })
    });
    let _ = file.truncate(0);
    let _ = file.write_at(
        0, content.len(), content.as_bytes(),
        spin::Mutex::new(crate::fs::vfs::FilePrivateData::Unused).lock(),
    );
}
