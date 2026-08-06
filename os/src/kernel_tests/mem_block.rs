//! 零盘 ktest 的内存块设备 fixture。
//!
//! 提供可在任意测试组间复用的内存 `BlockDevice`：
//! - `MemBlockDevice`：有界、越界读零、可整体替换内容的块设备（ext4/fat32
//!   零盘测试共用）；
//! - `from_initramfs_file`：把 initramfs 内的一个文件（如 `/tools/test-fat.img`）
//!   读入内存块设备，使 SMP 并发测试能挂载真实格式化的 FAT32/ext4 卷而不依赖
//!   外部工具盘。

use alloc::{sync::Arc, vec, vec::Vec};
use spin::Mutex;

use crate::drivers::block::BlockDevice;
use crate::hal::BLOCK_SZ;

/// 有界、零扩展的内存块设备；越界读返回 0，越界写被忽略。
pub struct MemBlockDevice {
    data: Mutex<Vec<u8>>,
    size: u64,
}

impl MemBlockDevice {
    pub fn new(size_bytes: usize) -> Self {
        Self {
            data: Mutex::new(alloc::vec![0u8; size_bytes]),
            size: size_bytes as u64,
        }
    }

    /// 用镜像字节整体替换设备内容；镜像超过设备容量时拒绝。
    pub fn load_image(&self, image: &[u8]) -> Result<(), &'static str> {
        let mut data = self.data.lock();
        if image.len() > data.len() {
            return Err("mem block image exceeds device capacity");
        }
        data[..image.len()].copy_from_slice(image);
        Ok(())
    }
}

impl BlockDevice for MemBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let offset = block_id * BLOCK_SZ;
        let data = self.data.lock();
        if offset >= data.len() {
            buf.fill(0);
            return;
        }
        let end = core::cmp::min(offset + buf.len(), data.len());
        buf[..end - offset].copy_from_slice(&data[offset..end]);
        buf[end - offset..].fill(0);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let offset = block_id * BLOCK_SZ;
        let mut data = self.data.lock();
        if offset >= data.len() {
            return;
        }
        let end = core::cmp::min(offset + buf.len(), data.len());
        data[offset..end].copy_from_slice(&buf[..end - offset]);
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size)
    }
}

/// 从 initramfs 绝对路径读文件字节（如 `/tools/test-fat.img`），装入新的内存块设备。
///
/// 该路径在 ktest 零盘拓扑下可用：initramfs 通过 `.incbin` 链接进内核并解包到
/// VFS_ROOT，`vfs_lookup_absolute` 即可访问。设备容量取 `max(image.len(), 1 MiB)`，
/// 保证 FAT 挂载时足够的头部空间。
pub fn from_initramfs_file(path: &str) -> Result<Arc<MemBlockDevice>, &'static str> {
    use crate::fs::vfs::{File, FileFlags};

    let inode = crate::fs::vfs_lookup_absolute(path).map_err(|_| "initramfs fixture file missing")?;
    let file = File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open initramfs fixture")?;
    let size = file
        .metadata()
        .map(|meta| meta.size.max(0) as usize)
        .map_err(|_| "failed to stat initramfs fixture")?;
    let mut image = Vec::new();
    image
        .try_reserve_exact(size)
        .map_err(|_| "initramfs fixture too large")?;
    image.resize(size, 0);
    let read = file.read(&mut image).map_err(|_| "failed to read initramfs fixture")?;
    if read != size {
        return Err("initramfs fixture short read");
    }
    let capacity = core::cmp::max(size, 1024 * 1024);
    let device = Arc::new(MemBlockDevice::new(capacity));
    device.load_image(&image)?;
    Ok(device)
}
