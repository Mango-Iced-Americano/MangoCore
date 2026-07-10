#[cfg(feature = "board_2k1000")]
use crate::config::HIGH_BASE_EIGHT;
use crate::config::PAGE_SIZE;
use crate::drivers::block::BlockDevice;
use crate::hal::BLOCK_SZ;
use crate::mm::{frame_alloc, frame_dealloc, PhysAddr};
use alloc::sync::Arc;
use isomorphic_drivers::{
    block::ahci::{AhciError, AHCI, BLOCK_SIZE},
    provider,
};
use log::info;
use pci::*;
use spin::Mutex;

#[inline(always)]
fn cpu_mmio_addr(physical: usize) -> usize {
    #[cfg(feature = "board_2k1000")]
    {
        // 2K1000 的 DMW2 将 VSEG=8 映射为强序非缓存区域。PCI/DMA 描述符仍需使用
        // 原始物理地址，只有 CPU 解引用 MMIO 寄存器时才使用该别名。
        return physical | HIGH_BASE_EIGHT;
    }
    #[cfg(not(feature = "board_2k1000"))]
    physical
}

pub struct SataBlock(Mutex<AHCI<Provider>>);

impl SataBlock {
    pub fn new() -> Self {
        Self(Mutex::new(sata_init().unwrap_or_else(|err| {
            panic!("SATA initialization failed: {:?}", err)
        })))
    }
}

impl BlockDevice for SataBlock {
    fn read_block(&self, mut block_id: usize, buf: &mut [u8]) {
        assert_eq!(
            buf.len() % BLOCK_SIZE,
            0,
            "SATA read must be sector aligned"
        );
        // 内核BLOCK_SZ为2048，SATA驱动中BLOCK_SIZE为512，四倍转化关系
        block_id = block_id * (BLOCK_SZ / BLOCK_SIZE);
        let mut controller = self.0.lock();
        for buf in buf.chunks_mut(BLOCK_SIZE) {
            controller
                .read_block(block_id, buf)
                .unwrap_or_else(|err| panic!("SATA read LBA {} failed: {:?}", block_id, err));
            block_id += 1;
        }
    }

    fn write_block(&self, mut block_id: usize, buf: &[u8]) {
        assert_eq!(
            buf.len() % BLOCK_SIZE,
            0,
            "SATA write must be sector aligned"
        );
        block_id = block_id * (BLOCK_SZ / BLOCK_SIZE);
        let mut controller = self.0.lock();
        for buf in buf.chunks(BLOCK_SIZE) {
            controller
                .write_block(block_id, buf)
                .unwrap_or_else(|err| panic!("SATA write LBA {} failed: {:?}", block_id, err));
            block_id += 1;
        }
        controller
            .flush()
            .unwrap_or_else(|err| panic!("SATA cache flush failed: {:?}", err));
    }

    fn size_bytes(&self) -> Option<u64> {
        self.0.lock().capacity_bytes()
    }
}

pub struct Provider;

impl provider::Provider for Provider {
    const PAGE_SIZE: usize = PAGE_SIZE;
    fn alloc_dma(size: usize) -> (usize, usize) {
        assert!(
            size > 0 && size <= PAGE_SIZE,
            "AHCI DMA allocation exceeds one page"
        );
        let frame = frame_alloc().expect("AHCI DMA frame allocation failed");
        let frame = Arc::try_unwrap(frame).expect("AHCI DMA frame has unexpected aliases");
        let frame_pa: PhysAddr = frame.ppn.into();
        let base: usize = frame_pa.into();
        assert!(
            base.checked_add(PAGE_SIZE)
                .map_or(false, |end| end <= 0x1_0000_0000),
            "2K1000 AHCI requires DMA memory below 4 GiB: {:#x}",
            base
        );
        // frame_alloc() returns a zeroed page. Keep it allocated until AHCI::drop().
        core::mem::forget(frame);
        info!("ahci_dma_alloc: pa={:#x} pages=1", base);
        (base, base)
    }

    fn dealloc_dma(va: usize, size: usize) {
        info!("dealloc_dma: {:x} {:x}", va, size);
        let pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
        let mut pa = va;
        for _ in 0..pages {
            frame_dealloc(PhysAddr::from(pa).into());
            pa += PAGE_SIZE;
        }
    }
}

// 2K1000LA manual: Bus 0, Device 8, Function 0 is the integrated SATA HBA.
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

#[derive(Debug)]
enum SataInitError {
    DeviceNotPresent {
        vendor: u16,
        device: u16,
    },
    WrongClass {
        class: u8,
        subclass: u8,
        prog_if: u8,
    },
    InvalidBar0 {
        raw_bar0: u32,
        raw_bar1: u32,
    },
    UnexpectedBar0 {
        found: u64,
        expected: u64,
    },
    PciCommandEnableFailed {
        command: u16,
    },
    Ahci(AhciError),
}

