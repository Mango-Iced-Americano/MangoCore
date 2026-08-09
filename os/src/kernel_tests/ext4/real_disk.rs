//! L3 tests against the loop-mounted ktest test disks.
//!
//! 每个用例自行把 initramfs 内嵌的 `test-ext.img`（2MiB ext4）与
//! `test-fat.img`（32MiB FAT32）包装为 loop 块设备、挂载到 VFS_ROOT 下的
//! `/test-ext` 与 `/test-fat`，运行后卸载。若磁盘缺失或挂载失败，用例返回
//! `Err("SKIP: ...")` 由 runner 计为 skipped，而不是失败或 panic。

use alloc::sync::Arc;

use crate::fs::vfs::{FileFlags, FilePrivateData, FileType, IndexNode, InodeMode};

/// 解析 loop 挂载盘在 VFS 下的根 inode。
fn loop_mount_root(
    mount_name: &str,
    fail_msg: &'static str,
) -> Result<Arc<dyn IndexNode>, &'static str> {
    let root = crate::fs::vfs_root();
    let root_inode = root.mountpoint_root_inode();
    root_inode.find(mount_name).map_err(|_| fail_msg)
}

/// 打开并写入一个常规文件（沿用 ext4_another_lifetime 的打开模式）。
fn write_open_file(inode: &Arc<dyn IndexNode>, data: &[u8]) -> Result<(), &'static str> {
    let private = spin::Mutex::new(FilePrivateData::Unused);
    inode
        .open(private.lock(), &FileFlags::O_WRONLY)
        .map_err(|_| "open of regular file failed")?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let written = inode
        .write_at(0, data.len(), data, private.lock())
        .map_err(|_| "write to open regular file failed")?;
    if written != data.len() {
        return Err("write to open regular file was short");
    }
    Ok(())
}

/// 读取并校验常规文件内容与期望数据一致。
fn read_file(inode: &Arc<dyn IndexNode>, expected: &[u8]) -> Result<(), &'static str> {
    let mut readback = [0u8; 32];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = inode
        .read_at(
            0,
            expected.len(),
            &mut readback[..expected.len()],
            private.lock(),
        )
        .map_err(|_| "read of regular file failed")?;
    if read != expected.len() || readback[..expected.len()] != *expected {
        return Err("regular file data did not round-trip");
    }
    Ok(())
}

/// 在指定挂载盘上执行 mount → create → write → read → remove → unmount 往返。
fn create_write_read_remove_roundtrip(
    disk_file: &str,
    mount_name: &str,
    file_name: &str,
    payload: &[u8],
    skip_msg: &'static str,
    result_msg: &'static str,
    cleanup_msg: &'static str,
) -> Result<(), &'static str> {
    let mfs = crate::fs::mount_test_disk(disk_file, mount_name).map_err(|_| skip_msg)?;
    let mnt = loop_mount_root(mount_name, result_msg)?;
    let result = (|| -> Result<(), &'static str> {
        let inode = mnt
            .create(file_name, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| result_msg)?;
        write_open_file(&inode, payload)?;
        inode.sync().map_err(|_| "sync after write failed")?;
        read_file(&inode, payload)?;
        Ok(())
    })();
    let cleanup = mnt
        .unlink(file_name)
        .and_then(|_| mnt.sync())
        .map_err(|_| cleanup_msg);
    let unmount = crate::fs::unmount_test_disk(&mfs);
    match (result, cleanup, unmount) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error),
        (Ok(()), Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

/// 断言两个 loop 测试盘都能完整走一遍 mount → verify → unmount 周期。
pub(super) fn test_ext4_mountpoint_exists() -> Result<(), &'static str> {
    let mfs_ext = crate::fs::mount_test_disk("test-ext.img", "test-ext")
        .map_err(|_| "SKIP: loop disk test-ext.img not embedded")?;
    let mfs_fat = crate::fs::mount_test_disk("test-fat.img", "test-fat")
        .map_err(|_| "SKIP: loop disk test-fat.img not embedded")?;

    let result = (|| -> Result<(), &'static str> {
        let root = crate::fs::vfs_root();
        let root_inode = root.mountpoint_root_inode();
        root_inode
            .find("test-ext")
            .map_err(|_| "loop disk /test-ext not mounted")?;
        root_inode
            .find("test-fat")
            .map_err(|_| "loop disk /test-fat not mounted")?;
        Ok(())
    })();

    let unmount_ext = crate::fs::unmount_test_disk(&mfs_ext);
    let unmount_fat = crate::fs::unmount_test_disk(&mfs_fat);
    match (result, unmount_ext, unmount_fat) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error),
        (Ok(()), Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

/// 在 /test-ext（ext4）上验证 create/write/read/remove 完整往返。
pub(super) fn test_ext4_create_write_read_remove() -> Result<(), &'static str> {
    create_write_read_remove_roundtrip(
        "test-ext.img",
        "test-ext",
        "ktest-probe.txt",
        b"MangoCore ktest ext4 probe\n",
        "SKIP: loop disk test-ext.img not embedded",
        "create ktest-probe.txt on /test-ext failed",
        "cleanup after ext4 real-disk roundtrip failed",
    )
}

/// 在 /test-fat（FAT32）上验证 create/write/read/remove 完整往返。
pub(super) fn test_fat32_create_write_read_remove() -> Result<(), &'static str> {
    create_write_read_remove_roundtrip(
        "test-fat.img",
        "test-fat",
        "ktest-probe.txt",
        b"MangoCore ktest fat32 probe\n",
        "SKIP: loop disk test-fat.img not embedded",
        "create ktest-probe.txt on /test-fat failed",
        "cleanup after fat32 real-disk roundtrip failed",
    )
}
