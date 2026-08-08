use super::BlockDevice;
use crate::mm::{
    frames_alloc, frames_alloc_fresh_contiguous, kernel_token, FrameTracker, PageTable,
    PageTableImpl, PhysAddr, VirtAddr,
};
use alloc::collections::BTreeMap;
use alloc::{sync::Arc, vec::Vec};
use core::ptr::NonNull;
use lazy_static::*;
use spin::Mutex;
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::{
    mmio::{MmioTransport, VirtIOHeader},
    DeviceType, Transport,
};
use virtio_drivers::{BufferDirection, Hal};
const VIRT_IO_BLOCK_SZ: usize = 512;
use super::virtio_dma_pool;
use crate::hal::{
    config::{PAGE_SIZE, PAGE_SIZE_BITS},
    BLOCK_SZ,
};
use crate::task::perf;
use crate::drivers::block::BlockDeviceResult;
const BLOCK_RATIO: usize = BLOCK_SZ / VIRT_IO_BLOCK_SZ;
// Multi-page DMA uses the pool for contiguity; fallback to BLOCK_SZ when pool
// is exhausted.  See virtio_dma_pool.rs.
const MAX_VIRTIO_REQ_BYTES: usize = virtio_dma_pool::DMA_POOL_BUF_BYTES;
#[allow(unused)]
const VIRTIO0: usize = 0x10001000;
const VIRTIO_MMIO_BASE: usize = 0x10001000;
const VIRTIO_MMIO_STRIDE: usize = 0x1000;

pub struct VirtIOBlock(Mutex<VirtIOBlk<VirtioHal, MmioTransport<'static>>>);

lazy_static! {
    static ref QUEUE_FRAMES: Mutex<BTreeMap<usize, Vec<Arc<FrameTracker>>>> =
        Mutex::new(BTreeMap::new());
}

/// Bridges the DMA pool reservation from `read_block`/`write_block` to
/// `VirtioHal::share()`.  The virtio-drivers library calls `share()` internally
/// during `dev.read_blocks()`/`write_blocks()`, passing through no extra
/// parameters, so the reservation is communicated via this static.
/// Stores `(slot, gen)` from `DmaReservation`.
static PENDING_DMA_RESERVATION: Mutex<Option<(usize, usize)>> = Mutex::new(None);

impl BlockDevice for VirtIOBlock {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        assert!(buf.len() % BLOCK_SZ == 0);
        perf::record_blk_vread(buf.len() / VIRT_IO_BLOCK_SZ);
        let mut dev = self.0.lock();

        let mut offset: usize = 0;
        while offset < buf.len() {
            let remaining = buf.len() - offset;
            let wanted = remaining.min(MAX_VIRTIO_REQ_BYTES);
            let pages = (wanted + PAGE_SIZE - 1) >> PAGE_SIZE_BITS;

            let reservation = virtio_dma_pool::dma_pool_reserve(pages);
            let chunk_len = if reservation.is_some() {
                wanted
            } else {
                BLOCK_SZ
            };

            // Store (slot, gen) for VirtioHal::share() to consume.
            // consume the original DmaReservation — the tuple alone is enough
            // for share() to reconstruct a DmaReservation and consume it.
            *PENDING_DMA_RESERVATION.lock() = reservation.map(|r| (r.slot, r.gen));

            let first_sector = (block_id + offset / BLOCK_SZ) * BLOCK_RATIO;
            dev.read_blocks(first_sector, &mut buf[offset..offset + chunk_len])
                .expect("Error when reading VirtIOBlk");

            // share() should have consumed the reservation. If it didn't (e.g.
            // because the virtio-drivers library split the buffer), cancel it.
            if let Some((slot, gen)) = PENDING_DMA_RESERVATION.lock().take() {
                virtio_dma_pool::dma_pool_cancel_reservation(slot, gen);
            }

            offset += chunk_len;
        }
        Ok(())
    }
    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        assert!(buf.len() % BLOCK_SZ == 0);
        perf::record_blk_vwrite(buf.len() / VIRT_IO_BLOCK_SZ);
        let mut dev = self.0.lock();

        let mut offset: usize = 0;
        while offset < buf.len() {
            let remaining = buf.len() - offset;
            let wanted = remaining.min(MAX_VIRTIO_REQ_BYTES);
            let pages = (wanted + PAGE_SIZE - 1) >> PAGE_SIZE_BITS;

            let reservation = virtio_dma_pool::dma_pool_reserve(pages);
            let chunk_len = if reservation.is_some() {
                wanted
            } else {
                BLOCK_SZ
            };

            *PENDING_DMA_RESERVATION.lock() = reservation.map(|r| (r.slot, r.gen));

            let first_sector = (block_id + offset / BLOCK_SZ) * BLOCK_RATIO;
            dev.write_blocks(first_sector, &buf[offset..offset + chunk_len])
                .expect("Error when writing VirtIOBlk");

            if let Some((slot, gen)) = PENDING_DMA_RESERVATION.lock().take() {
                virtio_dma_pool::dma_pool_cancel_reservation(slot, gen);
            }

            offset += chunk_len;
        }
        Ok(())
    }

    fn size_bytes(&self) -> Option<u64> {
        let sectors = self.0.lock().capacity();
        let bytes = sectors.saturating_mul(512);
        Some(bytes / BLOCK_SZ as u64 * BLOCK_SZ as u64)
    }

    fn flush(&self) -> BlockDeviceResult {
        self.0.lock().flush().map_err(|err| {
            log::error!("VirtIO block flush failed: {:?}", err);
            crate::drivers::block::BlockDeviceError::DeviceError
        })
    }

    fn supports_reliable_flush(&self) -> bool {
        true
    }
}

