#[cfg(feature = "boot_la_uboot_dmw")]
use crate::config::HIGH_BASE_EIGHT;
use crate::config::PAGE_SIZE;
use crate::drivers::block::{
    validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceNameStyle,
    BlockDeviceResult,
};
use crate::hal::BLOCK_SZ;
use crate::mm::{frame_dealloc, frames_alloc, PhysAddr};
use alloc::sync::Arc;
use isomorphic_drivers::{
    block::ahci::{AhciError, AHCI, BLOCK_SIZE},
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
const SATA_DMA_SECTORS: usize = SATA_DMA_BYTES / BLOCK_SIZE;

#[inline(always)]
fn cpu_mmio_addr(physical: usize) -> usize {
    #[cfg(feature = "boot_la_uboot_dmw")]
    {
        // 2K1000 的 DMW2 把 VSEG=8 映射为强序非缓存窗口。CPU 解引用 PCI/AHCI
        // 寄存器必须使用该别名；写入 HBA 的 DMA 地址仍保持原始物理地址。
        return physical | HIGH_BASE_EIGHT;
    }
    #[cfg(not(feature = "boot_la_uboot_dmw"))]
    physical
}

pub struct SataBlock(Mutex<AHCI<Provider>>);

impl SataBlock {
    pub(crate) fn probe() -> Result<Self, SataInitError> {
        let controller = sata_init()?;
        println!(
            "[sata] AHCI ready: model='{}' firmware='{}' sectors={} bytes={}",
            controller.model(),
            controller.firmware(),
            controller.capacity_sectors(),
            controller.capacity_bytes().unwrap_or(0)
        );
        Ok(Self(Mutex::new(controller)))
    }
}

impl BlockDevice for SataBlock {
    fn name_style(&self) -> BlockDeviceNameStyle {
        // SATA disks use the Linux-compatible `sd*` namespace. The userspace
        // root contract and U-Boot board environment refer to the first
        // persistent partition as `/dev/sda1`; leaving the default `blk*`
        // style would make an otherwise healthy board fail while resolving
        // an explicit `root=/dev/sda1` command line.
        BlockDeviceNameStyle::Alphabetic("sd")
    }

    fn read_block(&self, mut block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        // 上层 block_id 以平台 BLOCK_SZ 计数，ATA LBA 固定以 512 字节 sector 计数。
        block_id = block_id
            .checked_mul(BLOCK_SZ / BLOCK_SIZE)
            .ok_or(BlockDeviceError::OutOfBounds)?;
        let mut controller = self.0.lock();
        for chunk in buf.chunks_mut(SATA_DMA_BYTES) {
            let started =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            controller.read_blocks(block_id, chunk).map_err(|err| {
                log::error!("SATA read LBA {} failed: {:?}", block_id, err);
                BlockDeviceError::DeviceError
            })?;
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
            controller.write_blocks(block_id, chunk).map_err(|err| {
                log::error!("SATA write LBA {} failed: {:?}", block_id, err);
                BlockDeviceError::DeviceError
            })?;
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

    fn flush(&self) -> BlockDeviceResult {
        let started =
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
        let result = self.0.lock().flush().map_err(|err| {
            log::error!("SATA cache flush failed: {:?}", err);
            BlockDeviceError::DeviceError
        });
        crate::task::perf::record_sata_flush(
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                .wrapping_sub(started),
        );
        result
    }

    fn supports_reliable_flush(&self) -> bool {
        true
    }

    fn size_bytes(&self) -> Option<u64> {
        self.0.lock().capacity_bytes()
    }
}

pub struct Provider;

impl provider::Provider for Provider {
    const PAGE_SIZE: usize = PAGE_SIZE;
    const AHCI_MAX_TRANSFER_SECTORS: usize = SATA_DMA_SECTORS;
    #[cfg(feature = "boot_la_uboot_dmw")]
    // 2K1000 的 HBA reset 会清除厂商实现的可写 host 位。这里复用随板
    // U-Boot 已验证的恢复合同：保留 SMPS/SPM、强制 SSS，并恢复 PI。
    const AHCI_CAPABILITY_SAVE_MASK: u32 = (1 << 28) | (1 << 17);
    #[cfg(feature = "boot_la_uboot_dmw")]
    const AHCI_CAPABILITY_FORCE_BITS: u32 = 1 << 27;
    #[cfg(feature = "boot_la_uboot_dmw")]
    const AHCI_PORTS_IMPLEMENTED: Option<u32> = Some(0x0f);

    fn delay_us(micros: usize) {
        let frequency = crate::hal::get_clock_freq();
        let ticks = ((frequency as u128 * micros as u128 + 999_999) / 1_000_000) as usize;
        let start = crate::hal::get_time();
        while crate::hal::get_time().wrapping_sub(start) < ticks.max(1) {
            core::hint::spin_loop();
        }
    }

    fn alloc_dma(size: usize) -> (usize, usize) {
        assert!(
            size > 0 && size <= SATA_DMA_BYTES,
            "AHCI DMA allocation exceeds the reusable slot"
        );
        let pages = size.div_ceil(PAGE_SIZE);
        let frames = frames_alloc(pages).expect("AHCI contiguous DMA allocation failed");
        let frame_pa: PhysAddr = frames[0].ppn.into();
        let base: usize = frame_pa.into();
        assert!(
            base.checked_add(pages * PAGE_SIZE)
                .is_some_and(|end| end <= 0x1_0000_0000),
            "2K1000 AHCI requires DMA memory below 4 GiB: {:#x}",
            base
        );
        // AHCI 独占这段连续物理页直到 Drop；Arc 只用于分配器的统一 owner API，
        // 发布给设备前必须拆成唯一 FrameTracker，避免 DMA 生命周期外提前回收。
        for frame in frames {
            let frame = Arc::try_unwrap(frame).expect("AHCI DMA frame has unexpected aliases");
            core::mem::forget(frame);
        }
        info!("ahci_dma_alloc: pa={:#x} pages={}", base, pages);
        (base, base)
    }

    fn dealloc_dma(va: usize, size: usize) {
        info!("dealloc_dma: {:x} {:x}", va, size);
        let pages = size.div_ceil(PAGE_SIZE);
        let mut pa = va;
        for _ in 0..pages {
            frame_dealloc(PhysAddr::from(pa).into());
            pa += PAGE_SIZE;
        }
    }
}

// 2K1000LA 手册规定片上 SATA 位于 Bus 0, Device 8, Function 0。
const PCI_CONFIG_PHYS_ADDRESS: usize = 0xFE_0000_0000;
const SATA_CONFIG_PHYS_ADDRESS: usize = PCI_CONFIG_PHYS_ADDRESS + (8 << 11);
const SATA_ABAR_PHYS_ADDRESS: u64 = 0x400e_0000;
const SATA_ABAR_SIZE: usize = 0x1_0000;
const LOONGSON_VENDOR_ID: u16 = 0x0014;
const LOONGSON_2K1000_SATA_DEVICE_ID: u16 = 0x7a08;
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

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum SataInitError {
    DeviceNotPresent { vendor: u16, device: u16 },
    WrongClass { class: u8, subclass: u8, prog_if: u8 },
    InvalidBar0 { raw_bar0: u32, raw_bar1: u32 },
    UnexpectedBar0 { found: u64, expected: u64 },
    PciCommandEnableFailed { command: u16 },
    Ahci(AhciError),
}

impl From<AhciError> for SataInitError {
    fn from(value: AhciError) -> Self {
        Self::Ahci(value)
    }
}

#[cfg(feature = "boot_la_uboot_dmw")]
fn sata_init() -> Result<AHCI<Provider>, SataInitError> {
    let config = cpu_mmio_addr(SATA_CONFIG_PHYS_ADDRESS);

    // SAFETY: boot_la_uboot_dmw 已建立 DMW2 SUC 窗口；00:08.0 配置空间是
    // 2K1000 固定板级资源。所有读取均为对齐 volatile 访问，且在驱动发布前串行执行。
    let id = unsafe { (config as *const u32).read_volatile() };
    let vendor = id as u16;
    let device = (id >> 16) as u16;
    if vendor != LOONGSON_VENDOR_ID || device != LOONGSON_2K1000_SATA_DEVICE_ID {
        return Err(SataInitError::DeviceNotPresent { vendor, device });
    }

    // SAFETY: 与上面的固定配置空间访问相同，寄存器偏移按 PCI Type 0 header 对齐。
    let class_reg = unsafe { ((config + 0x08) as *const u32).read_volatile() };
    let prog_if = (class_reg >> 8) as u8;
    let subclass = (class_reg >> 16) as u8;
    let class = (class_reg >> 24) as u8;
    if class != 0x01 || subclass != 0x06 || prog_if != 0x01 {
        return Err(SataInitError::WrongClass { class, subclass, prog_if });
    }

    // SAFETY: BAR0/BAR1 属于同一已验证的 Type 0 header，32-bit volatile 读取对齐。
    let raw_bar0 = unsafe { ((config + 0x10) as *const u32).read_volatile() };
    let raw_bar1 = unsafe { ((config + 0x14) as *const u32).read_volatile() };
    if raw_bar0 & 1 != 0 {
        return Err(SataInitError::InvalidBar0 { raw_bar0, raw_bar1 });
    }
    let abar = match (raw_bar0 >> 1) & 0x3 {
        0 => (raw_bar0 & !0xf) as u64,
        2 => ((raw_bar1 as u64) << 32) | ((raw_bar0 & !0xf) as u64),
        _ => return Err(SataInitError::InvalidBar0 { raw_bar0, raw_bar1 }),
    };
    if abar != SATA_ABAR_PHYS_ADDRESS {
        return Err(SataInitError::UnexpectedBar0 {
            found: abar,
            expected: SATA_ABAR_PHYS_ADDRESS,
        });
    }

    // PCI Command 与 write-one-to-clear Status 共用一个 DWORD，只能用 16-bit
    // read-modify-write 开启 Memory Space 与 Bus Master，不能恢复旧的 32-bit 写法。
    let command_ptr = (config + PCI_COMMAND as usize) as *mut u16;
    // SAFETY: command_ptr 是已验证配置头内的 16-bit 对齐 MMIO 寄存器。
    let original = unsafe { command_ptr.read_volatile() };
    let required = Command::MEMORY_SPACE.bits() | Command::BUS_MASTER.bits();
    if original & required != required {
        // SAFETY: 同一寄存器的串行 volatile 写；此时尚未向其它 CPU 发布驱动对象。
        unsafe { command_ptr.write_volatile(original | required) };
    }
    // SAFETY: 读回用于冲刷 posted write 并验证硬件接受了命令位。
    let command = unsafe { command_ptr.read_volatile() };
    if command & required != required {
        return Err(SataInitError::PciCommandEnableFailed { command });
    }

    println!(
        "[sata] pci 00:08.0 id={:04x}:{:04x} class={:02x}/{:02x}/{:02x} BAR0={:#x} command={:#06x}",
        vendor, device, class, subclass, prog_if, abar, command
    );
    AHCI::new(cpu_mmio_addr(abar as usize), SATA_ABAR_SIZE).map_err(Into::into)
}

#[cfg(not(feature = "boot_la_uboot_dmw"))]
unsafe fn enable_pci_command(loc: Location) -> Result<(), SataInitError> {
    let config = cpu_mmio_addr(PCI_CONFIG_PHYS_ADDRESS)
        | ((loc.bus as usize) << 16)
        | ((loc.device as usize) << 11)
        | ((loc.function as usize) << 8);
    let command_ptr = (config + PCI_COMMAND as usize) as *mut u16;
    let original = command_ptr.read_volatile();
    let required = Command::MEMORY_SPACE.bits() | Command::BUS_MASTER.bits();
    command_ptr.write_volatile(original | required);
    let command = command_ptr.read_volatile();
    if command & required != required {
        return Err(SataInitError::PciCommandEnableFailed { command });
    }
    Ok(())
}

#[cfg(not(feature = "boot_la_uboot_dmw"))]
fn sata_init() -> Result<AHCI<Provider>, SataInitError> {
    for dev in unsafe {
        scan_bus(
            &UnusedPort,
            CSpaceAccessMethod::MemoryMapped,
            cpu_mmio_addr(PCI_CONFIG_PHYS_ADDRESS),
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
                unsafe { enable_pci_command(dev.loc)? };
                return AHCI::new(cpu_mmio_addr(pa as usize), len as usize).map_err(Into::into);
            }
        }
    }
    Err(SataInitError::DeviceNotPresent {
        vendor: 0xffff,
        device: 0xffff,
    })
}
