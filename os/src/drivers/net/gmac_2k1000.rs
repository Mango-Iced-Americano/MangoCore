//! 2K1000LA DesignWare GMAC handoff probe and GMAC0 polling driver.
//!
//! The Nebula board DTS identifies two DWMAC 3.70a instances at 0x4004_0000
//! and 0x4005_0000. Each controller owns an independent MDIO bus with a
//! Motorcomm YT8511H PHY at address 0. The `gmac_probe` path only reads the
//! handoff state left by U-Boot. The `gmac_2k1000` path resets GMAC0, configures
//! its PHY and alternate descriptor rings, then exposes a polling network
//! device to smoltcp.

use super::NetDevice;
use crate::config::{HIGH_BASE_EIGHT, PAGE_SIZE};
use crate::mm::{frame_alloc, FrameTracker, PhysAddr};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

const GMAC0_BASE: usize = 0x4004_0000;
const GMAC1_BASE: usize = 0x4005_0000;
const DMA_OFFSET: usize = 0x1000;

const MAC_CONFIG: usize = 0x0000;
const MAC_FRAME_FILTER: usize = 0x0004;
const MAC_MII_ADDR: usize = 0x0010;
const MAC_MII_DATA: usize = 0x0014;
const MAC_VERSION: usize = 0x0020;
const MAC_INT_STATUS: usize = 0x0038;
const MAC_INT_MASK: usize = 0x003c;
const MAC_ADDR0_HIGH: usize = 0x0040;
const MAC_ADDR0_LOW: usize = 0x0044;

const DMA_BUS_MODE: usize = 0x0000;
const DMA_RX_DESC_LIST: usize = 0x000c;
const DMA_TX_DESC_LIST: usize = 0x0010;
const DMA_STATUS: usize = 0x0014;
const DMA_OPERATION_MODE: usize = 0x0018;
const DMA_INT_ENABLE: usize = 0x001c;
const DMA_TX_POLL_DEMAND: usize = 0x0004;
const DMA_RX_POLL_DEMAND: usize = 0x0008;
const DMA_CURRENT_TX_DESC: usize = 0x0048;
const DMA_CURRENT_RX_DESC: usize = 0x004c;

const MAC_FRAME_BURST: u32 = 1 << 21;
const MAC_PORT_SELECT: u32 = 1 << 15;
const MAC_100M: u32 = 1 << 14;
const MAC_DISABLE_RX_OWN: u32 = 1 << 13;
const MAC_FULL_DUPLEX: u32 = 1 << 11;
const MAC_TX_ENABLE: u32 = 1 << 3;
const MAC_RX_ENABLE: u32 = 1 << 2;

const DMA_SOFT_RESET: u32 = 1 << 0;
const DMA_FIXED_BURST: u32 = 1 << 16;
const DMA_RX_TX_PRIORITY_4_1: u32 = 3 << 14;
const DMA_PBL_8: u32 = 8 << 8;
const DMA_STORE_AND_FORWARD: u32 = 1 << 21;
const DMA_FLUSH_TX_FIFO: u32 = 1 << 20;
const DMA_TX_START: u32 = 1 << 13;
const DMA_RX_START: u32 = 1 << 1;

const DESC_OWN: u32 = 1 << 31;
// The vendor U-Boot enables CONFIG_DW_ALTDESCRIPTOR for this board. In that
// layout TX framing/chain bits live in status and RX chain is control bit 14.
const TX_LAST: u32 = 1 << 29;
const TX_FIRST: u32 = 1 << 28;
const TX_CHAIN: u32 = 1 << 20;
const RX_ERROR: u32 = 1 << 15;
const RX_FIRST: u32 = 1 << 9;
const RX_LAST: u32 = 1 << 8;
const RX_FRAME_LEN_SHIFT: u32 = 16;
const RX_FRAME_LEN_MASK: u32 = 0x3fff << RX_FRAME_LEN_SHIFT;
const RX_CHAIN: u32 = 1 << 14;
const DESC_BUFFER_SIZE_MASK: u32 = 0x1fff;

const RX_DESC_COUNT: usize = 8;
const TX_DESC_COUNT: usize = 4;
const DMA_BUFFER_SIZE: usize = 2048;
const ETHERNET_FCS_SIZE: usize = 4;
const DESC_ALIGN: usize = 64;
const RX_DESC_OFFSET: usize = 0;
const TX_DESC_OFFSET: usize = RX_DESC_COUNT * DESC_ALIGN;