impl VirtIOBlock {
    #[allow(unused)]
    pub fn new() -> Self {
        Self::try_new(VIRTIO0).expect("VirtIOBlock::new: no device at VIRTIO0")
    }

    pub fn try_new(base_addr: usize) -> Option<Self> {
        let transport = unsafe {
            MmioTransport::new(NonNull::new(base_addr as *mut VirtIOHeader)?, 0x1000).ok()?
        };
        if transport.device_type() != DeviceType::Block {
            return None;
        }
        let blk = VirtIOBlk::<VirtioHal, MmioTransport<'static>>::new(transport).ok()?;
        Some(Self(Mutex::new(blk)))
    }
}

pub fn probe_rv64() -> [Option<alloc::sync::Arc<dyn super::BlockDevice>>; 2] {
    use alloc::sync::Arc;
    let d0 =
        VirtIOBlock::try_new(VIRTIO_MMIO_BASE).map(|b| Arc::new(b) as Arc<dyn super::BlockDevice>);
    if d0.is_some() {
        virtio_dma_pool::dma_pool_init_once();
        println!(
            "[kernel] block device 0: official fs (MMIO {:#x})",
            VIRTIO_MMIO_BASE
        );
    }
    let d1 = VirtIOBlock::try_new(VIRTIO_MMIO_BASE + VIRTIO_MMIO_STRIDE)
        .map(|b| Arc::new(b) as Arc<dyn super::BlockDevice>);
    if d1.is_some() {
        println!(
            "[kernel] block device 1: tools disk (MMIO {:#x})",
            VIRTIO_MMIO_BASE + VIRTIO_MMIO_STRIDE
        );
    }
    [d0, d1]
}

pub struct VirtioHal;

