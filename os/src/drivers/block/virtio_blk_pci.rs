use super::BlockDevice;
use crate::mm::{
    frame_alloc, frame_dealloc, frames_alloc, kernel_token, FrameTracker, PageTable, PageTableImpl,
    PhysAddr, PhysPageNum, StepByOne, VirtAddr,
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
use crate::hal::{
    config::{PAGE_SIZE, PAGE_SIZE_BITS},
    BLOCK_SZ,
};
use crate::task::perf;
const BLOCK_RATIO: usize = BLOCK_SZ / VIRT_IO_BLOCK_SZ;
const MAX_VIRTIO_REQ_BYTES: usize = BLOCK_SZ;
#[cfg(not(target_arch = "riscv64"))]
const PCI_ECAM_BASE: usize = 0x2000_0000; // loongarch64 qemu
#[cfg(target_arch = "riscv64")]
const PCI_ECAM_BASE: usize = 0x3000_0000; // riscv64 qemu
const VIRT_PCI_BASE: usize = 0x4000_0000;
const VIRT_PCI_SIZE: usize = 0x0002_0000;

pub struct VirtIOBlock(Mutex<VirtIOBlk<VirtioHal, PciTransport>>);

lazy_static! {
    static ref QUEUE_FRAMES: Mutex<BTreeMap<usize, Vec<Arc<FrameTracker>>>> = Mutex::new(BTreeMap::new());
    static ref PCI_RANGE_ALLOCATOR: Mutex<PciRangeAllocator> =
        Mutex::new(PciRangeAllocator::new(VIRT_PCI_BASE, VIRT_PCI_SIZE));
}

impl BlockDevice for VirtIOBlock {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert!(buf.len() % BLOCK_SZ == 0);
        perf::record_blk_vread(buf.len() / VIRT_IO_BLOCK_SZ);
        let mut dev = self.0.lock();
        for (chunk_idx, chunk) in buf.chunks_mut(MAX_VIRTIO_REQ_BYTES).enumerate() {
            let first_sector = (block_id + chunk_idx) * BLOCK_RATIO;
            dev.read_blocks(first_sector, chunk)
                .expect("read error");
        }
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        assert!(buf.len() % BLOCK_SZ == 0);
        perf::record_blk_vwrite(buf.len() / VIRT_IO_BLOCK_SZ);
        let mut dev = self.0.lock();
        for (chunk_idx, chunk) in buf.chunks(MAX_VIRTIO_REQ_BYTES).enumerate() {
            let first_sector = (block_id + chunk_idx) * BLOCK_RATIO;
            dev.write_blocks(first_sector, chunk)
                .expect("write error");
        }
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

pub fn enumerate_virtio_pci(device_type: DeviceType) -> Option<PciTransport> {
    enumerate_all_virtio_pci(device_type).into_iter().next().map(|(_df, t)| t)
}

pub fn enumerate_all_virtio_pci(
    device_type: DeviceType,
) -> alloc::vec::Vec<(DeviceFunction, PciTransport)> {
    let mmconfig_base = PCI_ECAM_BASE as *mut u8;
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

    fn try_from_iter(
        iter: impl Iterator<Item = (DeviceFunction, PciTransport)>,
    ) -> Option<Self> {
        for (_df, transport) in iter {
            match VirtIOBlk::<VirtioHal, PciTransport>::new(transport) {
                Ok(blk) => return Some(Self(Mutex::new(blk))),
                Err(_) => continue,
            }
        }
        None
    }
}

pub fn probe_la64() -> [Option<alloc::sync::Arc<dyn super::BlockDevice>>; 2] {
    use alloc::sync::Arc;
    let transports = enumerate_all_virtio_pci(DeviceType::Block);
    let mut result: [Option<Arc<dyn super::BlockDevice>>; 2] = [None, None];

    for (i, (df, transport)) in transports.into_iter().enumerate() {
        if i >= 2 {
            println!(
                "[kernel] block device {} ({:?}): skipping (max 2 devices)",
                i, df
            );
            break;
        }
        match VirtIOBlk::<VirtioHal, PciTransport>::new(transport) {
            Ok(blk) => {
                let label = if i == 0 { "official fs" } else { "tools disk" };
                result[i] = Some(Arc::new(VirtIOBlock(Mutex::new(blk))) as Arc<dyn super::BlockDevice>);
                println!("[kernel] block device {}: {} ({:?})", i, label, df);
            }
            Err(_e) => {
                if i == 0 {
                    panic!(
                        "[kernel] FATAL: failed to initialize block device 0 ({:?})",
                        df
                    );
                } else {
                    println!(
                        "[kernel] block device {} ({:?}): initialization failed, skipping",
                        i, df
                    );
                }
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
        let frames = frames_alloc(pages).unwrap();
        if matches!(dir, BufferDirection::DriverToDevice | BufferDirection::Both) {
            let pa_start = frames[0].ppn.start_addr().0;
            core::slice::from_raw_parts_mut(pa_start as *mut u8, buffer.len())
                .copy_from_slice(buffer);
        }
        let pa = frames[0].ppn.start_addr().0;
        let old = QUEUE_FRAMES.lock().insert(pa, frames);
        assert!(old.is_none(), "[virtio-pci] DMA frame key collision pa=0x{:x}", pa);
        pa
    }

    unsafe fn unshare(paddr: usize, mut buffer: NonNull<[u8]>, dir: BufferDirection) {
        let frames = QUEUE_FRAMES.lock()
            .remove(&paddr)
            .unwrap_or_else(|| panic!("[virtio-pci] unshare unknown paddr=0x{:x}", paddr));

        if matches!(dir, BufferDirection::DeviceToDriver | BufferDirection::Both) {
            let buffer = buffer.as_mut();
            let src = paddr as *const u8;
            buffer.copy_from_slice(core::slice::from_raw_parts(src, buffer.len()));
        }

        drop(frames);
    }
}

pub fn virtio_dma_alloc(pages: usize) -> PhysAddr {
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
    assert!(old.is_none(), "[virtio-pci] dma_alloc key collision pa=0x{:x}", pa);
    ppn_base.into()
}

pub fn virtio_dma_dealloc(pa: PhysAddr, _pages: usize) -> i32 {
    let frames = QUEUE_FRAMES.lock().remove(&pa.0);
    assert!(frames.is_some(), "[virtio-pci] dma_dealloc unknown pa=0x{:x}", pa.0);
    drop(frames);
    0
}

pub fn virtio_phys_to_virt(paddr: PhysAddr) -> VirtAddr {
    VirtAddr(paddr.0)
}

lazy_static! {
    static ref KERNEL_TOKEN: usize = kernel_token();
}

pub fn virtio_virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    PageTableImpl::from_token(*KERNEL_TOKEN)
        .translate_va(vaddr)
        .unwrap()
}
