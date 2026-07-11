//! Driver for AHCI
//!
//! Spec: https://www.intel.com/content/dam/www/public/us/en/documents/technical-specifications/serial-ata-ahci-spec-rev1-3-1.pdf

use alloc::string::String;
use core::hint::spin_loop;
use core::marker::PhantomData;
use core::mem::size_of;
use core::slice;
use core::sync::atomic::{fence, Ordering};

use bit_field::*;
use bitflags::*;
use volatile::Volatile;

use crate::provider::Provider;

///
pub struct AHCI<P: Provider> {
    header: usize,
    size: usize,
    provider: PhantomData<P>,
    ghc: &'static mut AHCIGenericHostControl,
    received_fis: &'static mut AHCIReceivedFIS,
    cmd_list: &'static mut [AHCICommandHeader],
    cmd_table: &'static mut AHCICommandTable,
    data: &'static mut [u8],
    port: &'static mut AHCIPort,
    port_num: usize,
    sectors: u64,
    serial: String,
    firmware: String,
    model: String,
}

/// AHCI initialization and polled-command failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AhciError {
    InvalidMmioSize {
        size: usize,
    },
    AhciEnableTimeout {
        ghc: u32,
    },
    ControllerResetTimeout {
        ghc: u32,
    },
    NoUsablePort {
        implemented: u32,
        port0_status: u32,
    },
    PortStopTimeout {
        port: usize,
        command: u32,
        mask: u32,
    },
    PortStartTimeout {
        port: usize,
        command: u32,
    },
    LinkTimeout {
        port: usize,
        sata_status: u32,
    },
    DeviceBusyTimeout {
        operation: &'static str,
        tfd: u32,
    },
    CommandTimeout {
        operation: &'static str,
        ci: u32,
        tfd: u32,
        interrupt_status: u32,
        sata_error: u32,
    },
    CommandFailed {
        operation: &'static str,
        tfd: u32,
        interrupt_status: u32,
        sata_error: u32,
    },
    InvalidBufferLength {
        expected: usize,
        actual: usize,
    },
    LbaOutOfRange {
        lba: u64,
        sectors: u64,
    },
    InvalidCapacity,
}

// These are bounded polling loops rather than time measurements because this
// reusable driver has no timer callback in Provider. On timeout the register
// snapshot is returned to the platform driver for diagnostics.
const REGISTER_POLL_LIMIT: usize = 10_000_000;
const COMMAND_POLL_LIMIT: usize = 50_000_000;

/// AHCI Generic Host Control (3.1)
#[repr(C)]
struct AHCIGenericHostControl {
    /// Host capability
    capability: Volatile<AHCICap>,
    /// Global host control
    global_host_control: Volatile<u32>,
    /// Interrupt status
    interrupt_status: Volatile<u32>,
    /// Port implemented
    port_implemented: Volatile<u32>,
    /// Version
    version: Volatile<u32>,
    /// Command completion coalescing control
    ccc_control: Volatile<u32>,
    /// Command completion coalescing ports
    ccc_ports: Volatile<u32>,
    /// Enclosure management location
    em_location: Volatile<u32>,
    /// Enclosure management control
    em_control: Volatile<u32>,
    /// Host capabilities extended
    capabilities2: Volatile<u32>,
    /// BIOS/OS handoff control and status
    bios_os_handoff_control: Volatile<u32>,
}

bitflags! {
    struct AHCICap : u32 {
        const S64A = 1 << 31;
        const SNCQ = 1 << 30;
        const SSNTF = 1 << 29;
        const SMPS = 1 << 28;
        const SSS = 1 << 27;
        const SALP = 1 << 26;
        const SAL = 1 << 25;
        const SCLO = 1 << 24;
        const ISS_GEN_1 = 1 << 20;
        const ISS_GEN_2 = 2 << 20;
        const ISS_GEN_3 = 3 << 20;
        const SAM = 1 << 18;
        const SPM = 1 << 17;
        const FBSS = 1 << 16;
        const PMD = 1 << 15;
        const SSC = 1 << 14;
        const PSC = 1 << 13;
        const CCCS = 1 << 7;
        const EMS = 1 << 6;
        const SXS = 1 << 5;
        // number of ports - 1
        const NUM_MASK = 0b11111;
    }
}

