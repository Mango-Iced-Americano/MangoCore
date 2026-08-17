//! VisionFive 2 JH7110 DWMAC 5.20 polling network driver.

#[path = "gmac_jh7110/mmio.rs"]
mod mmio;
#[path = "gmac_jh7110/phy.rs"]
mod phy;
#[path = "gmac_jh7110/ring.rs"]
mod ring;
#[path = "gmac_jh7110/ktest.rs"]
mod ktest;
#[path = "gmac_jh7110/probe.rs"]
mod probe;

use super::NetDevice;
use crate::hal::{get_clock_freq, get_time};
use crate::mm::{FrameTracker, PhysAddr};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use mmio::*;
use phy::LinkState;
use ring::DmaRings;

const EEPROM_MAC: [u8; 6] = [0x6c, 0xcf, 0x39, 0x00, 0x56, 0xd2];
const DWMAC_CORE_5_20: u32 = 0x52;
static GMAC_IRQ_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);

pub(crate) enum GmacJh7110Error {
    InvalidIrq,
    UnsupportedInstance(usize),
    IrqAlreadyBound,
    InvalidVersion(u32),
    InvalidMac,
    OutOfMemory,
    DmaResetTimeout,
    DmaAddressAbove4G(usize),
    InvalidPhy(u32),
}

impl core::fmt::Debug for GmacJh7110Error {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidIrq => formatter.write_str("InvalidIrq"),
            Self::UnsupportedInstance(base) => formatter
                .debug_tuple("UnsupportedInstance")
                .field(base)
                .finish(),
            Self::IrqAlreadyBound => formatter.write_str("IrqAlreadyBound"),
            Self::InvalidVersion(version) => {
                formatter.debug_tuple("InvalidVersion").field(version).finish()
            }
            Self::InvalidMac => formatter.write_str("InvalidMac"),
            Self::OutOfMemory => formatter.write_str("OutOfMemory"),
            Self::DmaResetTimeout => formatter.write_str("DmaResetTimeout"),
            Self::DmaAddressAbove4G(address) => formatter
                .debug_tuple("DmaAddressAbove4G")
                .field(address)
                .finish(),
            Self::InvalidPhy(phy_id) => {
                formatter.debug_tuple("InvalidPhy").field(phy_id).finish()
            }
        }
    }
}

struct GmacJh7110Inner {
    regs: GmacMmio,
    mac: [u8; 6],
    rings: DmaRings,
    link: LinkState,
    last_link_poll: usize,
}

// allow: SIZE_OK — the hardware bring-up state machine is intentionally kept together.
pub struct GmacJh7110 {
    inner: Mutex<GmacJh7110Inner>,
    irq: usize,
}

pub(crate) use ktest::{gmac_ktest_result, GmacKtestResult};
pub(crate) use probe::{discover_gmac0_resources, probe_gmac_from_device_manager};