const MII_BUSY: u32 = 1 << 0;
const MII_CLOCK_MASK: u32 = 0x3c;
const MII_CLOCK_150_250_MHZ: u32 = 0x10;
const MII_PHY_SHIFT: u32 = 11;
const MII_REG_SHIFT: u32 = 6;

const PHY_ADDR: u8 = 0;
const MII_BMCR: u8 = 0;
const MII_BMSR: u8 = 1;
const MII_PHYSID1: u8 = 2;
const MII_PHYSID2: u8 = 3;
const MII_ADVERTISE: u8 = 4;
const MII_LPA: u8 = 5;
const MII_CTRL1000: u8 = 9;
const MII_STAT1000: u8 = 10;
const YT8511_SPEC_STATUS: u8 = 0x11;

const BMSR_LINK_STATUS: u16 = 1 << 2;
const BMSR_ANEG_COMPLETE: u16 = 1 << 5;
const YT8511_LINK_STATUS: u16 = 1 << 10;
const YT8511_DUPLEX: u16 = 1 << 13;
const YT8511_SPEED_SHIFT: u16 = 14;

const MII_BMCR_ANENABLE: u16 = 1 << 12;
const MII_BMCR_POWERDOWN: u16 = 1 << 11;
const MII_BMCR_ANRESTART: u16 = 1 << 9;
const YT8511_DEBUG_ADDR: u8 = 0x1e;
const YT8511_DEBUG_DATA: u8 = 0x1f;
const YT8511_SLEEP_CONTROL: u16 = 0x27;
const YT8511_RGMII_CONFIG: u16 = 0x0c;

#[derive(Clone, Copy, Debug)]
pub(crate) enum MdioError {
    BusyBeforeCommand,
    CommandTimeout,
}

#[derive(Debug)]
pub(crate) enum GmacError {
    UnsupportedMacVersion(u32),
    InvalidMacAddress([u8; 6]),
    DmaResetTimeout,
    DmaAddressAbove4G(usize),
    OutOfMemory,
    Mdio(MdioError),
    UnexpectedPhyId(u32),
}

impl From<MdioError> for GmacError {
    fn from(value: MdioError) -> Self {
        Self::Mdio(value)
    }
}

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct DmaDesc {
    status: u32,
    control: u32,
    buffer: u32,
    next: u32,
    padding: [u32; 12],
}