impl AHCIGenericHostControl {
    fn enable_ahci(&mut self) -> Result<(), AhciError> {
        // ref: Linux ahci_enable_ahci
        self.global_host_control.update(|v| {
            // GHC.AE
            v.set_bit(31, true);
        });
        for _ in 0..1000 {
            if self.global_host_control.read().get_bit(31) {
                return Ok(());
            }
            self.global_host_control.update(|v| {
                // GHC.AE
                v.set_bit(31, true);
            });
            spin_loop();
        }
        Err(AhciError::AhciEnableTimeout {
            ghc: self.global_host_control.read(),
        })
    }
    fn enable(&mut self) -> Result<(), AhciError> {
        // ref: Linux ahci_reset_controller
        self.enable_ahci()?;
        self.global_host_control.update(|v| {
            // Polling mode: disable global interrupts and request HBA reset.
            v.set_bit(1, false);
            v.set_bit(0, true);
        });
        // Flush the posted MMIO write.
        self.global_host_control.read();
        for _ in 0..REGISTER_POLL_LIMIT {
            if !self.global_host_control.read().get_bit(0) {
                return self.enable_ahci();
            }
            spin_loop();
        }
        Err(AhciError::ControllerResetTimeout {
            ghc: self.global_host_control.read(),
        })
    }
    fn num_ports(&self) -> usize {
        self.capability.read().bits().get_bits(0..5) as usize + 1
    }
    fn has_port(&self, port_num: usize) -> bool {
        self.port_implemented.read().get_bit(port_num)
    }
    fn port_ptr(&self, port_num: usize) -> *mut AHCIPort {
        (self as *const _ as usize + 0x100 + 0x80 * port_num) as *mut AHCIPort
    }
}

/// AHCI Port Registers (3.3) (one set per port)
#[repr(C)]
struct AHCIPort {
    command_list_base_address: Volatile<u64>,
    fis_base_address: Volatile<u64>,
    interrupt_status: Volatile<u32>,
    interrupt_enable: Volatile<u32>,
    command: Volatile<u32>,
    reserved: Volatile<u32>,
    task_file_data: Volatile<u32>,
    signature: Volatile<u32>,
    sata_status: Volatile<u32>,
    sata_control: Volatile<u32>,
    sata_error: Volatile<u32>,
    sata_active: Volatile<u32>,
    command_issue: Volatile<u32>,
    sata_notification: Volatile<u32>,
    fis_based_switch_control: Volatile<u32>,
}

impl AHCIPort {
    fn wait_command_clear(&mut self, port: usize, mask: u32) -> Result<(), AhciError> {
        for _ in 0..REGISTER_POLL_LIMIT {
            let command = self.command.read();
            if command & mask == 0 {
                return Ok(());
            }
            spin_loop();
        }
        Err(AhciError::PortStopTimeout {
            port,
            command: self.command.read(),
            mask,
        })
    }

    fn stop(&mut self, port: usize) -> Result<(), AhciError> {
        // AHCI 1.3.1 section 10.1.2: stop command processing before FIS RX.
        self.command.update(|c| {
            c.set_bit(0, false);
        });
        self.wait_command_clear(port, 1 << 15)?; // PxCMD.CR
        self.command.update(|c| {
            c.set_bit(4, false);
        });
        self.wait_command_clear(port, 1 << 14) // PxCMD.FR
    }

    fn start(&mut self, port: usize) -> Result<(), AhciError> {
        self.command.update(|c| {
            c.set_bit(4, true); // PxCMD.FRE
        });
        self.command.read();
        self.command.update(|c| {
            c.set_bit(0, true); // PxCMD.ST
        });
        self.command.read();
        for _ in 0..REGISTER_POLL_LIMIT {
            let command = self.command.read();
            if command.get_bit(0) && command.get_bit(4) {
                return Ok(());
            }
            spin_loop();
        }
        Err(AhciError::PortStartTimeout {
            port,
            command: self.command.read(),
        })
    }

    fn wait_ready(&mut self, operation: &'static str) -> Result<(), AhciError> {
        const ATA_DEV_BUSY: u32 = 1 << 7;
        const ATA_DEV_DRQ: u32 = 1 << 3;
        for _ in 0..REGISTER_POLL_LIMIT {
            let tfd = self.task_file_data.read();
            if tfd & (ATA_DEV_BUSY | ATA_DEV_DRQ) == 0 {
                return Ok(());
            }
            spin_loop();
        }
        Err(AhciError::DeviceBusyTimeout {
            operation,
            tfd: self.task_file_data.read(),
        })
    }

