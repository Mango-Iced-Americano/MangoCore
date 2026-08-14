use super::ring::DMA_BUFFER_SIZE;

pub(super) use crate::hal::platform::jh7110_cache::{
    jh7110_dma_barrier as dma_barrier, jh7110_l2cc_flush_range as clean_dma_range,
};

pub(super) const GMAC0_BASE: usize = 0x1603_0000;
pub(super) const SYS_CRG_BASE: usize = 0x1302_0000;
pub(super) const AON_CRG_BASE: usize = 0x1700_0000;
pub(super) const AON_SYSCON_BASE: usize = 0x1701_0000;

pub(super) const GMAC_CONFIG: usize = 0x0000;
pub(super) const GMAC_FRAME_FILTER: usize = 0x0008;
pub(super) const MAC_VERSION: usize = 0x0110;
pub(super) const MDIO_ADDR: usize = 0x0200;
pub(super) const MDIO_DATA: usize = 0x0204;
pub(super) const MAC_ADDR0_HIGH: usize = 0x0300;
pub(super) const MAC_ADDR0_LOW: usize = 0x0304;
pub(super) const GMAC_RXQ_CTRL0: usize = 0x00a0;
pub(super) const GMAC_DEBUG: usize = 0x0114;
pub(super) const MTL_OP_MODE: usize = 0x0c00;
pub(super) const MTL_RXQ_DMA_MAP0: usize = 0x0c30;
pub(super) const MTL_TXQ0_OP_MODE: usize = 0x0d00;
pub(super) const MTL_RXQ0_OP_MODE: usize = 0x0d30;
pub(super) const DMA_BUS_MODE: usize = 0x1000;
pub(super) const DMA_SYS_BUS_MODE: usize = 0x1004;
pub(super) const DMA_CH0_CONTROL: usize = 0x1100;
pub(super) const DMA_CH0_TX_CONTROL: usize = 0x1104;
pub(super) const DMA_CH0_RX_CONTROL: usize = 0x1108;
pub(super) const DMA_CH0_TX_BASE_HI: usize = 0x1110;
pub(super) const DMA_CH0_TX_BASE: usize = 0x1114;
pub(super) const DMA_CH0_RX_BASE_HI: usize = 0x1118;
pub(super) const DMA_CH0_RX_BASE: usize = 0x111c;
pub(super) const DMA_CH0_TX_END: usize = 0x1120;
pub(super) const DMA_CH0_RX_END: usize = 0x1128;
pub(super) const DMA_CH0_TX_RING_LEN: usize = 0x112c;
pub(super) const DMA_CH0_RX_RING_LEN: usize = 0x1130;
pub(super) const DMA_CH0_INTR_ENA: usize = 0x1134;
pub(super) const DMA_CH0_CUR_TX_DESC: usize = 0x1144;
pub(super) const DMA_CH0_CUR_RX_DESC: usize = 0x114c;
pub(super) const DMA_CH0_STATUS: usize = 0x1160;

pub(super) const DMA_CH_INTR_NIE: u32 = 1 << 15;
pub(super) const DMA_CH_INTR_RIE: u32 = 1 << 6;

pub(super) const SYS_CRG_GMAC0_GTX: usize = 0x01b0;
pub(super) const SYS_CRG_GMAC0_PTP: usize = 0x01b4;
pub(super) const SYS_CRG_GMAC0_GTXCLK: usize = 0x01bc;
pub(super) const AON_CRG_GMAC0_AHB: usize = 0x0008;
pub(super) const AON_CRG_GMAC0_AXI: usize = 0x000c;
pub(super) const AON_CRG_GMAC0_RX_GATE: usize = 0x0010;
pub(super) const AON_CRG_GMAC0_RX: usize = 0x001c;
pub(super) const AON_CRG_GMAC0_RX_INV: usize = 0x0020;
pub(super) const AON_CRG_GMAC0_TX_INV: usize = 0x0018;
pub(super) const AON_CRG_GMAC0_TX: usize = 0x0014;
pub(super) const AON_CRG_RESET: usize = 0x0038;
pub(super) const AON_CRG_RESET_GMAC0_AXI: u32 = 1 << 0;
pub(super) const AON_CRG_RESET_GMAC0_AHB: u32 = 1 << 1;
pub(super) const AON_SYSCON_GMAC0_PHY_INTF: usize = 0x000c;
pub(super) const AON_SYSCON_GMAC0_PHY_INTF_MASK: u32 = 0b111 << 18;
pub(super) const AON_SYSCON_GMAC0_PHY_INTF_RGMII: u32 = 0b001 << 18;
pub(super) const CLOCK_ENABLE: u32 = 1 << 31;
pub(super) const AON_CRG_GMAC0_RX_RMII_RTX: u32 = 1 << 24;
pub(super) const GMAC0_TX_CLOCK_CONFIG: u32 = 0x8100_0000;
pub(super) const GMAC0_TX_CLOCK_INVERT: u32 = 0x4000_0000;
pub(super) const GMAC0_PTP_CLOCK_CONFIG: u32 = 0x8000_000a;
pub(super) const GMAC0_GTX_CLOCK_CONFIG: u32 = 0x8000_0008;
pub(super) const GMAC0_GTXCLK_CLOCK_CONFIG: u32 = 0x8000_0020;
pub(super) const GMAC0_RESET_SEQUENCE: [u32; 4] = [0x0000_00e1, 0x0000_00e3, 0x0000_00e2, 0x0000_00e0];

