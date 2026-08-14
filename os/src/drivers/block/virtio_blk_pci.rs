use super::{
    validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceNameStyle,
    BlockDeviceResult,
};
use crate::mm::{
    frame_alloc, frame_dealloc, frames_alloc, frames_alloc_fresh_contiguous, kernel_token,
    FrameTracker, PageTable, PageTableImpl, PhysAddr, PhysPageNum, StepByOne, VirtAddr,
};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::NonNull;
use lazy_static::*;
use spin::Mutex;
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::pci::bus::{
    BarInfo, Cam, Command, DeviceFunction, MemoryBarType, MmioCam, PciRoot,
};
use virtio_drivers::transport::pci::{virtio_device_type, PciTransport};
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{BufferDirection, Hal};
const VIRT_IO_BLOCK_SZ: usize = 512;
use super::virtio_dma_pool;
use crate::hal::{
    config::{PAGE_SIZE, PAGE_SIZE_BITS},
    BLOCK_SZ,
};
use crate::task::perf;
const BLOCK_RATIO: usize = BLOCK_SZ / VIRT_IO_BLOCK_SZ;
const MAX_VIRTIO_REQ_BYTES: usize = virtio_dma_pool::DMA_POOL_BUF_BYTES;

#[inline(always)]
fn small_dma_share_kind(len: usize) -> Option<usize> {
    match len {
        16 => Some(6),      // request header pool
        1 => Some(7),       // response status pool
        32 | 48 => Some(8), // indirect descriptor pool
        _ => None,
    }
}
#[cfg(not(target_arch = "riscv64"))]
const PCI_ECAM_BASE: usize = 0x2000_0000; // loongarch64 qemu
#[cfg(target_arch = "riscv64")]
const RV64_PCI_ECAM_FALLBACK_BASE: usize = 0x3000_0000;
const VIRT_PCI_BASE: usize = 0x4000_0000;
const VIRT_PCI_SIZE: usize = 0x0002_0000;

pub struct VirtIOBlock(Mutex<VirtIOBlk<VirtioHal, PciTransport>>);

#[inline(always)]
fn lock_virtio_device<'a, T>(mutex: &'a Mutex<T>) -> (spin::MutexGuard<'a, T>, usize) {
    let start = perf::perf_memory_io_time_now();
    match mutex.try_lock() {
        Some(guard) => (guard, start),
        None => {
            let guard = mutex.lock();
            let waited = perf::perf_memory_io_time_now().wrapping_sub(start);
            perf::record_virtio_device_lock_wait(1, waited);
            (guard, perf::perf_memory_io_time_now())
        }
    }
}

lazy_static! {
    static ref QUEUE_FRAMES: Mutex<BTreeMap<usize, Vec<Arc<FrameTracker>>>> =
        Mutex::new(BTreeMap::new());
    static ref PCI_RANGE_ALLOCATOR: Mutex<PciRangeAllocator> =
        Mutex::new(PciRangeAllocator::new(VIRT_PCI_BASE, VIRT_PCI_SIZE));
}

impl BlockDevice for VirtIOBlock {
    fn name_style(&self) -> BlockDeviceNameStyle {
        BlockDeviceNameStyle::Alphabetic("vd")
    }

    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        perf::record_blk_vread(buf.len() / VIRT_IO_BLOCK_SZ);
        let (mut dev, hold_start) = lock_virtio_device(&self.0);
        let _bridge = virtio_dma_pool::dma_bridge_lock();

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

            virtio_dma_pool::dma_bridge_set_reservation(reservation);

            let first_sector = block_id
                .checked_add(offset / BLOCK_SZ)
                .and_then(|current_block| current_block.checked_mul(BLOCK_RATIO))
                .ok_or(BlockDeviceError::OutOfBounds)?;
            // One record per submitted VirtIO request, after any DMA fallback split.
            perf::record_virtio_read();
            #[cfg(feature = "perf_stats")]
            let _blocked_reason = crate::task::current_task()
                .map(|task| task.blocked_reason_scope(crate::task::BlockedReason::BlockDevice));
            let result = dev.read_blocks(first_sector, &mut buf[offset..offset + chunk_len]);
            perf::record_virtio_blk_read_chunk(chunk_len);
            perf::record_virtio_request(1, false, chunk_len);

            virtio_dma_pool::dma_bridge_cancel_pending();

            result.map_err(|_| BlockDeviceError::DeviceError)?;