impl From<AhciError> for SataInitError {
    fn from(value: AhciError) -> Self {
        Self::Ahci(value)
    }
}

#[cfg(not(feature = "board_2k1000"))]
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

#[cfg(feature = "board_2k1000")]
fn sata_init() -> Result<AHCI<Provider>, SataInitError> {
    let config = cpu_mmio_addr(SATA_CONFIG_PHYS_ADDRESS);
    let id = unsafe { (config as *const u32).read_volatile() };
    let vendor = id as u16;
    let device = (id >> 16) as u16;
    if vendor != LOONGSON_VENDOR_ID || device != LOONGSON_2K1000_SATA_DEVICE_ID {
        return Err(SataInitError::DeviceNotPresent { vendor, device });
    }

    let class_reg = unsafe { ((config + 0x08) as *const u32).read_volatile() };
    let prog_if = (class_reg >> 8) as u8;
    let subclass = (class_reg >> 16) as u8;
    let class = (class_reg >> 24) as u8;
    if class != 0x01 || subclass != 0x06 || prog_if != 0x01 {
        return Err(SataInitError::WrongClass {
            class,
            subclass,
            prog_if,
        });
    }

    let raw_bar0 = unsafe { ((config + 0x10) as *const u32).read_volatile() };
    let raw_bar1 = unsafe { ((config + 0x14) as *const u32).read_volatile() };
    if raw_bar0 & 1 != 0 {
        return Err(SataInitError::InvalidBar0 { raw_bar0, raw_bar1 });
    }
    let bar_type = (raw_bar0 >> 1) & 0x3;
    let abar = if bar_type == 0x2 {
        ((raw_bar1 as u64) << 32) | ((raw_bar0 & !0xf) as u64)
    } else if bar_type == 0 {
        (raw_bar0 & !0xf) as u64
    } else {
        return Err(SataInitError::InvalidBar0 { raw_bar0, raw_bar1 });
    };
    if abar != SATA_ABAR_PHYS_ADDRESS {
        return Err(SataInitError::UnexpectedBar0 {
            found: abar,
            expected: SATA_ABAR_PHYS_ADDRESS,
        });
    }

    // PCI Command is a 16-bit register adjacent to write-one-to-clear Status.
    // A halfword access avoids accidentally acknowledging Status bits.
    let command_ptr = (config + PCI_COMMAND as usize) as *mut u16;
    let original_command = unsafe { command_ptr.read_volatile() };
    let required = Command::MEMORY_SPACE.bits() | Command::BUS_MASTER.bits();
    if original_command & required != required {
        unsafe { command_ptr.write_volatile(original_command | required) };
    }
    let command = unsafe { command_ptr.read_volatile() };
    if command & required != required {
        return Err(SataInitError::PciCommandEnableFailed { command });
    }

    println!(
        "[sata] pci 00:08.0 vendor={:#06x} device={:#06x} class={:02x}/{:02x}/{:02x} command={:#06x}",
        vendor, device, class, subclass, prog_if, command
    );
    println!(
        "[sata] BAR0={:#010x} BAR1={:#010x} ABAR={:#x} size={:#x}",
        raw_bar0, raw_bar1, abar, SATA_ABAR_SIZE
    );
    AHCI::new(cpu_mmio_addr(abar as usize), SATA_ABAR_SIZE).map_err(Into::into)
}

#[cfg(not(feature = "board_2k1000"))]
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

/// Run the 2K1000 SATA validation sequence without mounting or writing the SSD.
pub fn read_only_probe() {
    println!("[sata-probe] begin (IDENTIFY + repeated LBA0 read; no disk writes)");
    let mut controller = match sata_init() {
        Ok(controller) => controller,
        Err(err) => {
            println!("[sata-probe] controller initialization failed: {:?}", err);
            return;
        }
    };
    println!(
        "[sata-probe] ATA model='{}' serial='{}' firmware='{}' sectors={} bytes={}",
        controller.model(),
        controller.serial(),
        controller.firmware(),
        controller.capacity_sectors(),
        controller.capacity_bytes().unwrap_or(0)
    );

    let mut first = [0u8; BLOCK_SIZE];
    let mut second = [0u8; BLOCK_SIZE];
    if let Err(err) = controller.read_block(0, &mut first) {
        println!("[sata-probe] first LBA0 read failed: {:?}", err);
        return;
    }
    if let Err(err) = controller.read_block(0, &mut second) {
        println!("[sata-probe] second LBA0 read failed: {:?}", err);
        return;
    }
    if first != second {
        println!("[sata-probe] FAILED: repeated LBA0 reads differ");
        return;
    }
    print!("[sata-probe] LBA0 first 16 bytes:");
    for byte in &first[..16] {
        print!(" {:02x}", byte);
    }
    println!("");
    println!(
        "[sata-probe] PASS: repeated LBA0 reads match; MBR signature={:02x}{:02x}",
        first[510], first[511]
    );
}
