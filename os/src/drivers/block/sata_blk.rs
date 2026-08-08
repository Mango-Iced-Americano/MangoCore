use crate::config::PAGE_SIZE;
use crate::drivers::block::{
    validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceResult,
};
use crate::hal::BLOCK_SZ;
use crate::mm::{frame_alloc, frame_dealloc, PhysAddr};
use isomorphic_drivers::{
    block::ahci::{AHCI, BLOCK_SIZE},
    provider,
};
use log::info;
use pci::*;
use spin::Mutex;

/// One reusable AHCI DMA slot. The controller is serialized by `SataBlock`'s
/// mutex, so one slot covers every in-flight request without the per-request
/// allocation and fragmentation risks that the VirtIO DMA pool was designed to
/// remove. 64 KiB matches the proven VirtIO pool slot size while keeping the
/// permanently reserved low-memory extent modest.
const SATA_DMA_BYTES: usize = 64 * 1024;

pub struct SataBlock(Mutex<AHCI<Provider>>);

impl SataBlock {
    pub fn new() -> Self {
        Self(Mutex::new(pci_init().expect("AHCI new failed")))
    }
}

impl BlockDevice for SataBlock {
    fn read_block(&self, mut block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        // 内核BLOCK_SZ为2048，SATA驱动中BLOCK_SIZE为512，四倍转化关系
        block_id = block_id
            .checked_mul(BLOCK_SZ / BLOCK_SIZE)
            .ok_or(BlockDeviceError::OutOfBounds)?;
        let mut controller = self.0.lock();
        for chunk in buf.chunks_mut(SATA_DMA_BYTES) {
            let started =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            controller
                .read_blocks(block_id, chunk)
                .unwrap_or_else(|err| panic!("SATA read LBA {} failed: {:?}", block_id, err));
            crate::task::perf::record_sata_read(
                chunk.len(),
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .wrapping_sub(started),
            );
            block_id = block_id
                .checked_add(chunk.len() / BLOCK_SIZE)
                .ok_or(BlockDeviceError::OutOfBounds)?;
        }
        Ok(())
    }

    fn write_block(&self, mut block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        block_id = block_id
            .checked_mul(BLOCK_SZ / BLOCK_SIZE)
            .ok_or(BlockDeviceError::OutOfBounds)?;
        let mut controller = self.0.lock();
        for chunk in buf.chunks(SATA_DMA_BYTES) {
            let started =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            controller
                .write_blocks(block_id, chunk)
                .unwrap_or_else(|err| panic!("SATA write LBA {} failed: {:?}", block_id, err));
            crate::task::perf::record_sata_write(
                chunk.len(),
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .wrapping_sub(started),
            );
            block_id = block_id
                .checked_add(chunk.len() / BLOCK_SIZE)
                .ok_or(BlockDeviceError::OutOfBounds)?;
        }
        Ok(())
    }
}

pub struct Provider;

impl provider::Provider for Provider {
    const PAGE_SIZE: usize = PAGE_SIZE;
    fn alloc_dma(size: usize) -> (usize, usize) {
        let pages = size / PAGE_SIZE;
        let mut base = 0;
        for i in 0..pages {
            let frame = frame_alloc().unwrap();
            let frame_pa: PhysAddr = frame.ppn.into();
            let frame_pa = frame_pa.into();
            core::mem::forget(frame);
            if i == 0 {
                base = frame_pa;
            }
            assert_eq!(frame_pa, base + i * PAGE_SIZE);
        }
        let base_page = base / PAGE_SIZE;
        info!("virtio_dma_alloc: {:#x} {}", base_page, pages);
        (base, base)
    }

    fn dealloc_dma(va: usize, size: usize) {
        info!("dealloc_dma: {:x} {:x}", va, size);
        let pages = size / PAGE_SIZE;
        let mut pa = va;
        for _ in 0..pages {
            frame_dealloc(PhysAddr::from(pa).into());
            pa += PAGE_SIZE;
        }
    }
}

// 扫描pci设备
// 查看手册得知，配置空间位于 0xFE_0000_0000
const PCI_CONFIG_ADDRESS: usize = 0xFE_0000_0000;
const PCI_COMMAND: u16 = 0x04;

struct UnusedPort;
impl PortOps for UnusedPort {
    unsafe fn read8(&self, _port: u16) -> u8 {
        0
    }
    unsafe fn read16(&self, _port: u16) -> u16 {
        0
    }
    unsafe fn read32(&self, _port: u16) -> u32 {
        0
    }
    unsafe fn write8(&self, _port: u16, _val: u8) {}
    unsafe fn write16(&self, _port: u16, _val: u16) {}
    unsafe fn write32(&self, _port: u16, _val: u32) {}
}

unsafe fn enable(loc: Location) {
    let ops = &UnusedPort;
    let am = CSpaceAccessMethod::MemoryMapped;

    let orig = am.read16(ops, loc, PCI_COMMAND);
    // bit0     |bit1       |bit2          |bit3           |bit10
    // IO Space |MEM Space  |Bus Mastering |Special Cycles |PCI Interrupt Disable
    am.write32(ops, loc, PCI_COMMAND, (orig | 0x40f) as u32);
    // Use PCI legacy interrupt instead
    // IO Space | MEM Space | Bus Mastering | Special Cycles
    am.write32(ops, loc, PCI_COMMAND, (orig | 0xf) as u32);
}

pub fn pci_init() -> Option<AHCI<Provider>> {
    for dev in unsafe {
        scan_bus(
            &UnusedPort,
            CSpaceAccessMethod::MemoryMapped,
            PCI_CONFIG_ADDRESS,
        )
    } {
        info!(
            "pci: {:02x}:{:02x}.{} {:#x} {:#x} ({} {}) irq: {}:{:?}",
            dev.loc.bus,
            dev.loc.device,
            dev.loc.function,
            dev.id.vendor_id,
            dev.id.device_id,
            dev.id.class,
            dev.id.subclass,
            dev.pic_interrupt_line,
            dev.interrupt_pin
        );
        dev.bars.iter().enumerate().for_each(|(index, bar)| {
            if let Some(BAR::Memory(pa, len, _, t)) = bar {
                info!("\tbar#{} (MMIO) {:#x} [{:#x}] [{:?}]", index, pa, len, t);
            } else if let Some(BAR::IO(pa, len)) = bar {
                info!("\tbar#{} (IO) {:#x} [{:#x}]", index, pa, len);
            }
        });
        if dev.id.class == 0x01 && dev.id.subclass == 0x06 {
            // Mass storage class, SATA subclass
            if let Some(BAR::Memory(pa, len, _, _)) = dev.bars[0] {
                if pa == 0 {
                    continue;
                }
                info!("Found AHCI device");
                // 检查status的第五位是否为1，如果是，则说明该设备存在能力链表
                if dev.status | Status::CAPABILITIES_LIST == Status::empty() {
                    info!("\tNo capabilities list");
                    return None;
                }
                unsafe { enable(dev.loc) };
                if let Ok(x) = AHCI::new(pa as usize, len as usize) {
                    return Some(x);
                }
            }
        }
    }
    None
}