impl DmaDesc {
    const fn zeroed() -> Self {
        Self {
            status: 0,
            control: 0,
            buffer: 0,
            next: 0,
            padding: [0; 12],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LinkState {
    up: bool,
    speed_mbps: u16,
    full_duplex: bool,
}

struct GmacInner {
    base: usize,
    mac: [u8; 6],
    descriptor_frame: Arc<FrameTracker>,
    rx_frames: Vec<Arc<FrameTracker>>,
    tx_frames: Vec<Arc<FrameTracker>>,
    rx_index: usize,
    tx_index: usize,
    link: LinkState,
    last_link_poll: usize,
    #[cfg(feature = "gmac_diag")]
    last_diag_poll: usize,
    #[cfg(feature = "gmac_diag")]
    diag_polls: usize,
    #[cfg(feature = "gmac_diag")]
    rx_packets: usize,
    #[cfg(feature = "gmac_diag")]
    tx_packets: usize,
    #[cfg(feature = "gmac_diag")]
    tx_busy: usize,
}

pub struct Gmac2k1000(Mutex<GmacInner>);

#[inline(always)]
fn reg_addr(base: usize, offset: usize) -> usize {
    (base + offset) | HIGH_BASE_EIGHT
}

#[inline(always)]
fn read_reg(base: usize, offset: usize) -> u32 {
    // Safety: the board DTS reserves a 32 KiB MMIO window at each base. DMW2
    // maps VSEG=8 as strongly ordered uncached memory for device accesses.
    unsafe { core::ptr::read_volatile(reg_addr(base, offset) as *const u32) }
}

#[inline(always)]
fn write_reg(base: usize, offset: usize, value: u32) {
    // Safety: same MMIO ownership and DMW2 mapping requirements as read_reg.
    unsafe {
        core::ptr::write_volatile(reg_addr(base, offset) as *mut u32, value);
        core::arch::asm!("dbar 0", options(nostack, preserves_flags));
    }
}

fn wait_mii_idle(base: usize) -> bool {
    let start = crate::hal::get_time();
    let timeout = (crate::hal::get_clock_freq() / 10).max(1);
    while read_reg(base, MAC_MII_ADDR) & MII_BUSY != 0 {
        if crate::hal::get_time().wrapping_sub(start) >= timeout {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

fn mdio_read(base: usize, phy: u8, reg: u8) -> Result<u16, MdioError> {
    if !wait_mii_idle(base) {
        return Err(MdioError::BusyBeforeCommand);
    }

    // Preserve the clock divider selected by U-Boot. A cleared handoff value
    // falls back to the divider used by the vendor U-Boot DesignWare driver.
    let inherited_clock = read_reg(base, MAC_MII_ADDR) & MII_CLOCK_MASK;
    let clock = if inherited_clock == 0 {
        MII_CLOCK_150_250_MHZ
    } else {
        inherited_clock
    };
    let command =
        ((phy as u32) << MII_PHY_SHIFT) | ((reg as u32) << MII_REG_SHIFT) | clock | MII_BUSY;
    write_reg(base, MAC_MII_ADDR, command);
    if !wait_mii_idle(base) {
        return Err(MdioError::CommandTimeout);
    }
    Ok(read_reg(base, MAC_MII_DATA) as u16)
}

fn mdio_write(base: usize, phy: u8, reg: u8, value: u16) -> Result<(), MdioError> {
    if !wait_mii_idle(base) {
        return Err(MdioError::BusyBeforeCommand);
    }
    let inherited_clock = read_reg(base, MAC_MII_ADDR) & MII_CLOCK_MASK;
    let clock = if inherited_clock == 0 {
        MII_CLOCK_150_250_MHZ
    } else {
        inherited_clock
    };
    write_reg(base, MAC_MII_DATA, value as u32);
    let command = ((phy as u32) << MII_PHY_SHIFT)
        | ((reg as u32) << MII_REG_SHIFT)
        | clock
        | (1 << 1)
        | MII_BUSY;
    write_reg(base, MAC_MII_ADDR, command);
    if !wait_mii_idle(base) {
        return Err(MdioError::CommandTimeout);
    }
    Ok(())
}

fn yt8511_ext_read(base: usize, reg: u16) -> Result<u16, MdioError> {
    mdio_write(base, PHY_ADDR, YT8511_DEBUG_ADDR, reg)?;
    mdio_read(base, PHY_ADDR, YT8511_DEBUG_DATA)
}

fn yt8511_ext_write(base: usize, reg: u16, value: u16) -> Result<(), MdioError> {
    mdio_write(base, PHY_ADDR, YT8511_DEBUG_ADDR, reg)?;
    mdio_write(base, PHY_ADDR, YT8511_DEBUG_DATA, value)
}

#[inline(always)]
fn dma_barrier() {
    // The vendor LoongArch U-Boot uses dbar for this coherent GMAC DMA path.
    unsafe { core::arch::asm!("dbar 0", options(nostack, preserves_flags)) }
}

fn frame_address(frame: &Arc<FrameTracker>) -> Result<usize, GmacError> {
    let pa: PhysAddr = frame.ppn.into();
    let address: usize = pa.into();
    if address
        .checked_add(PAGE_SIZE)
        .map_or(true, |end| end > 0x1_0000_0000)
    {
        return Err(GmacError::DmaAddressAbove4G(address));
    }
    Ok(address)
}

fn read_mac(base: usize) -> [u8; 6] {
    let high = read_reg(base, MAC_ADDR0_HIGH);
    let low = read_reg(base, MAC_ADDR0_LOW);
    [
        low as u8,
        (low >> 8) as u8,
        (low >> 16) as u8,
        (low >> 24) as u8,
        high as u8,
        (high >> 8) as u8,
    ]
}

fn valid_mac(mac: [u8; 6]) -> bool {
    mac[0] & 1 == 0 && mac != [0; 6] && mac != [0xff; 6]
}

fn write_mac(base: usize, mac: [u8; 6]) {
    let low = (mac[0] as u32)
        | ((mac[1] as u32) << 8)
        | ((mac[2] as u32) << 16)
        | ((mac[3] as u32) << 24);
    let high = (mac[4] as u32) | ((mac[5] as u32) << 8);
    write_reg(base, MAC_ADDR0_HIGH, high);
    write_reg(base, MAC_ADDR0_LOW, low);
}

fn configure_yt8511(base: usize) -> Result<u32, GmacError> {
    let id1 = mdio_read(base, PHY_ADDR, MII_PHYSID1)? as u32;
    let id2 = mdio_read(base, PHY_ADDR, MII_PHYSID2)? as u32;
    let phy_id = (id1 << 16) | id2;
    if phy_id & 0x0fff != 0x010a {
        return Err(GmacError::UnexpectedPhyId(phy_id));
    }

    // Match the vendor Linux YT8511H setup: disable auto-sleep, keep RXC
    // available without a cable, output 125 MHz and select the board's RGMII
    // TX delay. U-Boot normally leaves these values configured, but the kernel
    // must not rely on firmware state for later warm/cold boot parity.
    let sleep = yt8511_ext_read(base, YT8511_SLEEP_CONTROL)?;
    yt8511_ext_write(base, YT8511_SLEEP_CONTROL, sleep & !(1 << 15))?;
    yt8511_ext_write(base, 0xa000, 0)?;
    let rgmii = yt8511_ext_read(base, YT8511_RGMII_CONFIG)?;
    yt8511_ext_write(
        base,
        YT8511_RGMII_CONFIG,
        (rgmii & !(1 << 12)) | (1 << 1) | (1 << 2) | (1 << 7),
    )?;

    let mut bmcr = mdio_read(base, PHY_ADDR, MII_BMCR)?;
    bmcr &= !MII_BMCR_POWERDOWN;
    bmcr |= MII_BMCR_ANENABLE;
    let bmsr = mdio_read(base, PHY_ADDR, MII_BMSR)?;
    let bmsr = mdio_read(base, PHY_ADDR, MII_BMSR).unwrap_or(bmsr);
    if bmsr & BMSR_LINK_STATUS == 0 {
        bmcr |= MII_BMCR_ANRESTART;
    }
    mdio_write(base, PHY_ADDR, MII_BMCR, bmcr)?;
    Ok(phy_id)
}

fn read_link_state(base: usize) -> Result<LinkState, MdioError> {
    let _ = mdio_read(base, PHY_ADDR, MII_BMSR)?;
    let bmsr = mdio_read(base, PHY_ADDR, MII_BMSR)?;
    let status = mdio_read(base, PHY_ADDR, YT8511_SPEC_STATUS)?;
    let up = bmsr & BMSR_LINK_STATUS != 0 && status & YT8511_LINK_STATUS != 0;
    let speed_mbps = match status >> YT8511_SPEED_SHIFT {
        0 => 10,
        1 => 100,
        2 => 1000,
        _ => 0,
    };
    Ok(LinkState {
        up,
        speed_mbps,
        full_duplex: status & YT8511_DUPLEX != 0,
    })
}

fn apply_link_state(base: usize, state: LinkState) {
    let mut config = read_reg(base, MAC_CONFIG);
    config &= !(MAC_PORT_SELECT | MAC_100M | MAC_FULL_DUPLEX);
    config |= MAC_FRAME_BURST | MAC_DISABLE_RX_OWN;
    match state.speed_mbps {
        10 => config |= MAC_PORT_SELECT,
        100 => config |= MAC_PORT_SELECT | MAC_100M,
        _ => {}
    }
    if state.full_duplex {
        config |= MAC_FULL_DUPLEX;
    }
    write_reg(base, MAC_CONFIG, config);
}

fn wait_initial_link(base: usize) -> LinkState {
    let start = crate::hal::get_time();
    let timeout = crate::hal::get_clock_freq().saturating_mul(3);
    loop {
        if let Ok(state) = read_link_state(base) {
            if state.up {
                return state;
            }
        }
        if crate::hal::get_time().wrapping_sub(start) >= timeout {
            return LinkState {
                up: false,
                speed_mbps: 1000,
                full_duplex: true,
            };
        }
        core::hint::spin_loop();
    }
}

impl GmacInner {
    fn descriptor_base(&self) -> usize {
        frame_address(&self.descriptor_frame).expect("validated GMAC descriptor frame")
    }

    fn rx_desc(&self, index: usize) -> *mut DmaDesc {
        (self.descriptor_base() + RX_DESC_OFFSET + index * DESC_ALIGN) as *mut DmaDesc
    }

    fn tx_desc(&self, index: usize) -> *mut DmaDesc {
        (self.descriptor_base() + TX_DESC_OFFSET + index * DESC_ALIGN) as *mut DmaDesc
    }

    fn poll_link(&mut self) {
        let now = crate::hal::get_time();
        if now.wrapping_sub(self.last_link_poll) < crate::hal::get_clock_freq() {
            return;
        }
        self.last_link_poll = now;
        if let Ok(state) = read_link_state(self.base) {
            if state != self.link {
                apply_link_state(self.base, state);
                println!(
                    "[gmac] link {} speed={}M duplex={}",
                    if state.up { "up" } else { "down" },
                    state.speed_mbps,
                    if state.full_duplex { "full" } else { "half" }
                );
                self.link = state;
            }
        }
    }

    #[cfg(feature = "gmac_diag")]
    fn poll_diag(&mut self) {
        if self.diag_polls >= 8 {
            return;
        }
        let now = crate::hal::get_time();
        if now.wrapping_sub(self.last_diag_poll) < crate::hal::get_clock_freq() {
            return;
        }
        self.last_diag_poll = now;
        self.diag_polls += 1;
        let dma = self.base + DMA_OFFSET;
        let rx_desc = self.rx_desc(self.rx_index);
        let tx_desc = self.tx_desc(self.tx_index);
        dma_barrier();
        // Safety: ring frames remain owned by the driver for its lifetime.
        let rx_status = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*rx_desc).status)) };
        let tx_status = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*tx_desc).status)) };
        println!(
            "[gmac-diag] dma={:#010x} cur_rx={:#010x} cur_tx={:#010x} rx[{}]={:#010x} tx[{}]={:#010x} counts rx={} tx={} busy={}",
            read_reg(dma, DMA_STATUS),
            read_reg(dma, DMA_CURRENT_RX_DESC),
            read_reg(dma, DMA_CURRENT_TX_DESC),
            self.rx_index,
            rx_status,
            self.tx_index,
            tx_status,
            self.rx_packets,
            self.tx_packets,
            self.tx_busy
        );
    }

    fn receive(&mut self, output: &mut [u8]) -> Option<usize> {
        self.poll_link();
        #[cfg(feature = "gmac_diag")]
        self.poll_diag();
        for _ in 0..RX_DESC_COUNT {
            let desc = self.rx_desc(self.rx_index);
            dma_barrier();
            // Safety: descriptor_frame is retained by self and the current RX
            // descriptor is CPU-owned whenever OWN is clear.
            let status = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*desc).status)) };
            if status & DESC_OWN != 0 {
                return None;
            }

            let raw_len = ((status & RX_FRAME_LEN_MASK) >> RX_FRAME_LEN_SHIFT) as usize;
            // The observed 2K1000LA descriptor length includes the four-byte
            // FCS (a 98-byte ICMP Ethernet frame is reported as 102 bytes).
            let valid = status & RX_ERROR == 0
                && status & RX_FIRST != 0
                && status & RX_LAST != 0
                && raw_len >= 14 + ETHERNET_FCS_SIZE
                && raw_len <= DMA_BUFFER_SIZE;
            let len = raw_len.saturating_sub(ETHERNET_FCS_SIZE).min(output.len());
            if valid {
                #[cfg(feature = "gmac_diag")]
                if self.rx_packets < 8 {
                    println!(
                        "[gmac-diag] RX index={} status={:#010x} len={}",
                        self.rx_index, status, raw_len
                    );
                }
                let buffer = frame_address(&self.rx_frames[self.rx_index]).ok()?;
                dma_barrier();
                // Safety: each RX frame is retained exclusively by this ring;
                // len is bounded by both the hardware size and output slice.
                unsafe {
                    core::ptr::copy_nonoverlapping(buffer as *const u8, output.as_mut_ptr(), len)
                };
            }

            dma_barrier();
            // Safety: after copying, returning OWN is the final CPU write to
            // this descriptor until the DMA engine completes another frame.
            unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!((*desc).status), DESC_OWN) };
            dma_barrier();
            self.rx_index = (self.rx_index + 1) % RX_DESC_COUNT;
            write_reg(self.base + DMA_OFFSET, DMA_RX_POLL_DEMAND, u32::MAX);
            if valid {
                #[cfg(feature = "gmac_diag")]
                if self.rx_packets == 0 {
                    dma_barrier();
                    println!(
                        "[gmac-diag] RX advance hw_cur={:#010x} next_index={}",
                        read_reg(self.base + DMA_OFFSET, DMA_CURRENT_RX_DESC),
                        self.rx_index
                    );
                    for index in 0..RX_DESC_COUNT {
                        let ring_desc = self.rx_desc(index);
                        // Safety: the descriptor ring remains allocated.
                        let ring_status = unsafe {
                            core::ptr::read_volatile(core::ptr::addr_of!((*ring_desc).status))
                        };
                        println!("[gmac-diag] RX ring[{}]={:#010x}", index, ring_status);
                    }
                }
                #[cfg(feature = "gmac_diag")]
                {
                    self.rx_packets += 1;
                }
                return Some(len);
            }
        }
        None
    }

    fn transmit(&mut self, input: &[u8]) {
        self.poll_link();
        if !self.link.up || input.is_empty() || input.len() > DMA_BUFFER_SIZE {
            return;
        }
        let desc = self.tx_desc(self.tx_index);
        dma_barrier();
        // Safety: descriptor_frame remains allocated for the driver's lifetime.
        let status = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*desc).status)) };
        if status & DESC_OWN != 0 {
            #[cfg(feature = "gmac_diag")]
            {
                self.tx_busy += 1;
            }
            return;
        }

        let buffer = match frame_address(&self.tx_frames[self.tx_index]) {
            Ok(address) => address,
            Err(_) => return,
        };
        let tx_len = input.len().max(60);
        // Safety: the TX frame is a private 4 KiB allocation, tx_len <= 2047,
        // and all bytes are initialized before ownership transfers to DMA.
        unsafe {
            core::ptr::copy_nonoverlapping(input.as_ptr(), buffer as *mut u8, input.len());
            if tx_len > input.len() {
                core::ptr::write_bytes((buffer + input.len()) as *mut u8, 0, tx_len - input.len());
            }
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*desc).control),
                tx_len as u32 & DESC_BUFFER_SIZE_MASK,
            );
        }
        dma_barrier();
        // Safety: OWN is written last, after buffer and descriptor contents.
        unsafe {
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*desc).status),
                DESC_OWN | TX_FIRST | TX_LAST | TX_CHAIN,
            )
        };
        dma_barrier();
        self.tx_index = (self.tx_index + 1) % TX_DESC_COUNT;
        write_reg(self.base + DMA_OFFSET, DMA_TX_POLL_DEMAND, u32::MAX);
        #[cfg(feature = "gmac_diag")]
        if self.tx_packets < 8 {
            let start = crate::hal::get_time();
            let timeout = (crate::hal::get_clock_freq() / 100).max(1);
            let final_status = loop {
                dma_barrier();
                // Safety: the submitted descriptor remains in the TX ring.
                let current =
                    unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*desc).status)) };
                if current & DESC_OWN == 0 || crate::hal::get_time().wrapping_sub(start) >= timeout
                {
                    break current;
                }
                core::hint::spin_loop();
            };
            println!(
                "[gmac-diag] TX index={} len={} previous={:#010x} final={:#010x} dma={:#010x}",
                (self.tx_index + TX_DESC_COUNT - 1) % TX_DESC_COUNT,
                tx_len,
                status,
                final_status,
                read_reg(self.base + DMA_OFFSET, DMA_STATUS)
            );
        }
        #[cfg(feature = "gmac_diag")]
        {
            self.tx_packets += 1;
        }
    }
}

