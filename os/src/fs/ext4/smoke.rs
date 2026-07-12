//! ext4 缓存 smoke test — boot 时自动运行，验证各缓存层是否生效。
//!
//! 调用: ext4::smoke::run_boot_smoke()
//! 输出: 创建 5 个 fast symlink → 重复 lookup ×20 → 重复 readlink ×10 → dump counters

use super::counters;
use crate::fs::vfs::{FilePrivateData, FileType, IndexNode as _};
use crate::println;

pub fn run_boot_smoke() {
    counters::enable_counters();
    counters::reset_counters();

    println!("[ext4_smoke] starting...");
    let root = crate::fs::vfs_root().mountpoint_root_inode();

    // 1) Create fast symlinks
    for i in 0..5 {
        let name = alloc::format!("boot_sym{}", i);
        let _ = root.symlink(&name, "/bin/busybox");
    }
    println!("[ext4_smoke] created 5 fast symlinks");

    // 2) Repeated lookup
    for _ in 0..20 {
        let _ = root.find("boot_sym0");
    }
    println!("[ext4_smoke] repeated lookup x20");

    // 3) Repeated readlink
    for _ in 0..10 {
        let inode = root.find("boot_sym0").ok();
        if let Some(ino) = inode {
            let mut buf = [0u8; 64];
            let _ = ino.read_at(
                0,
                buf.len(),
                &mut buf,
                spin::Mutex::new(FilePrivateData::Unused).lock(),
            );
        }
    }
    println!("[ext4_smoke] repeated readlink x10");

    // 4) Cleanup
    for i in 0..5 {
        let name = alloc::format!("boot_sym{}", i);
        let _ = root.unlink(&name);
    }

    counters::dump_scenario("boot_smoke");
    counters::disable_counters();
}