    fn wait_link_active(&mut self, port: usize) -> Result<(), AhciError> {
        for _ in 0..REGISTER_POLL_LIMIT {
            let status = self.sata_status.read();
            let det_present = status.get_bits(0..4) == 3;
            let ipm_active = status.get_bits(8..12) == 1;
            if det_present && ipm_active {
                return Ok(());
            }
            spin_loop();
        }
        Err(AhciError::LinkTimeout {
            port,
            sata_status: self.sata_status.read(),
        })
    }

    fn spin_on_slot(&mut self, slot: usize, operation: &'static str) -> Result<(), AhciError> {
        for _ in 0..COMMAND_POLL_LIMIT {
            let ci = self.command_issue.read();
            if !ci.get_bit(slot) {
                fence(Ordering::SeqCst);
                let tfd = self.task_file_data.read();
                let interrupt_status = self.interrupt_status.read();
                let sata_error = self.sata_error.read();
                const ATA_DEV_ERR: u32 = 1;
                const PORT_IRQ_TF_ERR: u32 = 1 << 30;
                if tfd & ATA_DEV_ERR != 0
                    || interrupt_status & PORT_IRQ_TF_ERR != 0
                    || sata_error != 0
                {
                    return Err(AhciError::CommandFailed {
                        operation,
                        tfd,
                        interrupt_status,
                        sata_error,
                    });
                }
                return Ok(());
            }
            spin_loop();
        }
        Err(AhciError::CommandTimeout {
            operation,
            ci: self.command_issue.read(),
            tfd: self.task_file_data.read(),
            interrupt_status: self.interrupt_status.read(),
            sata_error: self.sata_error.read(),
        })
    }
    fn issue_command(&mut self, slot: usize) {
        assert!(slot < 32);
        self.command_issue.write(1 << (slot as u32));
    }
}

/// AHCI Received FIS Structure (4.2.1)
#[repr(C)]
struct AHCIReceivedFIS {
    dma: [u8; 0x20],
    pio: [u8; 0x20],
    d2h: [u8; 0x18],
    sdbfis: [u8; 0x8],
    ufis: [u8; 0x40],
    reserved: [u8; 0x60],
}

/// # AHCI Command List Structure (4.2.2)
///
/// Host sends commands to the device through Command List.
///
/// Command List consists of 1 to 32 command headers, each one is called a slot.
///
/// Each command header describes an ATA or ATAPI command, including a
/// Command FIS, an ATAPI command buffer and a bunch of Physical Region
/// Descriptor Tables specifying the data payload address and size.
///
/// https://wiki.osdev.org/images/e/e8/Command_list.jpg
#[repr(C)]
struct AHCICommandHeader {
    /// PMP R C B R P W A CFL
    flags: u16,
    /// Physical region descriptor table length in entries
    prdt_length: u16,
    /// Physical region descriptor byte count transferred
    prd_byte_count: u32,
    /// Command table descriptor base address
    command_table_base_address: u64,
    /// Reserved
    reserved: [u32; 4],
}

bitflags! {
    struct CommandHeaderFlags: u16 {
        /// Command FIS length in DWORDS, 2 ~ 16
        const CFL_MASK = 0b11111;
        /// ATAPI
        const ATAPI = 1 << 5;
        /// Write, 1: H2D, 0: D2H
        const WRITE = 1 << 6;
        /// Prefetchable
        const PREFETCHABLE = 1 << 7;
        /// Reset
        const RESET = 1 << 8;
        /// BIST
        const BIST = 1 << 9;
        /// Clear busy upon R_OK
        const CLEAR = 1 << 10;
        /// Port multiplier port
        const PORT_MULTIPLIER_PORT_MASK = 0b1111 << 12;
    }
}

/// AHCI Command Table (4.2.3)
#[repr(C)]
struct AHCICommandTable {
    /// Command FIS
    cfis: SATAFISRegH2D,
    /// ATAPI command, 12 or 16 bytes
    acmd: [u8; 16],
    /// Reserved
    reserved: [u8; 48],
    /// Physical region descriptor table entries, 0 ~ 65535
    prdt: [AHCIPrdtEntry; 1],
}