impl Gmac2k1000 {
    pub(crate) fn new() -> Result<Self, GmacError> {
        let base = GMAC0_BASE;
        let version = read_reg(base, MAC_VERSION);
        if version & 0xff != 0x37 {
            return Err(GmacError::UnsupportedMacVersion(version));
        }
        let mac = read_mac(base);
        if !valid_mac(mac) {
            return Err(GmacError::InvalidMacAddress(mac));
        }

        let descriptor_frame = frame_alloc().ok_or(GmacError::OutOfMemory)?;
        let descriptor_base = frame_address(&descriptor_frame)?;
        debug_assert!(TX_DESC_OFFSET + TX_DESC_COUNT * DESC_ALIGN <= PAGE_SIZE);
        let mut rx_frames = Vec::with_capacity(RX_DESC_COUNT);
        let mut tx_frames = Vec::with_capacity(TX_DESC_COUNT);
        for _ in 0..RX_DESC_COUNT {
            let frame = frame_alloc().ok_or(GmacError::OutOfMemory)?;
            frame_address(&frame)?;
            rx_frames.push(frame);
        }
        for _ in 0..TX_DESC_COUNT {
            let frame = frame_alloc().ok_or(GmacError::OutOfMemory)?;
            frame_address(&frame)?;
            tx_frames.push(frame);
        }

        let dma = base + DMA_OFFSET;
        write_reg(
            base,
            MAC_CONFIG,
            read_reg(base, MAC_CONFIG) & !(MAC_RX_ENABLE | MAC_TX_ENABLE),
        );
        write_reg(dma, DMA_OPERATION_MODE, 0);
        write_reg(dma, DMA_INT_ENABLE, 0);
        write_reg(
            dma,
            DMA_BUS_MODE,
            read_reg(dma, DMA_BUS_MODE) | DMA_SOFT_RESET,
        );
        let reset_start = crate::hal::get_time();
        let reset_timeout = crate::hal::get_clock_freq();
        while read_reg(dma, DMA_BUS_MODE) & DMA_SOFT_RESET != 0 {
            if crate::hal::get_time().wrapping_sub(reset_start) >= reset_timeout {
                return Err(GmacError::DmaResetTimeout);
            }
            core::hint::spin_loop();
        }

        write_mac(base, mac);
        let phy_id = configure_yt8511(base)?;

        for index in 0..RX_DESC_COUNT {
            let buffer = frame_address(&rx_frames[index])? as u32;
            let next = (descriptor_base
                + RX_DESC_OFFSET
                + ((index + 1) % RX_DESC_COUNT) * DESC_ALIGN) as u32;
            let desc = (descriptor_base + RX_DESC_OFFSET + index * DESC_ALIGN) as *mut DmaDesc;
            let mut value = DmaDesc::zeroed();
            value.status = DESC_OWN;
            value.control = RX_CHAIN | (DMA_BUFFER_SIZE as u32 & DESC_BUFFER_SIZE_MASK);
            value.buffer = buffer;
            value.next = next;
            // Safety: descriptor_frame is page-aligned and each slot is 64-byte
            // aligned and disjoint during initialization.
            unsafe { core::ptr::write_volatile(desc, value) };
        }
        for index in 0..TX_DESC_COUNT {
            let buffer = frame_address(&tx_frames[index])? as u32;
            let next = (descriptor_base
                + TX_DESC_OFFSET
                + ((index + 1) % TX_DESC_COUNT) * DESC_ALIGN) as u32;
            let desc = (descriptor_base + TX_DESC_OFFSET + index * DESC_ALIGN) as *mut DmaDesc;
            let mut value = DmaDesc::zeroed();
            value.status = TX_CHAIN;
            value.buffer = buffer;
            value.next = next;
            // Safety: same disjoint descriptor-slot guarantee as RX above.
            unsafe { core::ptr::write_volatile(desc, value) };
        }
        dma_barrier();

        write_reg(
            dma,
            DMA_RX_DESC_LIST,
            (descriptor_base + RX_DESC_OFFSET) as u32,
        );
        write_reg(
            dma,
            DMA_TX_DESC_LIST,
            (descriptor_base + TX_DESC_OFFSET) as u32,
        );
        write_reg(
            dma,
            DMA_BUS_MODE,
            DMA_FIXED_BURST | DMA_RX_TX_PRIORITY_4_1 | DMA_PBL_8,
        );
        write_reg(dma, DMA_STATUS, u32::MAX);
        write_reg(base, MAC_FRAME_FILTER, 0);
        write_reg(
            dma,
            DMA_OPERATION_MODE,
            DMA_STORE_AND_FORWARD | DMA_FLUSH_TX_FIFO | DMA_RX_START | DMA_TX_START,
        );

        let link = wait_initial_link(base);
        apply_link_state(base, link);
        write_reg(
            base,
            MAC_CONFIG,
            read_reg(base, MAC_CONFIG) | MAC_RX_ENABLE | MAC_TX_ENABLE,
        );
        println!(
            "[gmac] gmac0 DWMAC={:#x} PHY={:#x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            version, phy_id, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );
        println!(
            "[gmac] rings desc={:#x} rx={} tx={} link={} {}M {}",
            descriptor_base,
            RX_DESC_COUNT,
            TX_DESC_COUNT,
            if link.up { "up" } else { "down" },
            link.speed_mbps,
            if link.full_duplex { "full" } else { "half" }
        );

        Ok(Self(Mutex::new(GmacInner {
            base,
            mac,
            descriptor_frame,
            rx_frames,
            tx_frames,
            rx_index: 0,
            tx_index: 0,
            link,
            last_link_poll: crate::hal::get_time(),
            #[cfg(feature = "gmac_diag")]
            last_diag_poll: crate::hal::get_time(),
            #[cfg(feature = "gmac_diag")]
            diag_polls: 0,
            #[cfg(feature = "gmac_diag")]
            rx_packets: 0,
            #[cfg(feature = "gmac_diag")]
            tx_packets: 0,
            #[cfg(feature = "gmac_diag")]
            tx_busy: 0,
        })))
    }
}

impl NetDevice for Gmac2k1000 {
    fn receive(&self, buf: &mut [u8]) -> Option<usize> {
        self.0.lock().receive(buf)
    }

