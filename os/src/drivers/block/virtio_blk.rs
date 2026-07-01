use super::BlockDevice;
use crate::mm::{
    frame_alloc, frame_dealloc, frames_alloc, kernel_token, FrameTracker, PageTable, PageTableImpl,
    PhysAddr, PhysPageNum, StepByOne, VirtAddr,
};
use alloc::collections::BTreeMap;
use alloc::{sync::Arc, vec::Vec};
use core::ptr::NonNull;
use lazy_static::*;
use spin::Mutex;
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};
use virtio_drivers::{BufferDirection, Hal};
const VIRT_IO_BLOCK_SZ: usize = 512;
use crate::hal::{
    config::{PAGE_SIZE, PAGE_SIZE_BITS},
    BLOCK_SZ,
};
use crate::task::perf;
const BLOCK_RATIO: usize = BLOCK_SZ / VIRT_IO_BLOCK_SZ;
// MAX_VIRTIO_REQ_BYTES 受限于 VirtioHal::share 中 frames_alloc 不保证物理连续；
// 每页之内安全，跨页需先修复 DMA 分配为 frames_alloc_contiguous。
const MAX_VIRTIO_REQ_BYTES: usize = BLOCK_SZ;
#[allow(unused)]
const VIRTIO0: usize = 0x10001000;
const VIRTIO_MMIO_BASE: usize = 0x10001000;
const VIRTIO_MMIO_STRIDE: usize = 0x1000;

pub struct VirtIOBlock(Mutex<VirtIOBlk<VirtioHal, MmioTransport<'static>>>);

lazy_static! {
    static ref QUEUE_FRAMES: Mutex<BTreeMap<usize, Vec<Arc<FrameTracker>>>> = Mutex::new(BTreeMap::new());
}

impl BlockDevice for VirtIOBlock {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert!(buf.len() % BLOCK_SZ == 0);
        perf::record_blk_vread(buf.len() / VIRT_IO_BLOCK_SZ);
        let mut dev = self.0.lock();
        for (chunk_idx, chunk) in buf.chunks_mut(MAX_VIRTIO_REQ_BYTES).enumerate() {
            let first_sector = (block_id + chunk_idx) * BLOCK_RATIO;
            dev.read_blocks(first_sector, chunk)
                .expect("Error when reading VirtIOBlk");
        }
    }
    fn write_block(&self, block_id: usize, buf: &[u8]) {
        assert!(buf.len() % BLOCK_SZ == 0);
        perf::record_blk_vwrite(buf.len() / VIRT_IO_BLOCK_SZ);
        let mut dev = self.0.lock();
        for (chunk_idx, chunk) in buf.chunks(MAX_VIRTIO_REQ_BYTES).enumerate() {
            let first_sector = (block_id + chunk_idx) * BLOCK_RATIO;
            dev.write_blocks(first_sector, chunk)
                .expect("Error when writing VirtIOBlk");
        }
    }

    fn size_bytes(&self) -> Option<u64> {
        let sectors = self.0.lock().capacity();
        let bytes = sectors.saturating_mul(512);
        Some(bytes / BLOCK_SZ as u64 * BLOCK_SZ as u64)
    }
}

impl VirtIOBlock {
    #[allow(unused)]
    pub fn new() -> Self {
        Self::try_new(VIRTIO0).expect("VirtIOBlock::new: no device at VIRTIO0")
    }

    pub fn try_new(base_addr: usize) -> Option<Self> {
        let transport = unsafe {
            MmioTransport::new(
                NonNull::new(base_addr as *mut VirtIOHeader)?,
                0x1000,
            )
            .ok()?
        };
        let blk = VirtIOBlk::<VirtioHal, MmioTransport<'static>>::new(transport).ok()?;
        Some(Self(Mutex::new(blk)))
    }
}

pub fn probe_rv64() -> [Option<alloc::sync::Arc<dyn super::BlockDevice>>; 2] {
    use alloc::sync::Arc;
    let d0 = VirtIOBlock::try_new(VIRTIO_MMIO_BASE)
        .map(|b| Arc::new(b) as Arc<dyn super::BlockDevice>);
    if d0.is_some() {
        println!("[kernel] block device 0: official fs (MMIO {:#x})", VIRTIO_MMIO_BASE);
    }
    let d1 = VirtIOBlock::try_new(VIRTIO_MMIO_BASE + VIRTIO_MMIO_STRIDE)
        .map(|b| Arc::new(b) as Arc<dyn super::BlockDevice>);
    if d1.is_some() {
        println!("[kernel] block device 1: tools disk (MMIO {:#x})", VIRTIO_MMIO_BASE + VIRTIO_MMIO_STRIDE);
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
        let frames = frames_alloc(pages).expect("share: failed to alloc frames");

        if matches!(
            direction,
            BufferDirection::DriverToDevice | BufferDirection::Both
        ) {
            let pa_start = frames[0].ppn.start_addr().0;
            let dst_slice = core::slice::from_raw_parts_mut(pa_start as *mut u8, buffer.len());
            dst_slice.copy_from_slice(buffer);
        }

        let pa = frames[0].ppn.start_addr().0;
        let old = QUEUE_FRAMES.lock().insert(pa, frames);
        assert!(old.is_none(), "[virtio] DMA frame key collision pa=0x{:x}", pa);
        pa
    }

    unsafe fn unshare(paddr: usize, mut buffer: NonNull<[u8]>, direction: BufferDirection) {
        // Remove from map first — if paddr is unknown, panic before copying garbage
        let frames = QUEUE_FRAMES.lock()
            .remove(&paddr)
            .unwrap_or_else(|| panic!("[virtio] unshare unknown paddr=0x{:x}", paddr));

        // Copy data while frames are still alive (paddr is valid)
        if matches!(
            direction,
            BufferDirection::DeviceToDriver | BufferDirection::Both
        ) {
            let buffer = buffer.as_mut();
            let src_ptr = paddr as *const u8;
            buffer.copy_from_slice(core::slice::from_raw_parts(src_ptr, buffer.len()));
        }

        // Drop frames OUTSIDE the lock to avoid deadlock
        // (FrameTracker::drop → frame_dealloc → FRAME_ALLOCATOR lock)
        drop(frames);
    }
}

#[no_mangle]
pub extern "C" fn virtio_dma_alloc(pages: usize) -> PhysAddr {
    let mut ppn_base = PhysPageNum(0);
    let mut frames = Vec::with_capacity(pages);
    for i in 0..pages {
        let frame = frame_alloc().unwrap();
        if i == 0 {
            ppn_base = frame.ppn;
        }
        assert_eq!(frame.ppn.0, ppn_base.0 + i);
        frames.push(frame);
    }
    let pa = PhysAddr::from(ppn_base).0;
    let old = QUEUE_FRAMES.lock().insert(pa, frames);
    assert!(old.is_none(), "[virtio] dma_alloc key collision pa=0x{:x}", pa);
    ppn_base.into()
}

#[no_mangle]
pub extern "C" fn virtio_dma_dealloc(pa: PhysAddr, _pages: usize) -> i32 {
    let frames = QUEUE_FRAMES.lock().remove(&pa.0);
    assert!(frames.is_some(), "[virtio] dma_dealloc unknown pa=0x{:x}", pa.0);
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