/// Physical region descriptor table entry
#[repr(C)]
struct AHCIPrdtEntry {
    /// Data base address
    data_base_address: u64,
    /// Reserved
    reserved: u32,
    /// Bit 21-0: Byte count, 4M max
    /// Bit 31:   Interrupt on completion
    byte_count_i: u32,
}

const FIS_REG_H2D: u8 = 0x27;

const CMD_READ_DMA_EXT: u8 = 0x25;
const CMD_WRITE_DMA_EXT: u8 = 0x35;
const CMD_IDENTIFY_DEVICE: u8 = 0xec;
const CMD_FLUSH_CACHE_EXT: u8 = 0xea;

/// SATA Register FIS - Host to Device
///
/// https://wiki.osdev.org/AHCI Figure 5-2
#[repr(C)]
struct SATAFISRegH2D {
    fis_type: u8,
    cflags: u8,
    command: u8,
    feature_lo: u8,

    lba_0: u8, // LBA 7:0
    lba_1: u8, // LBA 15:8
    lba_2: u8, // LBA 23:16
    dev_head: u8,

    lba_3: u8, // LBA 31:24
    lba_4: u8, // LBA 39:32
    lba_5: u8, // LBA 47:40
    feature_hi: u8,

    sector_count: u16,
    reserved: u8,
    control: u8,

    _padding: [u8; 48],
}

impl SATAFISRegH2D {
    fn set_lba(&mut self, lba: u64) {
        self.lba_0 = (lba >> 0) as u8;
        self.lba_1 = (lba >> 8) as u8;
        self.lba_2 = (lba >> 16) as u8;
        self.lba_3 = (lba >> 24) as u8;
        self.lba_4 = (lba >> 32) as u8;
        self.lba_5 = (lba >> 40) as u8;
    }
}

/// IDENTIFY DEVICE data
///
/// ATA8-ACS Table 29
#[repr(C)]
struct ATAIdentifyPacket {
    _1: [u16; 10],
    serial: [u8; 20], // words 10-19
    _2: [u16; 3],
    firmware: [u8; 8], // words 23-26
    model: [u8; 40],   // words 27-46
    _3: [u16; 13],
    lba_sectors: u32, // words 60-61
    _4a: [u16; 21],
    command_set_support: u16, // word 83; bit 10 means 48-bit LBA support
    _4b: [u16; 16],
    lba48_sectors: u64, // words 100-103
}

