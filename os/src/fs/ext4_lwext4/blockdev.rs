//! MangoCore BlockDevice → lwext4 KernelDevOp bridge.
//!
//! Translates byte-level seek+read/write (from lwext4 C library) into
//! block_id-based read_block/write_block (MangoCore BlockDevice trait).
//! MangoCore blocks are 4096 bytes; lwext4 device blocks are 512 bytes.
//!
//! lwext4 C code calls:
//!   - dev_bread: seek(blk_id * 512, SEEK_SET) → read(buf, blk_cnt * 512)
//!   - dev_bwrite: seek(blk_id * 512, SEEK_SET) → write(buf, blk_cnt * 512)
//!   - dev_open: seek(0, SEEK_END) to determine device size
//!
//! Since 4096 / 512 = 8, most lwext4 I/O is naturally aligned to MangoCore
//! block boundaries. The bridge handles the general case (partial-block
//! overlap via read-modify-write) for correctness.

use alloc::sync::Arc;
use crate::drivers::block::BlockDevice;
use crate::hal::BLOCK_SZ;
use lwext4_rust::KernelDevOp;

/// Holds the MangoCore block device and current seek position for lwext4.
/// Created once per ext4 mount and owned by `Ext4BlockWrapper`.
pub struct MangoBlockDev {
    /// Underlying MangoCore block device (4096-byte blocks).
    pub dev: Arc<dyn BlockDevice>,
    /// Current byte-level read/write position (advanced by seek/read/write).
    pub pos: usize,
    /// Total device size in bytes (set by dev_open via SEEK_END).
    pub size: u64,
}

/// Stateless KernelDevOp implementation — the `MangoBlockDev` state
/// (position, device) is passed via `&mut Self::DevType`.
pub struct MangoKernelDevOp;

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
        let mut total: usize = 0;
        let start_pos = dev.pos;

        while total < buf.len() {
            let byte_pos = start_pos + total;
            let block_id = byte_pos / BLOCK_SZ;
            let block_off = byte_pos % BLOCK_SZ;
            let remain = buf.len() - total;
            let chunk = remain.min(BLOCK_SZ - block_off);

            // Read full MangoCore block (4096 bytes)
            let mut blk_buf = [0u8; 4096];
            dev.dev.read_block(block_id, &mut blk_buf[..BLOCK_SZ]);

            // Copy the requested portion into output buffer
            buf[total..total + chunk]
                .copy_from_slice(&blk_buf[block_off..block_off + chunk]);

            total += chunk;
        }

        dev.pos = start_pos + total;
        Ok(total)
    }

    fn write(dev: &mut MangoBlockDev, buf: &[u8]) -> Result<usize, i32> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut total: usize = 0;
        let start_pos = dev.pos;

        while total < buf.len() {
            let byte_pos = start_pos + total;
            let block_id = byte_pos / BLOCK_SZ;
            let block_off = byte_pos % BLOCK_SZ;
            let remain = buf.len() - total;
            let chunk = remain.min(BLOCK_SZ - block_off);

            if chunk == BLOCK_SZ && block_off == 0 {
                // Full-block aligned write — pass slice directly
                dev.dev.write_block(block_id, &buf[total..total + BLOCK_SZ]);
            } else {
                // Partial-block write — read-modify-write
                let mut blk_buf = [0u8; 4096];
                dev.dev.read_block(block_id, &mut blk_buf[..BLOCK_SZ]);
                blk_buf[block_off..block_off + chunk]
                    .copy_from_slice(&buf[total..total + chunk]);
                dev.dev.write_block(block_id, &blk_buf[..BLOCK_SZ]);
            }
            total += chunk;
        }

        dev.pos = start_pos + total;
        Ok(total)
    }

    fn flush(_dev: &mut MangoBlockDev) -> Result<usize, i32> {
        // Block devices don't need explicit flush
        Ok(0)
    }
}
