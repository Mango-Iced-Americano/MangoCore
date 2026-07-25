use crate::drivers::net::gmac_jh7110::{gmac_ktest_result, GmacKtestResult};
use crate::kernel_tests::runner::KernelTest;
use alloc::vec;
use alloc::vec::Vec;

pub(super) fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("gmac::tx_submitted", tx_submitted),
        KernelTest::new("gmac::tx_own_cleared", tx_own_cleared),
        KernelTest::new("gmac::tx_healthy", tx_healthy),
        KernelTest::new("gmac::rx_dma_running", rx_dma_running),
        KernelTest::new("gmac::rx_mac_config_ok", rx_mac_config_ok),
        KernelTest::new("gmac::rx_frames_received", rx_frames_received),
        KernelTest::new("gmac::rx_writeback", rx_writeback),
    ]
}

fn result() -> Result<GmacKtestResult, &'static str> {
    gmac_ktest_result().ok_or("GMAC init-time ARP probe did not run")
}

fn tx_submitted() -> Result<(), &'static str> {
    if result()?.tx_submitted {
        Ok(())
    } else {
        Err("ARP frame was not submitted to a TX descriptor")
    }
}

fn tx_own_cleared() -> Result<(), &'static str> {
    if result()?.tx_own_cleared {
        Ok(())
    } else {
        Err("TX descriptor OWN did not clear within 100ms")
    }
}

fn tx_healthy() -> Result<(), &'static str> {
    match gmac_ktest_result() {
        Some(result) if result.tx_own_cleared && result.dma_status & (1 << 2) == 0 => Ok(()),
        Some(result) if result.dma_status & (1 << 2) != 0 => {
            Err("TX underrun (TBU) — TX DMA buffer issue")
        }
        _ => Err("GMAC init-time ARP probe did not run"),
    }
}

fn rx_dma_running() -> Result<(), &'static str> {
    match gmac_ktest_result() {
        Some(result) if result.dma_rx_ctrl & 1 != 0 => Ok(()),
        Some(_) => Err("RX DMA channel not started (SR bit = 0)"),
        None => Err("GMAC init-time ARP probe did not run"),
    }
}

fn rx_mac_config_ok() -> Result<(), &'static str> {
    match gmac_ktest_result() {
        Some(result) if result.mac_config & 1 != 0 && result.mac_config & (1 << 13) != 0 => Ok(()),
        Some(_) => Err("MAC config missing RE or DM (full-duplex)"),
        None => Err("GMAC init-time ARP probe did not run"),
    }
}

fn rx_frames_received() -> Result<(), &'static str> {
    match gmac_ktest_result() {
        Some(result) if result.gmac_debug != 0 => Ok(()),
        Some(result) => {
            crate::println!(
                "  diag: DMA_STATUS={:#010x} CUR_DESC={:#010x} GMAC_DEBUG={:#010x} RXQ_CTRL0={:#010x}",
                result.dma_status,
                result.cur_rx_desc,
                result.gmac_debug,
                result.rxq_ctrl0
            );
            crate::println!(
                "  phy: valid={} A001={:#06x} A010={:#06x} A012={:#06x} EXT_000c={:#06x} AON_RX={:#010x} RX_INV={:#010x} TX_MUX={:#010x}",
                result.phy_diagnostics_valid,
                result.phy_chip_config,
                result.phy_pad_drive_strength,
                result.phy_synce_config,
                result.phy_clock_gating,
                result.aon_gmac0_rx,
                result.aon_gmac0_rx_inv,
                result.aon_gmac0_tx
            );
            Err("GMAC_DEBUG=0 — MAC received zero frames. PHY may not be driving RGMII.")
        }
        None => Err("GMAC init-time ARP probe did not run"),
    }
}

fn rx_writeback() -> Result<(), &'static str> {
    match gmac_ktest_result() {
        Some(result) if result.rx_writeback => Ok(()),
        Some(result) => {
            crate::println!(
                "  RX diag: DMA_STATUS={:#010x} CUR_RX_DESC={:#010x} MAC_CONFIG={:#010x} GMAC_DEBUG={:#010x} RXQ_CTRL0={:#010x} MTL_RXQ_OP={:#010x} DMA_RX_CTRL={:#010x}",
                result.dma_status,
                result.cur_rx_desc,
                result.mac_config,
                result.gmac_debug,
                result.rxq_ctrl0,
                result.mtl_rxq_op,
                result.dma_rx_ctrl
            );
            crate::println!(
                "  PHY diag: valid={} A001={:#06x} A010={:#06x} A012={:#06x} EXT_000c={:#06x} AON_RX={:#010x} RX_INV={:#010x} TX_MUX={:#010x}",
                result.phy_diagnostics_valid,
                result.phy_chip_config,
                result.phy_pad_drive_strength,
                result.phy_synce_config,
                result.phy_clock_gating,
                result.aon_gmac0_rx,
                result.aon_gmac0_rx_inv,
                result.aon_gmac0_tx
            );
            Err("no RX descriptor write-back observed within 100ms")
        }
        None => Err("GMAC init-time ARP probe did not run"),
    }
}