            offset += chunk_len;
        }
        perf::record_virtio_device_lock(1, 0, perf::perf_memory_io_time_now().wrapping_sub(hold_start));
        Ok(())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        perf::record_blk_vwrite(buf.len() / VIRT_IO_BLOCK_SZ);
        let (mut dev, hold_start) = lock_virtio_device(&self.0);
        let _bridge = virtio_dma_pool::dma_bridge_lock();

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

            virtio_dma_pool::dma_bridge_set_reservation(reservation);

            let first_sector = block_id
                .checked_add(offset / BLOCK_SZ)
                .and_then(|current_block| current_block.checked_mul(BLOCK_RATIO))
                .ok_or(BlockDeviceError::OutOfBounds)?;
            // One record per submitted VirtIO request, after any DMA fallback split.
            perf::record_virtio_write(chunk_len);
            #[cfg(feature = "perf_stats")]
            let _blocked_reason = crate::task::current_task()
                .map(|task| task.blocked_reason_scope(crate::task::BlockedReason::BlockDevice));
            let result = dev.write_blocks(first_sector, &buf[offset..offset + chunk_len]);
            perf::record_virtio_blk_write_chunk(chunk_len);
            perf::record_virtio_request(1, true, chunk_len);

            virtio_dma_pool::dma_bridge_cancel_pending();

            result.map_err(|_| BlockDeviceError::DeviceError)?;

            offset += chunk_len;
        }
        perf::record_virtio_device_lock(1, 0, perf::perf_memory_io_time_now().wrapping_sub(hold_start));
        Ok(())
    }

    fn flush(&self) -> BlockDeviceResult {
        let (mut dev, hold_start) = lock_virtio_device(&self.0);
        let _bridge = virtio_dma_pool::dma_bridge_lock();
        if !dev.supports_flush() {
            return Err(BlockDeviceError::FlushUnsupported);
        }
        perf::record_device_flush();
        let result = dev.flush().map_err(|_| BlockDeviceError::DeviceError);
        perf::record_virtio_device_lock(1, 0, perf::perf_memory_io_time_now().wrapping_sub(hold_start));
        result
    }

    fn supports_reliable_flush(&self) -> bool {
        self.0.lock().supports_flush()
    }

    fn size_bytes(&self) -> Option<u64> {
        let sectors = self.0.lock().capacity();
        let bytes = sectors.saturating_mul(512);
        Some(bytes / BLOCK_SZ as u64 * BLOCK_SZ as u64)
    }
}

pub struct PciRangeAllocator {
    end: usize,
    current: usize,
}

impl PciRangeAllocator {
    pub const fn new(pci_base: usize, pci_size: usize) -> Self {
        Self {
            current: pci_base,
            end: pci_base + pci_size,
        }
    }

    pub fn alloc_pci_mem(&mut self, size: usize) -> Option<usize> {
        if !size.is_power_of_two() {
            return None;
        }
        let ret = align_up(self.current, size);
        if ret + size > self.end {
            return None;
        }
        self.current = ret + size;
        Some(ret & !0xf)
    }
}

const fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

#[cfg(target_arch = "riscv64")]
fn pci_ecam_base() -> usize {
    match crate::hal::platform::platform_info().pci_host() {
        Some(host) => host.ecam_base,
        None => {
            println!(
                "[PCI] WARNING: no usable FDT PCI host; falling back to ECAM {:#x}",
                RV64_PCI_ECAM_FALLBACK_BASE
            );
            RV64_PCI_ECAM_FALLBACK_BASE
        }
    }
}

#[cfg(not(target_arch = "riscv64"))]
const fn pci_ecam_base() -> usize {
    PCI_ECAM_BASE
}

pub fn enumerate_virtio_pci(device_type: DeviceType) -> Option<PciTransport> {
    enumerate_all_virtio_pci(device_type)
        .into_iter()
        .next()
        .map(|(_df, t)| t)
}

