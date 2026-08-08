//! VisionFive 2 JH7110 DWMAC 5.20 polling network driver.

#[path = "gmac_jh7110/mmio.rs"]
mod mmio;
#[path = "gmac_jh7110/phy.rs"]
mod phy;
#[path = "gmac_jh7110/ring.rs"]
mod ring;
#[path = "gmac_jh7110/ktest.rs"]
mod ktest;

use super::NetDevice;
use crate::hal::{get_clock_freq, get_time};
use crate::mm::{FrameTracker, PhysAddr};
use alloc::sync::Arc;
use spin::Mutex;

use mmio::*;
use phy::LinkState;
use ring::DmaRings;

const EEPROM_MAC: [u8; 6] = [0x6c, 0xcf, 0x39, 0x00, 0x56, 0xd2];
const DWMAC_CORE_5_20: u32 = 0x52;

#[derive(Debug)]
pub(crate) enum GmacJh7110Error {
    InvalidVersion(u32),
    InvalidMac,
    OutOfMemory,
    DmaResetTimeout,
    DmaAddressAbove4G(usize),
    InvalidPhy(u32),
}

struct GmacJh7110Inner {
    base: usize,
    mac: [u8; 6],
    rings: DmaRings,
    link: LinkState,
    last_link_poll: usize,
}

// allow: SIZE_OK — the hardware bring-up state machine is intentionally kept together.
pub struct GmacJh7110(Mutex<GmacJh7110Inner>);

pub(crate) use ktest::{gmac_ktest_result, GmacKtestResult};

fn enable_gmac0_clocks_and_release_resets() {
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
}

fn wait_for_gmac0_reset_settle() {
    const RESET_SETTLE_MS: usize = 100;
    let start = crate::timer::get_time_ms();
    while crate::timer::get_time_ms().wrapping_sub(start) < RESET_SETTLE_MS {
        core::hint::spin_loop();
    }
}