impl<P: Provider> AHCI<P> {
    pub fn new(header: usize, size: usize) -> Result<Self, AhciError> {
        if size < 0x100 + size_of::<AHCIPort>() {
            return Err(AhciError::InvalidMmioSize { size });
        }
        let ghc = unsafe { &mut *(header as *mut AHCIGenericHostControl) };

        ghc.enable()?;

        // Some integrated controllers clear the writable PI register during
        // HBA reset. Restore only a bitmap supplied by the platform provider;
        // generic PCI controllers continue to use their hardware value.
        if let Some(port_map) = P::AHCI_PORTS_IMPLEMENTED {
            ghc.port_implemented.write(port_map);
            ghc.port_implemented.read(); // Flush the posted MMIO write.
        }

        assert_eq!(size_of::<SATAFISRegH2D>(), 64);
        assert_eq!(size_of::<AHCIReceivedFIS>(), 256);
        assert_eq!(size_of::<AHCICommandHeader>(), 32);

        let mapped_ports = (size - 0x100) / 0x80;
        let implemented = ghc.port_implemented.read();
        let port_count = ghc.num_ports().min(mapped_ports).min(32);
        // Select an implemented port before waiting for DET/IPM. Controllers
        // with staggered spin-up may not report DET=3 until PxCMD.SUD is set.
        let port_num =
            (0..port_count)
                .find(|&i| ghc.has_port(i))
                .ok_or(AhciError::NoUsablePort {
                    implemented,
                    port0_status: if port_count == 0 {
                        0
                    } else {
                        unsafe { &mut *ghc.port_ptr(0) }.sata_status.read()
                    },
                })?;
        let port = unsafe { &mut *ghc.port_ptr(port_num) };

        debug!("AHCI probing port {}", port_num);
        port.stop(port_num)?;

        let (rfis_va, rfis_pa) = P::alloc_dma(P::PAGE_SIZE);
        let (cmd_list_va, cmd_list_pa) = P::alloc_dma(P::PAGE_SIZE);
        let (cmd_table_va, cmd_table_pa) = P::alloc_dma(P::PAGE_SIZE);
        let (data_va, data_pa) = P::alloc_dma(P::PAGE_SIZE);

        let received_fis = unsafe { &mut *(rfis_va as *mut AHCIReceivedFIS) };
        let cmd_list = unsafe {
            slice::from_raw_parts_mut(
                cmd_list_va as *mut AHCICommandHeader,
                P::PAGE_SIZE / size_of::<AHCICommandHeader>(),
            )
        };
        let cmd_table = unsafe { &mut *(cmd_table_va as *mut AHCICommandTable) };
        let data = unsafe { slice::from_raw_parts_mut(data_va as *mut u8, BLOCK_SIZE) };

        cmd_table.prdt[0].data_base_address = data_pa as u64;
        cmd_table.prdt[0].reserved = 0;
        cmd_table.prdt[0].byte_count_i = (BLOCK_SIZE - 1) as u32;
        cmd_list[0].command_table_base_address = cmd_table_pa as u64;
        cmd_list[0].prdt_length = 1;
        cmd_list[0].prd_byte_count = 0;
        cmd_list[0].flags = 5; // Register H2D FIS is five DWORDs.

        port.command_list_base_address.write(cmd_list_pa as u64);
        port.fis_base_address.write(rfis_pa as u64);
        port.interrupt_enable.write(0);
        port.interrupt_status.write(u32::MAX);
        port.sata_error.write(u32::MAX);

        let mut ahci = AHCI {
            header,
            size,
            provider: PhantomData,
            ghc,
            received_fis,
            cmd_list,
            cmd_table,
            data,
            port,
            port_num,
            sectors: 0,
            serial: String::new(),
            firmware: String::new(),
            model: String::new(),
        };

        // Spin up the drive and request the active interface power state.
        ahci.port.command.update(|c| *c |= 1 << 1); // PxCMD.SUD
        ahci.port.command.update(|c| {
            *c &= !(0xf << 28);
            *c |= 1 << 28; // PxCMD.ICC = active
        });
        ahci.port.wait_link_active(port_num)?;
        // PxSIG is only a classification hint. 2K1000LA may return 0xffff_ffff
        // here after HBA reset even though PxSSTS reports an active SATA link;
        // the board U-Boot likewise proceeds from link-up to an ATA command
        // without rejecting the port by signature. IDENTIFY DEVICE below is
        // the authoritative, read-only test that an ATA disk is present.
        ahci.port.start(port_num)?;

        ahci.prepare_command(false, 1, CMD_IDENTIFY_DEVICE);
        ahci.cmd_table.cfis.sector_count = 1;
        ahci.execute_command("IDENTIFY DEVICE")?;

        let identify_data = unsafe { &*(ahci.data.as_ptr() as *const ATAIdentifyPacket) };
        let lba28_sectors = identify_data.lba_sectors as u64;
        let lba48_sectors = identify_data.lba48_sectors;
        let supports_lba48 = identify_data.command_set_support & (1 << 10) != 0;
        ahci.sectors = if supports_lba48 && lba48_sectors != 0 {
            lba48_sectors
        } else {
            lba28_sectors
        };
        if ahci.sectors == 0 {
            return Err(AhciError::InvalidCapacity);
        }
        ahci.serial = from_ata_string(&identify_data.serial);
        ahci.firmware = from_ata_string(&identify_data.firmware);
        ahci.model = from_ata_string(&identify_data.model);

        debug!(
            "Found ATA Device serial {} firmware {} model {} sectors={}",
            ahci.serial.trim_end(),
            ahci.firmware.trim_end(),
            ahci.model.trim_end(),
            ahci.sectors
        );
        Ok(ahci)
    }

    fn prepare_command(&mut self, write: bool, prdt_length: u16, command: u8) {
        unsafe {
            core::ptr::write_bytes(&mut self.cmd_table.cfis as *mut SATAFISRegH2D, 0, 1);
        }
        self.cmd_list[0].flags = 5 | if write {
            CommandHeaderFlags::WRITE.bits()
        } else {
            0
        };
        self.cmd_list[0].prdt_length = prdt_length;
        self.cmd_list[0].prd_byte_count = 0;
        self.cmd_table.prdt[0].byte_count_i = (BLOCK_SIZE - 1) as u32;
        let fis = &mut self.cmd_table.cfis;
        fis.fis_type = FIS_REG_H2D;
        fis.cflags = 1 << 7;
        fis.command = command;
    }