unsafe impl Hal for VirtioHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (usize, NonNull<u8>) {
        //log::info!("use dma_alloc with pages: {}", pages);
        let paddr = virtio_dma_alloc(pages);
        let vaddr = virtio_phys_to_virt(paddr);
        let ptr = NonNull::new(vaddr.0 as *mut u8)
            .expect("virtio_phys_to_virt returned null pointer in dma_alloc");
        (paddr.0, ptr)
    }

    unsafe fn dma_dealloc(paddr: usize, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        //log::info!("use dma_dealloc with paddr: {}, pages: {}", paddr, pages);
        virtio_dma_dealloc(PhysAddr(paddr), pages)
    }

    unsafe fn mmio_phys_to_virt(paddr: usize, _size: usize) -> NonNull<u8> {
        //log::info!("use mmio_phys_to_virt with paddr: {}", paddr);
        let vaddr = virtio_phys_to_virt(PhysAddr(paddr));
        NonNull::new(vaddr.0 as *mut u8)
            .expect("virtio_phys_to_virt returned null pointer in mmio_phys_to_virt")
    }

    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> usize {
        let buffer = buffer.as_ref();
        let pages = (buffer.len() + PAGE_SIZE - 1) >> PAGE_SIZE_BITS;

        // Check for pending pool reservation (set by read_block/write_block).
        // A single read_blocks/write_blocks generates 3 share() calls:
        // BlkReq header (16B), data, BlkResp status (1B).
        // Only consume for the data buffer (>= BLOCK_SZ); leave header/status
        // small descriptors to fall through to the single-page path.
        let mut pending = PENDING_DMA_RESERVATION.lock();
        if let Some((slot, gen)) = pending.take() {
            if buffer.len() >= BLOCK_SZ {
                drop(pending);
                let reservation = virtio_dma_pool::DmaReservation { slot, gen };
                let pa = virtio_dma_pool::dma_pool_consume_reserved(reservation);

                if matches!(
                    direction,
                    BufferDirection::DriverToDevice | BufferDirection::Both
                ) {
                    core::slice::from_raw_parts_mut(pa as *mut u8, buffer.len())
                        .copy_from_slice(buffer);
                }
                return pa;
            }
            // Small descriptor (header/status) — put reservation back for data call
            *pending = Some((slot, gen));
        }
        drop(pending);

        // Fallback: single-page allocation.
        // Multi-page without reservation cannot happen because read_block/write_block
        // always reserves before submitting multi-page chunks.
        assert_eq!(pages, 1, "share: multi-page DMA without pool reservation");
        let frames = frames_alloc(1).expect("share: failed to alloc frame");
        let pa = frames[0].ppn.start_addr().0;
        if matches!(
            direction,
            BufferDirection::DriverToDevice | BufferDirection::Both
        ) {
            core::slice::from_raw_parts_mut(pa as *mut u8, buffer.len()).copy_from_slice(buffer);
        }
        let old = QUEUE_FRAMES.lock().insert(pa, frames);
        assert!(
            old.is_none(),
            "[virtio] DMA frame key collision pa=0x{:x}",
            pa
        );
        pa
    }

    unsafe fn unshare(paddr: usize, mut buffer: NonNull<[u8]>, direction: BufferDirection) {
        // Check if this is a pool slot first.
        if let Some(slot) = virtio_dma_pool::dma_pool_lookup(paddr) {
            if matches!(
                direction,
                BufferDirection::DeviceToDriver | BufferDirection::Both
            ) {
                let buffer = buffer.as_mut();
                let src = paddr as *const u8;
                buffer.copy_from_slice(core::slice::from_raw_parts(src, buffer.len()));
            }
            virtio_dma_pool::dma_pool_finish_unshare(slot);
            return;
        }

        // Fallback: single-page allocation from QUEUE_FRAMES.
        let frames = QUEUE_FRAMES
            .lock()
            .remove(&paddr)
            .unwrap_or_else(|| panic!("[virtio] unshare unknown paddr=0x{:x}", paddr));

        if matches!(
            direction,
            BufferDirection::DeviceToDriver | BufferDirection::Both
        ) {
            let buffer = buffer.as_mut();
            let src = paddr as *const u8;
            buffer.copy_from_slice(core::slice::from_raw_parts(src, buffer.len()));
        }

        drop(frames);
    }
}

#[no_mangle]
pub extern "C" fn virtio_dma_alloc(pages: usize) -> PhysAddr {
    // Use fresh contiguous allocation — bypasses recycled stack fragmentation.
    let frames = frames_alloc_fresh_contiguous(pages)
        .expect("virtio_dma_alloc: failed to alloc contiguous fresh frames");

    let pa = PhysAddr::from(frames[0].ppn).0;
    let ppn = frames[0].ppn;
    let old = QUEUE_FRAMES.lock().insert(pa, frames);
    assert!(
        old.is_none(),
        "[virtio] dma_alloc key collision pa=0x{:x}",
        pa
    );
    ppn.into()
}

#[no_mangle]
pub extern "C" fn virtio_dma_dealloc(pa: PhysAddr, _pages: usize) -> i32 {
    let frames = QUEUE_FRAMES.lock().remove(&pa.0);
    assert!(
        frames.is_some(),
        "[virtio] dma_dealloc unknown pa=0x{:x}",
        pa.0
    );
    drop(frames);
    0
}

#[no_mangle]
pub extern "C" fn virtio_phys_to_virt(paddr: PhysAddr) -> VirtAddr {
    VirtAddr(paddr.0)
}

lazy_static! {
    static ref KERNEL_TOKEN: usize = kernel_token();
}

#[no_mangle]
pub extern "C" fn virtio_virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    PageTableImpl::from_token(*KERNEL_TOKEN)
        .translate_va(vaddr)
        .unwrap()
}