fn enable_gmac0_clocks_and_release_resets(mac_base: usize) -> Result<(), GmacJh7110Error> {
    if mac_base != JH7110_GMAC0_BASE {
        return Err(GmacJh7110Error::UnsupportedInstance(mac_base));
    }
    // L1 board glue: these JH7110 GMAC0 CRG/SYSCON registers remain fixed until
    // L2 consumes their FDT phandle resources. The checked MAC base is the guard
    // that prevents this sequence from being applied to GMAC1.
    write_mmio(SYS_CRG_BASE, SYS_CRG_GMAC0_PTP, GMAC0_PTP_CLOCK_CONFIG);
    write_mmio(SYS_CRG_BASE, SYS_CRG_GMAC0_GTX, GMAC0_GTX_CLOCK_CONFIG);
    write_mmio(
        SYS_CRG_BASE,
        SYS_CRG_GMAC0_GTXCLK,
        GMAC0_GTXCLK_CLOCK_CONFIG,
    );
    write_mmio(
        AON_CRG_BASE,
        AON_CRG_GMAC0_AHB,
        read_mmio(AON_CRG_BASE, AON_CRG_GMAC0_AHB) | CLOCK_ENABLE,
    );
    write_mmio(
        AON_CRG_BASE,
        AON_CRG_GMAC0_AXI,
        read_mmio(AON_CRG_BASE, AON_CRG_GMAC0_AXI) | CLOCK_ENABLE,
    );
    write_mmio(AON_CRG_BASE, AON_CRG_GMAC0_TX, GMAC0_TX_CLOCK_CONFIG);
    write_mmio(AON_CRG_BASE, AON_CRG_GMAC0_TX_INV, GMAC0_TX_CLOCK_INVERT);
    // RX clock gate (0x10): power on the RX clock domain.
    write_mmio(
        AON_CRG_BASE,
        AON_CRG_GMAC0_RX_GATE,
        read_mmio(AON_CRG_BASE, AON_CRG_GMAC0_RX_GATE) | CLOCK_ENABLE,
    );
    // RX clock mux (0x1c): select RGMII RX clock (clear RMII_RTX bit).
    write_mmio(
        AON_CRG_BASE,
        AON_CRG_GMAC0_RX,
        (read_mmio(AON_CRG_BASE, AON_CRG_GMAC0_RX) & !AON_CRG_GMAC0_RX_RMII_RTX) | CLOCK_ENABLE,
    );
    // Disable RX clock inversion — inverted RX clock causes the MAC to
    // sample RGMII data on the wrong edge, corrupting all received frames.
    write_mmio(
        AON_CRG_BASE,
        AON_CRG_GMAC0_RX_INV,
        read_mmio(AON_CRG_BASE, AON_CRG_GMAC0_RX_INV) & !(1 << 30),
    );
    for reset_state in GMAC0_RESET_SEQUENCE {
        write_mmio(AON_CRG_BASE, AON_CRG_RESET, reset_state);
        wait_for_gmac0_reset_settle();
    }
    let phy_interface = read_mmio(AON_SYSCON_BASE, AON_SYSCON_GMAC0_PHY_INTF);
    write_mmio(
        AON_SYSCON_BASE,
        AON_SYSCON_GMAC0_PHY_INTF,
        (phy_interface & !AON_SYSCON_GMAC0_PHY_INTF_MASK) | AON_SYSCON_GMAC0_PHY_INTF_RGMII,
    );
    Ok(())
}

fn wait_for_gmac0_reset_settle() {
    const RESET_SETTLE_MS: usize = 100;
    let start = crate::timer::get_time_ms();
    while crate::timer::get_time_ms().wrapping_sub(start) < RESET_SETTLE_MS {
        core::hint::spin_loop();
    }
}