pub fn enumerate_all_virtio_pci(
    device_type: DeviceType,
) -> alloc::vec::Vec<(DeviceFunction, PciTransport)> {
    let mmconfig_base = PhysAddr(pci_ecam_base()).direct_map_ptr();
    println!("[PCI] ECAM base: {:#x}", mmconfig_base as usize);

    let mmio_cam = unsafe { MmioCam::new(mmconfig_base, Cam::Ecam) };
    let mut pci_root = PciRoot::new(mmio_cam);
    let mut transports = alloc::vec::Vec::new();

    for (device_function, info) in pci_root.enumerate_bus(0) {
        println!(
            "[PCI] Device {:?}: vendor={:#x} device={:#x}",
            device_function, info.vendor_id, info.device_id
        );
        if let Some(virtio_type) = virtio_device_type(&info) {
            println!("[PCI] VirtIO device: {:?}", virtio_type);
            if virtio_type != device_type {
                continue;
            }

            println!("[PCI] Configuring BARs...");
            let mut device_ok = true;
            let mut bar_index = 0;
            'configure_bars: while bar_index < 6 {
                if let Some(bar) = pci_root.bar_info(device_function, bar_index).unwrap() {
                    if let BarInfo::Memory {
                        address_type,
                        address,
                        size,
                        ..
                    } = bar
                    {
                        println!(
                            "[PCI] BAR{}: {:?}, addr={:#x}, size={:#x}",
                            bar_index, address_type, address, size
                        );
                        if address == 0 && size != 0 {
                            let mut allocator = PCI_RANGE_ALLOCATOR.lock();
                            if let Some(alloc_addr) = allocator.alloc_pci_mem(size as usize) {
                                match address_type {
                                    MemoryBarType::Width64 => pci_root.set_bar_64(
                                        device_function,
                                        bar_index,
                                        alloc_addr as u64,
                                    ),
                                    MemoryBarType::Width32 => pci_root.set_bar_32(
                                        device_function,
                                        bar_index,
                                        alloc_addr as u32,
                                    ),
                                    _ => {}
                                }
                            } else {
                                println!(
                                    "[PCI] WARNING: PCI range allocator exhausted for BAR{}, skipping device {:?}",
                                    bar_index, device_function
                                );
                                device_ok = false;
                                break 'configure_bars;
                            }
                        }
                    }
                    if bar.takes_two_entries() {
                        println!("[PCI] BAR{} is 64-bit", bar_index);
                        bar_index += 1;
                    }
                }
                bar_index += 1;
            }
            if !device_ok {
                continue;
            }

            pci_root.set_command(
                device_function,
                Command::IO_SPACE | Command::MEMORY_SPACE | Command::BUS_MASTER,
            );
            println!("[PCI] Device enabled.");
            match PciTransport::new::<VirtioHal, MmioCam>(&mut pci_root, device_function) {
                Ok(transport) => {
                    transports.push((device_function, transport));
                }
                Err(e) => {
                    println!(
                        "[PCI] WARNING: failed to create PciTransport for {:?}: {:?}",
                        device_function, e
                    );
                }
            }
        }
    }
    transports
}

impl VirtIOBlock {
    pub fn new() -> Self {
        Self::try_from_iter(enumerate_all_virtio_pci(DeviceType::Block).into_iter())
            .expect("No VirtIO block device found")
    }

    fn try_from_iter(iter: impl Iterator<Item = (DeviceFunction, PciTransport)>) -> Option<Self> {
        for (_df, transport) in iter {
            match VirtIOBlk::<VirtioHal, PciTransport>::new(transport) {
                Ok(blk) => return Some(Self(Mutex::new(blk))),
                Err(_) => continue,
            }
        }
        None
    }
}

pub fn probe_la64() -> Vec<Arc<dyn super::BlockDevice>> {
    use alloc::sync::Arc;
    let transports = enumerate_all_virtio_pci(DeviceType::Block);
    let mut result = Vec::new();

    for (index, (df, transport)) in transports.into_iter().enumerate() {
        match VirtIOBlk::<VirtioHal, PciTransport>::new(transport) {
            Ok(blk) => {
                if result.is_empty() {
                    virtio_dma_pool::dma_pool_init_once();
                }
                result.push(Arc::new(VirtIOBlock(Mutex::new(blk))) as Arc<dyn super::BlockDevice>);
                println!(
                    "[kernel] discovered VirtIO PCI block device {} ({:?})",
                    index, df
                );
            }
            Err(_) => {
                println!(
                    "[kernel] VirtIO PCI block device {} ({:?}): initialization failed, skipping",
                    index, df
                );
            }
        }
    }
    result
}

pub struct VirtioHal;

unsafe impl Hal for VirtioHal {
    fn dma_alloc(pages: usize, _dir: BufferDirection) -> (usize, NonNull<u8>) {
        let paddr = virtio_dma_alloc(pages);
        let vaddr = virtio_phys_to_virt(paddr);
        let ptr = NonNull::new(vaddr.0 as *mut u8).unwrap();
        (paddr.0, ptr)
    }

    unsafe fn dma_dealloc(paddr: usize, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        virtio_dma_dealloc(PhysAddr(paddr), pages)
    }