pub(super) const DMA_SOFTWARE_RESET: u32 = 1 << 0;
pub(super) const DMA_BUS_DCHE: u32 = 1 << 19;
pub(super) const DMA_BUS_INTM_MODE1: u32 = 1 << 16;
pub(super) const DMA_SYS_BUS_FIXED_BURST: u32 = 1 << 0;
pub(super) const DMA_SYS_BUS_ALL_BURSTS: u32 = 0x7f << 1;
pub(super) const MTL_TXQ_TSF: u32 = 1 << 1;
pub(super) const MTL_TXQ_ENABLE: u32 = 1 << 3;
pub(super) const MTL_QUEUE_SIZE_2K: u32 = 7 << 16;
pub(super) const MTL_RXQ_RSF: u32 = 1 << 5;
pub(super) const MTL_QUEUE_SIZE_2K_RX: u32 = 7 << 20;
pub(super) const GMAC_RXQ0_ENABLE: u32 = 2;
pub(super) const DMA_CH_TX_PBL_16: u32 = 16 << 16;
pub(super) const DMA_CH_TX_OSP: u32 = 1 << 4;
pub(super) const DMA_CH_TX_START: u32 = 1 << 0;
pub(super) const DMA_CH_RX_PBL_16: u32 = 16 << 16;
pub(super) const DMA_CH_RX_BUFFER_SIZE: u32 = (DMA_BUFFER_SIZE as u32) & 0x3fff;
pub(super) const DMA_CH_RX_START: u32 = 1 << 0;
pub(super) const MAC_RE: u32 = 1 << 0;
pub(super) const MAC_TE: u32 = 1 << 1;
pub(super) const MAC_DCRS: u32 = 1 << 9;
pub(super) const MAC_DM: u32 = 1 << 13;
pub(super) const MAC_FES: u32 = 1 << 14;
pub(super) const MAC_PS: u32 = 1 << 15;
pub(super) const MAC_JD: u32 = 1 << 17;
pub(super) const MAC_BE: u32 = 1 << 18;
pub(super) const MAC_ACS: u32 = 1 << 20;
pub(super) const MAC_ADDR_ENABLE: u32 = 1 << 31;

#[inline(always)]
pub(super) fn read_mmio(base: usize, offset: usize) -> u32 {
    // SAFETY: Categories 6 and 11. Private call sites use documented, aligned
    // JH7110 register offsets in the kernel's supervisor MMIO window.
    unsafe {
        core::ptr::read_volatile(
            crate::mm::PhysAddr(base + offset)
                .direct_map_ptr()
                .cast::<u32>(),
        )
    }
}

#[inline(always)]
pub(super) fn write_mmio(base: usize, offset: usize, value: u32) {
    // SAFETY: Categories 6 and 11. The private base/offset pairs target only
    // aligned documented JH7110 registers or the L2 cache flush trigger in the
    // supervisor MMIO window.
    unsafe {
        core::ptr::write_volatile(
            crate::mm::PhysAddr(base + offset)
                .direct_map_ptr()
                .cast::<u32>(),
            value,
        )
    }
}

#[inline(always)]
pub(super) fn read_reg(offset: usize) -> u32 {
    read_mmio(GMAC0_BASE, offset)
}

#[inline(always)]
pub(super) fn write_reg(offset: usize, value: u32) {
    write_mmio(GMAC0_BASE, offset, value)
}
