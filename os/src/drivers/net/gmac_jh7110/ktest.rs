use super::GmacJh7110;
use super::{mmio, phy};
use spin::Mutex;

const TEST_IP: [u8; 4] = [192, 168, 80, 20];
const PEER_IP: [u8; 4] = [192, 168, 80, 10];

#[derive(Clone, Copy)]
pub(crate) struct GmacKtestResult {
    pub(crate) tx_submitted: bool,
    pub(crate) tx_own_cleared: bool,
    pub(crate) rx_writeback: bool,
    pub(crate) dma_status: u32,
    pub(crate) cur_rx_desc: u32,
    pub(crate) mac_config: u32,
    pub(crate) gmac_debug: u32,
    pub(crate) rxq_ctrl0: u32,
    pub(crate) mtl_rxq_op: u32,
    pub(crate) dma_rx_ctrl: u32,
    pub(crate) phy_diagnostics_valid: bool,
    pub(crate) phy_chip_config: u16,
    pub(crate) phy_pad_drive_strength: u16,
    pub(crate) phy_synce_config: u16,
    pub(crate) phy_clock_gating: u16,
    pub(crate) aon_gmac0_rx: u32,
    pub(crate) aon_gmac0_rx_inv: u32,
    pub(crate) aon_gmac0_tx: u32,
}

static GMAC_KTEST_RESULT: Mutex<Option<GmacKtestResult>> = Mutex::new(None);

pub(super) fn run(driver: &GmacJh7110) {
    let mut inner = driver.0.lock();
    let frame = arp_request(inner.mac);
    let result = inner.rings.ktest_probe(&frame);
    let phy_diagnostics = phy::read_diagnostics(inner.base).ok();
    let result = GmacKtestResult {
        tx_submitted: result.tx_submitted,
        tx_own_cleared: result.tx_own_cleared,
        rx_writeback: result.rx_writeback,
        dma_status: result.dma_status,
        cur_rx_desc: result.cur_rx_desc,
        mac_config: result.mac_config,
        gmac_debug: result.gmac_debug,
        rxq_ctrl0: result.rxq_ctrl0,
        mtl_rxq_op: result.mtl_rxq_op,
        dma_rx_ctrl: result.dma_rx_ctrl,
        phy_diagnostics_valid: phy_diagnostics.is_some(),
        phy_chip_config: phy_diagnostics.map_or(0, |value| value.chip_config),
        phy_pad_drive_strength: phy_diagnostics.map_or(0, |value| value.pad_drive_strength),
        phy_synce_config: phy_diagnostics.map_or(0, |value| value.synce_config),
        phy_clock_gating: phy_diagnostics.map_or(0, |value| value.clock_gating),
        aon_gmac0_rx: mmio::read_mmio(mmio::AON_CRG_BASE, mmio::AON_CRG_GMAC0_RX),
        aon_gmac0_rx_inv: mmio::read_mmio(mmio::AON_CRG_BASE, mmio::AON_CRG_GMAC0_RX_INV),
        aon_gmac0_tx: mmio::read_mmio(mmio::AON_CRG_BASE, mmio::AON_CRG_GMAC0_TX_MUX),
    };
    drop(inner);
    *GMAC_KTEST_RESULT.lock() = Some(result);
}

pub(crate) fn gmac_ktest_result() -> Option<GmacKtestResult> {
    *GMAC_KTEST_RESULT.lock()
}

fn arp_request(mac: [u8; 6]) -> [u8; 60] {
    let mut frame = [0u8; 60];
    frame[..6].fill(0xff);
    frame[6..12].copy_from_slice(&mac);
    frame[12..14].copy_from_slice(&[0x08, 0x06]);
    frame[14..16].copy_from_slice(&[0x00, 0x01]);
    frame[16..18].copy_from_slice(&[0x08, 0x00]);
    frame[18] = 6;
    frame[19] = 4;
    frame[20..22].copy_from_slice(&[0x00, 0x01]);
    frame[22..28].copy_from_slice(&mac);
    frame[28..32].copy_from_slice(&TEST_IP);
    frame[38..42].copy_from_slice(&PEER_IP);
    frame
}
