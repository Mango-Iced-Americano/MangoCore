//! MangoCore BlockDevice → lwext4 KernelDevOp bridge.
//!
//! Translates byte-level seek+read/write (from lwext4 C library) into
//! block_id-based read_block/write_block (MangoCore BlockDevice trait).
//! MangoCore blocks use the platform `BLOCK_SZ`; lwext4 device blocks are 512 bytes.
//!
//! lwext4 C code calls:
//!   - dev_bread: seek(blk_id * 512, SEEK_SET) → read(buf, blk_cnt * 512)
//!   - dev_bwrite: seek(blk_id * 512, SEEK_SET) → write(buf, blk_cnt * 512)
//!   - dev_open: seek(0, SEEK_END) to determine device size
//!
//! Most lwext4 I/O is naturally aligned to MangoCore block boundaries. The
//! bridge handles partial head/tail blocks with bounce buffers and forwards
//! every aligned middle run as one multi-block request.

use crate::drivers::block::BlockDevice;
use crate::hal::BLOCK_SZ;
use alloc::sync::Arc;
use lwext4_rust::KernelDevOp;

/// Holds the MangoCore block device and current seek position for lwext4.
/// Created once per ext4 mount and owned by `Ext4BlockWrapper`.
pub struct MangoBlockDev {
    /// Underlying MangoCore block device (`BLOCK_SZ`-byte blocks).
    pub dev: Arc<dyn BlockDevice>,
    /// Current byte-level read/write position (advanced by seek/read/write).
    pub pos: usize,
    /// Total device size in bytes (set by dev_open via SEEK_END).
    pub size: u64,
    /// Reject every write before it reaches the generic BlockDevice trait.
    /// This lets lwext4 observe EROFS instead of treating a physical barrier
    /// as a successful writeback.
    pub read_only: bool,
    pub(crate) blocked_writes: usize,
}

/// Stateless KernelDevOp implementation — the `MangoBlockDev` state
/// (position, device) is passed via `&mut Self::DevType`.
pub struct MangoKernelDevOp;

/// Byte-range bridge core, parameterized so ktests can exercise the board's
/// 2048-byte platform-block contract even when the test kernel itself uses
/// 4096-byte QEMU blocks.
pub(crate) fn read_bytes_for_block_size<const PLATFORM_BLOCK_SIZE: usize>(
    device: &Arc<dyn BlockDevice>,
    start_pos: usize,
    buf: &mut [u8],
) {
    let mut done = 0usize;
    let head_offset = start_pos % PLATFORM_BLOCK_SIZE;
    if head_offset != 0 {
        let chunk = (PLATFORM_BLOCK_SIZE - head_offset).min(buf.len());
        let mut bounce = [0u8; PLATFORM_BLOCK_SIZE];
        device.read_block(start_pos / PLATFORM_BLOCK_SIZE, &mut bounce);
        buf[..chunk].copy_from_slice(&bounce[head_offset..head_offset + chunk]);
        done = chunk;
    }

    let middle_len = ((buf.len() - done) / PLATFORM_BLOCK_SIZE) * PLATFORM_BLOCK_SIZE;
    if middle_len != 0 {
        device.read_block(
            (start_pos + done) / PLATFORM_BLOCK_SIZE,
            &mut buf[done..done + middle_len],
        );
        done += middle_len;
    }

    if done < buf.len() {
        let mut bounce = [0u8; PLATFORM_BLOCK_SIZE];
        device.read_block(
            (start_pos + done) / PLATFORM_BLOCK_SIZE,
            &mut bounce,
        );
        let tail_len = buf.len() - done;
        buf[done..].copy_from_slice(&bounce[..tail_len]);
    }
}

pub(crate) fn write_bytes_for_block_size<const PLATFORM_BLOCK_SIZE: usize>(
    device: &Arc<dyn BlockDevice>,
    start_pos: usize,
    buf: &[u8],
) {
    let mut done = 0usize;
    let head_offset = start_pos % PLATFORM_BLOCK_SIZE;
    if head_offset != 0 {
        let chunk = (PLATFORM_BLOCK_SIZE - head_offset).min(buf.len());
        let block_id = start_pos / PLATFORM_BLOCK_SIZE;
        let mut bounce = [0u8; PLATFORM_BLOCK_SIZE];
        device.read_block(block_id, &mut bounce);
        bounce[head_offset..head_offset + chunk].copy_from_slice(&buf[..chunk]);
        device.write_block(block_id, &bounce);
        done = chunk;
    }

    let middle_len = ((buf.len() - done) / PLATFORM_BLOCK_SIZE) * PLATFORM_BLOCK_SIZE;
    if middle_len != 0 {
        device.write_block(
            (start_pos + done) / PLATFORM_BLOCK_SIZE,
            &buf[done..done + middle_len],
        );
        done += middle_len;
    }

    if done < buf.len() {
        let block_id = (start_pos + done) / PLATFORM_BLOCK_SIZE;
        let mut bounce = [0u8; PLATFORM_BLOCK_SIZE];
        device.read_block(block_id, &mut bounce);
        let tail_len = buf.len() - done;
        bounce[..tail_len].copy_from_slice(&buf[done..]);
        device.write_block(block_id, &bounce);
    }
}

impl KernelDevOp for MangoKernelDevOp {
    type DevType = MangoBlockDev;

    fn seek(dev: &mut MangoBlockDev, off: i64, whence: i32) -> Result<i64, i32> {
        let new_pos: i64 = match whence {
            0 => off,                                 // SEEK_SET
            1 => dev.pos as i64 + off,                // SEEK_CUR
            2 => dev.size as i64 + off,               // SEEK_END
            _ => return Err(-22),                     // EINVAL
        };
        if new_pos < 0 {
            return Err(-22);                          // EINVAL
        }
        dev.pos = new_pos as usize;
        Ok(new_pos)
    }

    fn read(dev: &mut MangoBlockDev, buf: &mut [u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }
        let start_pos = dev.pos;
        let end_pos = start_pos.checked_add(buf.len()).ok_or(-22)?;

        read_bytes_for_block_size::<BLOCK_SZ>(&dev.dev, start_pos, buf);

        dev.pos = end_pos;
        Ok(buf.len())
    }

    fn write(dev: &mut MangoBlockDev, buf: &[u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }
        if dev.read_only {
            dev.blocked_writes = dev.blocked_writes.saturating_add(1);
            if dev.blocked_writes <= 4 {
                log::error!(
                    "[lwext4][ro] rejected internal write at byte {} ({} bytes)",
                    dev.pos,
                    buf.len()
                );
            }
            return Err(lwext4_rust::bindings::EROFS as i32);
        }
        let start_pos = dev.pos;
        let end_pos = start_pos.checked_add(buf.len()).ok_or(-22)?;

        write_bytes_for_block_size::<BLOCK_SZ>(&dev.dev, start_pos, buf);

        dev.pos = end_pos;
        Ok(buf.len())
    }

    fn flush(dev: &mut MangoBlockDev) -> Result<usize, i32> {
        dev.dev.flush().map_err(|error| error as i32)?;
        Ok(0)
    }
}