fn reset_dma(regs: GmacMmio) -> Result<(), GmacJh7110Error> {
    regs.write(DMA_BUS_MODE, DMA_SOFTWARE_RESET);
    let start = get_time();
    let timeout = get_clock_freq();
    while regs.read(DMA_BUS_MODE) & DMA_SOFTWARE_RESET != 0 {
        if get_time().wrapping_sub(start) >= timeout {
            return Err(GmacJh7110Error::DmaResetTimeout);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

fn dma_address(frame: &Arc<FrameTracker>) -> Result<usize, GmacJh7110Error> {
    let physical: PhysAddr = frame.ppn.into();
    let address: usize = physical.into();
    if address
        .checked_add(crate::config::PAGE_SIZE)
        .map_or(true, |end| end > 0x1_0000_0000)
    {
        return Err(GmacJh7110Error::DmaAddressAbove4G(address));
    }
    Ok(address)
}

fn read_mac(regs: GmacMmio) -> [u8; 6] {
    let low = regs.read(MAC_ADDR0_LOW);
    let high = regs.read(MAC_ADDR0_HIGH);
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

fn write_mac(regs: GmacMmio, mac: [u8; 6]) {
    let low = u32::from(mac[0])
        | (u32::from(mac[1]) << 8)
        | (u32::from(mac[2]) << 16)
        | (u32::from(mac[3]) << 24);
    let high = u32::from(mac[4]) | (u32::from(mac[5]) << 8) | MAC_ADDR_ENABLE;
    regs.write(MAC_ADDR0_HIGH, high);
    regs.write(MAC_ADDR0_LOW, low);
}

fn configure_dma(regs: GmacMmio, rings: &DmaRings) {
    regs.write(DMA_SYS_BUS_MODE, DMA_SYS_BUS_FIXED_BURST | DMA_SYS_BUS_ALL_BURSTS);
    regs.write(DMA_BUS_MODE, DMA_BUS_DCHE | DMA_BUS_INTM_MODE1);
    regs.write(MTL_OP_MODE, 0);
    regs.write(MTL_TXQ0_OP_MODE, MTL_TXQ_TSF | MTL_TXQ_ENABLE | MTL_QUEUE_SIZE_2K);
    regs.write(MTL_RXQ0_OP_MODE, MTL_RXQ_RSF | MTL_QUEUE_SIZE_2K_RX);
    regs.write(MTL_RXQ_DMA_MAP0, 0);
    regs.write(GMAC_RXQ_CTRL0, GMAC_RXQ0_ENABLE);

    regs.write(DMA_CH0_CONTROL, 0);
    regs.write(DMA_CH0_TX_CONTROL, DMA_CH_TX_PBL_16 | DMA_CH_TX_OSP);
    regs.write(DMA_CH0_RX_CONTROL, DMA_CH_RX_PBL_16 | DMA_CH_RX_BUFFER_SIZE);
    regs.write(DMA_CH0_TX_BASE_HI, 0);
    regs.write(DMA_CH0_TX_BASE, rings.tx_descriptor_base() as u32);
    regs.write(DMA_CH0_RX_BASE_HI, 0);
    regs.write(DMA_CH0_RX_BASE, rings.rx_descriptor_base() as u32);
    regs.write(DMA_CH0_TX_RING_LEN, (ring::TX_DESC_COUNT - 1) as u32);
    regs.write(DMA_CH0_RX_RING_LEN, (ring::RX_DESC_COUNT - 1) as u32);
    regs.write(DMA_CH0_TX_END, rings.tx_descriptor_base() as u32);
    regs.write(DMA_CH0_RX_END, rings.rx_descriptor_end() as u32);
}

fn configure_mac(regs: GmacMmio) {
    let config = (regs.read(GMAC_CONFIG) | MAC_JD | MAC_ACS | MAC_BE | MAC_DCRS | MAC_TE)
        & !(MAC_PS | MAC_FES | MAC_DM | MAC_RE);
    regs.write(GMAC_CONFIG, config);
    regs.write(GMAC_FRAME_FILTER, 1); // promiscuous mode during bring-up
}

fn apply_link_state(regs: GmacMmio, state: LinkState) {
    let mut config = regs.read(GMAC_CONFIG) & !(MAC_PS | MAC_FES | MAC_DM);
    match state.speed_mbps {
        10 => config |= MAC_PS,
        100 => config |= MAC_PS | MAC_FES,
        _ => {}
    }
    if state.full_duplex {
        config |= MAC_DM;
    }
    regs.write(GMAC_CONFIG, config);
}

fn start_dma(regs: GmacMmio, rings: &DmaRings) {
    regs.write(
        DMA_CH0_TX_CONTROL,
        regs.read(DMA_CH0_TX_CONTROL) | DMA_CH_TX_START,
    );
    regs.write(DMA_CH0_TX_END, rings.tx_descriptor_base() as u32);
    regs.write(
        DMA_CH0_RX_CONTROL,
        regs.read(DMA_CH0_RX_CONTROL) | DMA_CH_RX_START,
    );
    regs.write(DMA_CH0_RX_END, rings.rx_descriptor_end() as u32);
    regs.write(GMAC_CONFIG, regs.read(GMAC_CONFIG) | MAC_RE | MAC_TE);
    regs.write(DMA_CH0_STATUS, u32::MAX);
    regs.write(DMA_CH0_INTR_ENA, DMA_CH_INTR_NIE | DMA_CH_INTR_RIE);
}

impl GmacJh7110Inner {
    fn poll_link(&mut self) {
        let now = get_time();
        if now.wrapping_sub(self.last_link_poll) < get_clock_freq() {
            return;
        }
        self.last_link_poll = now;
        if let Ok(link) = phy::read_link_state(self.regs) {
            if link != self.link {
                apply_link_state(self.regs, link);
                println!(
                    "[gmac-jh7110] link {} speed={}M duplex={}",
                    if link.up { "up" } else { "down" },
                    link.speed_mbps,
                    if link.full_duplex { "full" } else { "half" }
                );
                self.link = link;
            }
        }
    }
}

impl GmacJh7110 {
    pub(crate) fn new(base: usize, irq: usize) -> Result<Self, GmacJh7110Error> {
        if irq == 0 {
            return Err(GmacJh7110Error::InvalidIrq);
        }
        let regs = GmacMmio::new(base)?;
        enable_gmac0_clocks_and_release_resets(base)?;
        reset_dma(regs)?;

        let version = regs.read(MAC_VERSION);
        if version == 0 || version == u32::MAX || version & 0xff < DWMAC_CORE_5_20 {
            return Err(GmacJh7110Error::InvalidVersion(version));
        }
        let register_mac = read_mac(regs);
        let mac = if register_mac == [0xff; 6] {
            EEPROM_MAC
        } else if valid_mac(register_mac) {
            register_mac
        } else {
            return Err(GmacJh7110Error::InvalidMac);
        };

        let rings = DmaRings::allocate()?;
        configure_dma(regs, &rings);
        write_mac(regs, mac);
        configure_mac(regs);

        let phy_id = phy::configure_yt8531(regs)?;
        let link = phy::wait_initial_link(regs);
        apply_link_state(regs, link);
        start_dma(regs, &rings);

        println!(
            "[gmac-jh7110] DWMAC={:#x} PHY={:#x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            version, phy_id, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );

        let driver = Self {
            inner: Mutex::new(GmacJh7110Inner {
                regs,
                mac,
                rings,
                link,
                last_link_poll: get_time(),
            }),
            irq,
        };
        // Release ensures PLIC can only invoke the bounded handler after the
        // MAC/DMA register programming above is visible to this CPU.
        GMAC_IRQ_MMIO_BASE
            .compare_exchange(0, base, Ordering::Release, Ordering::Acquire)
            .map_err(|_| GmacJh7110Error::IrqAlreadyBound)?;
        if crate::bootargs::load().mode == crate::bootargs::BootMode::Ktest {
            ktest::run(&driver);
        }
        Ok(driver)
    }
}

impl NetDevice for GmacJh7110 {
    fn receive(&self, buf: &mut [u8]) -> Option<usize> {
        let mut inner = self.inner.lock();
        inner.poll_link();
        let regs = inner.regs;
        inner.rings.receive(regs, buf)
    }

    fn transmit(&self, buf: &[u8]) {
        let mut inner = self.inner.lock();
        inner.poll_link();
        if inner.link.up {
            let regs = inner.regs;
            let _ = inner.rings.transmit(regs, buf);
        }
    }

    fn mac_address(&self) -> [u8; 6] {
        self.inner.lock().mac
    }

    fn interrupt(&self) -> Option<(usize, fn())> {
        Some((self.irq, gmac_jh7110_irq))
    }
}

fn gmac_jh7110_irq() {
    let base = GMAC_IRQ_MMIO_BASE.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    let status = read_mmio(base, DMA_CH0_STATUS);
    if status != 0 {
        write_mmio(base, DMA_CH0_STATUS, status);
    }
    crate::net::config::NET_INTERFACE.notify_rx_interrupt();
}