    unsafe fn mmio_phys_to_virt(paddr: usize, _size: usize) -> NonNull<u8> {
        let vaddr = virtio_phys_to_virt(PhysAddr(paddr));
        NonNull::new(vaddr.0 as *mut u8).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, dir: BufferDirection) -> usize {
        let buffer = buffer.as_ref();
        let pages = (buffer.len() + PAGE_SIZE - 1) >> PAGE_SIZE_BITS;

        if buffer.len() >= BLOCK_SZ {
            if let Some(reservation) = virtio_dma_pool::dma_bridge_take_data_reservation() {
                let pa = virtio_dma_pool::dma_pool_consume_reserved(reservation);
                perf::record_virtio_dma_share(0);

                if matches!(dir, BufferDirection::DriverToDevice | BufferDirection::Both) {
                    core::slice::from_raw_parts_mut(PhysAddr(pa).direct_map_ptr(), buffer.len())
                        .copy_from_slice(buffer);
                }
                return pa;
            }
        }

        // Reuse fixed single-page slots for block request descriptors. Keep
        // arbitrary one-page users on the old fallback path.
        if let Some(pool_kind) = small_dma_share_kind(buffer.len()) {
            if let Some((_slot, pa)) = virtio_dma_pool::dma_pool_try_alloc_small() {
                perf::record_virtio_dma_share(pool_kind);
                if matches!(dir, BufferDirection::DriverToDevice | BufferDirection::Both) {
                    core::slice::from_raw_parts_mut(PhysAddr(pa).direct_map_ptr(), buffer.len())
                        .copy_from_slice(buffer);
                }
                return pa;
            }
        }

        assert_eq!(pages, 1, "share: multi-page DMA without pool reservation");
        let share_kind = if buffer.len() >= BLOCK_SZ {
            1 // data fallback
        } else if buffer.len() == 1 {
            3 // response status
        } else if buffer.len() == 16 {
            2 // request header
        } else if matches!(buffer.len(), 32 | 48) {
            4 // indirect descriptor table
        } else {
            5
        };
        perf::record_virtio_dma_share(share_kind);
        let frames = frames_alloc(1).expect("share: failed to alloc frame");
        let pa = frames[0].ppn.start_addr().0;
        if matches!(dir, BufferDirection::DriverToDevice | BufferDirection::Both) {
            core::slice::from_raw_parts_mut(PhysAddr(pa).direct_map_ptr(), buffer.len())
                .copy_from_slice(buffer);
        }
        let old = QUEUE_FRAMES.lock().insert(pa, frames);
        assert!(
            old.is_none(),
            "[virtio-pci] DMA frame key collision pa=0x{:x}",
            pa
        );
        pa
    }

    unsafe fn unshare(paddr: usize, mut buffer: NonNull<[u8]>, dir: BufferDirection) {
        if let Some(slot) = virtio_dma_pool::dma_pool_lookup(paddr) {
            if matches!(dir, BufferDirection::DeviceToDriver | BufferDirection::Both) {
                let buffer = buffer.as_mut();
                let src = PhysAddr(paddr).direct_map_ptr().cast_const();
                buffer.copy_from_slice(core::slice::from_raw_parts(src, buffer.len()));
            }
            virtio_dma_pool::dma_pool_finish_unshare(slot);
            return;
        }

        let frames = QUEUE_FRAMES
            .lock()
            .remove(&paddr)
            .unwrap_or_else(|| panic!("[virtio-pci] unshare unknown paddr=0x{:x}", paddr));

        if matches!(dir, BufferDirection::DeviceToDriver | BufferDirection::Both) {
            let buffer = buffer.as_mut();
            let src = PhysAddr(paddr).direct_map_ptr().cast_const();
            buffer.copy_from_slice(core::slice::from_raw_parts(src, buffer.len()));
        }

        drop(frames);
    }
}

pub fn virtio_dma_alloc(pages: usize) -> PhysAddr {
    let frames = frames_alloc_fresh_contiguous(pages)
        .expect("virtio_dma_alloc: failed to alloc contiguous fresh frames");

    let pa = PhysAddr::from(frames[0].ppn).0;
    let ppn = frames[0].ppn;
    let old = QUEUE_FRAMES.lock().insert(pa, frames);
    assert!(
        old.is_none(),
        "[virtio-pci] dma_alloc key collision pa=0x{:x}",
        pa
    );
    ppn.into()
}

pub fn virtio_dma_dealloc(pa: PhysAddr, _pages: usize) -> i32 {
    let frames = QUEUE_FRAMES.lock().remove(&pa.0);
    assert!(
        frames.is_some(),
        "[virtio-pci] dma_dealloc unknown pa=0x{:x}",
        pa.0
    );
    drop(frames);
    0
}

pub fn virtio_phys_to_virt(paddr: PhysAddr) -> VirtAddr {
    VirtAddr(paddr.direct_map_ptr() as usize)
}

lazy_static! {
    static ref KERNEL_TOKEN: usize = kernel_token();
}

pub fn virtio_virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    PageTableImpl::from_token(*KERNEL_TOKEN)
        .translate_va(vaddr)
        .unwrap()
}