    fn execute_command(&mut self, operation: &'static str) -> Result<(), AhciError> {
        self.port.interrupt_status.write(u32::MAX);
        self.port.sata_error.write(u32::MAX);
        // Read back both W1C registers to flush posted clears before issuing CI.
        self.port.interrupt_status.read();
        self.port.sata_error.read();
        self.port.wait_ready(operation)?;
        fence(Ordering::SeqCst);
        self.port.issue_command(0);
        self.port.spin_on_slot(0, operation)
    }

    fn check_io(&self, lba: u64, actual: usize) -> Result<(), AhciError> {
        if actual != BLOCK_SIZE {
            return Err(AhciError::InvalidBufferLength {
                expected: BLOCK_SIZE,
                actual,
            });
        }
        if lba >= self.sectors {
            return Err(AhciError::LbaOutOfRange {
                lba,
                sectors: self.sectors,
            });
        }
        Ok(())
    }

    pub fn read_block(&mut self, block_id: usize, buf: &mut [u8]) -> Result<usize, AhciError> {
        let lba = block_id as u64;
        self.check_io(lba, buf.len())?;
        self.prepare_command(false, 1, CMD_READ_DMA_EXT);
        let fis = &mut self.cmd_table.cfis;
        fis.sector_count = 1;
        fis.dev_head = 0x40;
        fis.set_lba(lba);
        self.execute_command("READ DMA EXT")?;
        fence(Ordering::SeqCst);
        buf.copy_from_slice(self.data);
        Ok(BLOCK_SIZE)
    }

    pub fn write_block(&mut self, block_id: usize, buf: &[u8]) -> Result<usize, AhciError> {
        let lba = block_id as u64;
        self.check_io(lba, buf.len())?;
        self.data.copy_from_slice(buf);
        self.prepare_command(true, 1, CMD_WRITE_DMA_EXT);
        let fis = &mut self.cmd_table.cfis;
        fis.sector_count = 1;
        fis.dev_head = 0x40;
        fis.set_lba(lba);
        self.execute_command("WRITE DMA EXT")?;
        Ok(BLOCK_SIZE)
    }

    pub fn flush(&mut self) -> Result<(), AhciError> {
        self.prepare_command(false, 0, CMD_FLUSH_CACHE_EXT);
        self.execute_command("FLUSH CACHE EXT")
    }

    pub fn capacity_sectors(&self) -> u64 {
        self.sectors
    }

    pub fn capacity_bytes(&self) -> Option<u64> {
        self.sectors.checked_mul(BLOCK_SIZE as u64)
    }

    pub fn serial(&self) -> &str {
        self.serial.trim_end()
    }

    pub fn firmware(&self) -> &str {
        self.firmware.trim_end()
    }

    pub fn model(&self) -> &str {
        self.model.trim_end()
    }
}

impl<P: Provider> Drop for AHCI<P> {
    fn drop(&mut self) {
        // Never return DMA pages while the HBA may still own them. If stopping
        // the engine times out, leaking four pages is safer than a DMA use-after-free.
        if self.port.stop(self.port_num).is_err() {
            warn!(
                "AHCI port {} did not stop; retaining DMA pages",
                self.port_num
            );
            return;
        }
        P::dealloc_dma(self.received_fis as *mut _ as usize, P::PAGE_SIZE);
        P::dealloc_dma(self.cmd_list.as_ptr() as usize, P::PAGE_SIZE);
        P::dealloc_dma(self.cmd_table as *mut _ as usize, P::PAGE_SIZE);
        P::dealloc_dma(self.data.as_ptr() as usize, P::PAGE_SIZE);
    }
}

pub const BLOCK_SIZE: usize = 512;

fn from_ata_string(data: &[u8]) -> String {
    assert_eq!(data.len() % 2, 0);
    let mut value = String::new();
    for i in (0..data.len()).step_by(2) {
        for byte in [data[i + 1], data[i]] {
            value.push(if byte == b' ' || byte.is_ascii_graphic() {
                byte as char
            } else {
                '?'
            });
        }
    }
    value
}