    fn transmit(&self, buf: &[u8]) {
        self.0.lock().transmit(buf)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.0.lock().mac
    }
}

fn read_phy_registers(base: usize) {
    let id1 = mdio_read(base, PHY_ADDR, MII_PHYSID1);
    let id2 = mdio_read(base, PHY_ADDR, MII_PHYSID2);
    println!("[gmac-probe] phy0 id1={:?} id2={:?}", id1, id2);

    let bmcr = mdio_read(base, PHY_ADDR, MII_BMCR);
    // BMSR link and auto-negotiation bits are latched low, so the second read
    // is the current state while the first read preserves diagnostic history.
    let bmsr_latched = mdio_read(base, PHY_ADDR, MII_BMSR);
    let bmsr_current = mdio_read(base, PHY_ADDR, MII_BMSR);
    let advertise = mdio_read(base, PHY_ADDR, MII_ADVERTISE);
    let partner = mdio_read(base, PHY_ADDR, MII_LPA);
    let ctrl1000 = mdio_read(base, PHY_ADDR, MII_CTRL1000);
    let stat1000 = mdio_read(base, PHY_ADDR, MII_STAT1000);
    let specific = mdio_read(base, PHY_ADDR, YT8511_SPEC_STATUS);
    println!(
        "[gmac-probe] phy0 bmcr={:?} bmsr(latched/current)={:?}/{:?}",
        bmcr, bmsr_latched, bmsr_current
    );
    println!(
        "[gmac-probe] phy0 advertise={:?} lpa={:?} ctrl1000={:?} stat1000={:?}",
        advertise, partner, ctrl1000, stat1000
    );

    match (bmsr_current, specific) {
        (Ok(bmsr), Ok(status)) => {
            let standard_link = bmsr & BMSR_LINK_STATUS != 0;
            let auto_negotiated = bmsr & BMSR_ANEG_COMPLETE != 0;
            let vendor_link = status & YT8511_LINK_STATUS != 0;
            let duplex = if status & YT8511_DUPLEX != 0 {
                "full"
            } else {
                "half"
            };
            let speed = match status >> YT8511_SPEED_SHIFT {
                0 => "10M",
                1 => "100M",
                2 => "1000M",
                _ => "unknown",
            };
            println!(
                "[gmac-probe] phy0 status={:#06x} link(bmsr/yt)={}/{} aneg={} speed={} duplex={}",
                status, standard_link, vendor_link, auto_negotiated, speed, duplex
            );
        }
        (_, Err(err)) => println!("[gmac-probe] phy0 YT8511 status read failed: {:?}", err),
        (Err(err), _) => println!("[gmac-probe] phy0 BMSR read failed: {:?}", err),
    }
}

fn probe_one(index: usize, base: usize) {
    let mac_hi = read_reg(base, MAC_ADDR0_HIGH);
    let mac_lo = read_reg(base, MAC_ADDR0_LOW);
    let mac = [
        mac_lo as u8,
        (mac_lo >> 8) as u8,
        (mac_lo >> 16) as u8,
        (mac_lo >> 24) as u8,
        mac_hi as u8,
        (mac_hi >> 8) as u8,
    ];
    println!(
        "[gmac-probe] gmac{} base={:#x} version={:#010x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        index, base, read_reg(base, MAC_VERSION), mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    println!(
        "[gmac-probe] gmac{} mac_cfg={:#010x} frame_filter={:#010x} irq={:#010x} mask={:#010x}",
        index,
        read_reg(base, MAC_CONFIG),
        read_reg(base, MAC_FRAME_FILTER),
        read_reg(base, MAC_INT_STATUS),
        read_reg(base, MAC_INT_MASK)
    );
    let dma = base + DMA_OFFSET;
    println!(
        "[gmac-probe] gmac{} dma bus={:#010x} op={:#010x} status={:#010x} irq_en={:#010x}",
        index,
        read_reg(dma, DMA_BUS_MODE),
        read_reg(dma, DMA_OPERATION_MODE),
        read_reg(dma, DMA_STATUS),
        read_reg(dma, DMA_INT_ENABLE)
    );
    println!(
        "[gmac-probe] gmac{} dma rx_desc={:#010x} tx_desc={:#010x}",
        index,
        read_reg(dma, DMA_RX_DESC_LIST),
        read_reg(dma, DMA_TX_DESC_LIST)
    );
    read_phy_registers(base);
}

/// Inspect both integrated controllers without changing their operating state.
pub fn probe_all() {
    println!("[gmac-probe] non-destructive DWMAC/PHY handoff probe begin");
    probe_one(0, GMAC0_BASE);
    probe_one(1, GMAC1_BASE);
    println!("[gmac-probe] probe complete; MAC reset and descriptor DMA were not touched");
}
