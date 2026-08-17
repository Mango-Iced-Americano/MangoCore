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
        KernelTest::new("gmac::platform_clock_reset_state", platform_clock_reset_state),
    ]
}

fn result() -> Result<GmacKtestResult, &'static str> {
    gmac_ktest_result().ok_or("SKIP: no initialized JH7110 GMAC hardware")
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
    let result = result()?;
    if result.tx_submitted && result.tx_own_cleared {
        // DMA_STATUS is diagnostic-only. TBU (bit 2) after a successful
        // single-descriptor DMA fetch is normal queue-tail exhaustion,
        // not a TX data-path failure — the ARP frame reached the wire.
        if result.dma_status & (1 << 2) != 0 {
            crate::println!(
                "  diag: DMA_STATUS={:#010x} (TBU=1 — benign tail exhaustion)",
                result.dma_status,
            );
        }
        Ok(())
    } else {
        Err("TX descriptor OWN did not clear within 100ms")
    }
}

fn rx_dma_running() -> Result<(), &'static str> {
    if result()?.dma_rx_ctrl & 1 != 0 {
        Ok(())
    } else {
        Err("RX DMA channel not started (SR bit = 0)")
    }
}

fn rx_mac_config_ok() -> Result<(), &'static str> {
    let result = result()?;
    if result.mac_config & 1 != 0 && result.mac_config & (1 << 13) != 0 {
        Ok(())
    } else {
        Err("MAC config missing RE or DM (full-duplex)")
    }
}

fn rx_frames_received() -> Result<(), &'static str> {
    let result = result()?;
    if result.rx_descriptor_valid {
        Ok(())
    } else {
        crate::println!(
            "  diag: DMA_STATUS={:#010x} CUR_DESC={:#010x} GMAC_DEBUG={:#010x} RXQ_CTRL0={:#010x}",
            result.dma_status,
            result.cur_rx_desc,
            result.gmac_debug,
            result.rxq_ctrl0
        );
        crate::println!(
            "  phy: valid={} A001={:#06x} A010={:#06x} A012={:#06x} EXT_000c={:#06x} AON_RX={:#010x} RX_INV={:#010x} TX_CLK={:#010x}",
            result.phy_diagnostics_valid,
            result.phy_chip_config,
            result.phy_pad_drive_strength,
            result.phy_synce_config,
            result.phy_clock_gating,
            result.aon_gmac0_rx,
            result.aon_gmac0_rx_inv,
            result.aon_gmac0_tx
        );
        Err("RX descriptor invalid: expected OWN clear, no RX error, FIRST/LAST, and frame length 14..=DMA_BUFFER_SIZE")
    }
}

fn rx_writeback() -> Result<(), &'static str> {
    let result = result()?;
    if result.rx_writeback {
        Ok(())
    } else {
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
            "  PHY diag: valid={} A001={:#06x} A010={:#06x} A012={:#06x} EXT_000c={:#06x} AON_RX={:#010x} RX_INV={:#010x} TX_CLK={:#010x}",
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
}

fn platform_clock_reset_state() -> Result<(), &'static str> {
    let result = result()?;
    if result.aon_gmac0_ahb & (1 << 31) == 0
        || result.aon_gmac0_axi & (1 << 31) == 0
        || result.aon_gmac0_tx != 0x8100_0000
        || result.aon_gmac0_tx_inv != 0x4000_0000
        || result.sys_gmac0_ptp != 0x8000_000a
        || result.sys_gmac0_gtx != 0x8000_0008
        || result.sys_gmac0_gtxclk != 0x8000_0020
        || result.aon_gmac0_reset != 0x0000_00e0
    {
        crate::println!(
            "  platform clocks: AON_AHB={:#010x} AON_AXI={:#010x} AON_TX={:#010x} AON_TXI={:#010x} SYS_PTP={:#010x} SYS_GTX={:#010x} SYS_GTXCLK={:#010x} AON_RESET={:#010x}",
            result.aon_gmac0_ahb,
            result.aon_gmac0_axi,
            result.aon_gmac0_tx,
            result.aon_gmac0_tx_inv,
            result.sys_gmac0_ptp,
            result.sys_gmac0_gtx,
            result.sys_gmac0_gtxclk,
            result.aon_gmac0_reset,
        );
        return Err("VF2 GMAC0 platform clock/reset state does not match the verified bring-up sequence");
    }
    Ok(())
}