fn reset_dma() -> Result<(), GmacJh7110Error> {
    write_reg(DMA_BUS_MODE, DMA_SOFTWARE_RESET);
    let start = get_time();
    let timeout = get_clock_freq();
    while read_reg(DMA_BUS_MODE) & DMA_SOFTWARE_RESET != 0 {
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

fn read_mac() -> [u8; 6] {
    let low = read_reg(MAC_ADDR0_LOW);
    let high = read_reg(MAC_ADDR0_HIGH);
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

fn write_mac(mac: [u8; 6]) {
    let low = u32::from(mac[0])
        | (u32::from(mac[1]) << 8)
        | (u32::from(mac[2]) << 16)
        | (u32::from(mac[3]) << 24);
    let high = u32::from(mac[4]) | (u32::from(mac[5]) << 8) | MAC_ADDR_ENABLE;
    write_reg(MAC_ADDR0_HIGH, high);
    write_reg(MAC_ADDR0_LOW, low);
}

fn configure_dma(rings: &DmaRings) {
    write_reg(DMA_SYS_BUS_MODE, DMA_SYS_BUS_FIXED_BURST | DMA_SYS_BUS_ALL_BURSTS);
    write_reg(DMA_BUS_MODE, DMA_BUS_DCHE | DMA_BUS_INTM_MODE1);
    write_reg(MTL_OP_MODE, 0);
    write_reg(MTL_TXQ0_OP_MODE, MTL_TXQ_TSF | MTL_TXQ_ENABLE | MTL_QUEUE_SIZE_2K);
    write_reg(MTL_RXQ0_OP_MODE, MTL_RXQ_RSF | MTL_QUEUE_SIZE_2K_RX);
    write_reg(MTL_RXQ_DMA_MAP0, 0);
    write_reg(GMAC_RXQ_CTRL0, GMAC_RXQ0_ENABLE);

    write_reg(DMA_CH0_CONTROL, 0);
    write_reg(DMA_CH0_TX_CONTROL, DMA_CH_TX_PBL_16 | DMA_CH_TX_OSP);
    write_reg(DMA_CH0_RX_CONTROL, DMA_CH_RX_PBL_16 | DMA_CH_RX_BUFFER_SIZE);
    write_reg(DMA_CH0_TX_BASE_HI, 0);
    write_reg(DMA_CH0_TX_BASE, rings.tx_descriptor_base() as u32);
    write_reg(DMA_CH0_RX_BASE_HI, 0);
    write_reg(DMA_CH0_RX_BASE, rings.rx_descriptor_base() as u32);
    write_reg(DMA_CH0_TX_RING_LEN, (ring::TX_DESC_COUNT - 1) as u32);
    write_reg(DMA_CH0_RX_RING_LEN, (ring::RX_DESC_COUNT - 1) as u32);
    write_reg(DMA_CH0_TX_END, rings.tx_descriptor_base() as u32);
    write_reg(DMA_CH0_RX_END, rings.rx_descriptor_end() as u32);
    write_reg(DMA_CH0_INTR_ENA, 0);
    write_reg(DMA_CH0_STATUS, u32::MAX);
}

fn configure_mac() {
    let config = (read_reg(GMAC_CONFIG) | MAC_JD | MAC_ACS | MAC_BE | MAC_DCRS | MAC_TE)
        & !(MAC_PS | MAC_FES | MAC_DM | MAC_RE);
    write_reg(GMAC_CONFIG, config);
    write_reg(GMAC_FRAME_FILTER, 1); // promiscuous mode during bring-up
}

fn apply_link_state(state: LinkState) {
    let mut config = read_reg(GMAC_CONFIG) & !(MAC_PS | MAC_FES | MAC_DM);
    match state.speed_mbps {
        10 => config |= MAC_PS,
        100 => config |= MAC_PS | MAC_FES,
        _ => {}
    }
    if state.full_duplex {
        config |= MAC_DM;
    }
    write_reg(GMAC_CONFIG, config);
}

impl GmacJh7110Inner {
    fn poll_link(&mut self) {
        let now = get_time();
        if now.wrapping_sub(self.last_link_poll) < get_clock_freq() {
            return;
        }
        self.last_link_poll = now;
        if let Ok(link) = phy::read_link_state(self.base) {
            if link != self.link {
                apply_link_state(link);
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
    pub(crate) fn new() -> Result<Self, GmacJh7110Error> {
        enable_gmac0_clocks_and_release_resets();
        reset_dma()?;

        let version = read_reg(MAC_VERSION);
        if version == 0 || version == u32::MAX || version & 0xff < DWMAC_CORE_5_20 {
            return Err(GmacJh7110Error::InvalidVersion(version));
        }
        let register_mac = read_mac();
        let mac = if register_mac == [0xff; 6] {
            EEPROM_MAC
        } else if valid_mac(register_mac) {
            register_mac
        } else {
            return Err(GmacJh7110Error::InvalidMac);
        };

        let rings = DmaRings::allocate()?;
        configure_dma(&rings);
        write_mac(mac);
        configure_mac();

        write_reg(DMA_CH0_TX_CONTROL, read_reg(DMA_CH0_TX_CONTROL) | DMA_CH_TX_START);
        write_reg(DMA_CH0_TX_END, rings.tx_descriptor_base() as u32);
        write_reg(DMA_CH0_RX_CONTROL, read_reg(DMA_CH0_RX_CONTROL) | DMA_CH_RX_START);
        write_reg(DMA_CH0_RX_END, rings.rx_descriptor_end() as u32);
        write_reg(GMAC_CONFIG, read_reg(GMAC_CONFIG) | MAC_RE | MAC_TE);

        let phy_id = phy::configure_yt8531(GMAC0_BASE)?;
        let link = phy::wait_initial_link(GMAC0_BASE);
        apply_link_state(link);

        println!(
            "[gmac-jh7110] DWMAC={:#x} PHY={:#x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            version, phy_id, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );

        let driver = Self(Mutex::new(GmacJh7110Inner {
            base: GMAC0_BASE,
            mac,
            rings,
            link,
            last_link_poll: get_time(),
        }));
        if crate::bootargs::load().mode == crate::bootargs::BootMode::Ktest {
            ktest::run(&driver);
        }
        Ok(driver)
    }
}

impl NetDevice for GmacJh7110 {
    fn receive(&self, buf: &mut [u8]) -> Option<usize> {
        let mut inner = self.0.lock();
        inner.poll_link();
        inner.rings.receive(buf)
    }

    fn transmit(&self, buf: &[u8]) {
        let mut inner = self.0.lock();
        inner.poll_link();
        if inner.link.up {
            let _ = inner.rings.transmit(buf);
        }
    }

    fn mac_address(&self) -> [u8; 6] {
        self.0.lock().mac
    }
}
