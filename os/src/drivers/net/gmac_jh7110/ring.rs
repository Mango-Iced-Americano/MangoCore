use crate::config::PAGE_SIZE;
use crate::hal::{get_clock_freq, get_time};
use crate::mm::{frame_alloc, FrameTracker, PhysAddr};
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::mmio::{
    clean_dma_range, dma_barrier, GmacMmio, DMA_CH0_CUR_RX_DESC, DMA_CH0_RX_CONTROL,
    DMA_CH0_RX_END, DMA_CH0_STATUS, DMA_CH0_TX_END, GMAC_CONFIG, GMAC_DEBUG, GMAC_RXQ_CTRL0,
    MTL_RXQ0_OP_MODE,
};
use super::{dma_address, GmacJh7110Error};

pub(super) const RX_DESC_COUNT: usize = 64;
pub(super) const TX_DESC_COUNT: usize = 16;
pub(super) const DMA_BUFFER_SIZE: usize = 2048;
const DESC_SIZE: usize = 16;
const RX_DESC_OFFSET: usize = 0;
const TX_DESC_OFFSET: usize = RX_DESC_COUNT * DESC_SIZE;
const DESC_OWN: u32 = 1 << 31;
const TX_LAST: u32 = 1 << 28;
const TX_FIRST: u32 = 1 << 29;
const BUF1_VALID: u32 = 1 << 24; // RDES3_BUFFER1_VALID_ADDR for DWMAC4/5 normal RX descriptor
const RX_ERROR: u32 = 1 << 15;
const RX_FIRST: u32 = 1 << 29;
const RX_LAST: u32 = 1 << 28;
const RX_FRAME_LEN_MASK: u32 = 0x7fff;
const BUF_SIZE_MASK: u32 = 0x3fff;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct DmaDesc {
    des0: u32,
    des1: u32,
    des2: u32,
    des3: u32,
}

const _: () = assert!(core::mem::size_of::<DmaDesc>() == DESC_SIZE);
const _: () = assert!(TX_DESC_OFFSET + TX_DESC_COUNT * DESC_SIZE <= PAGE_SIZE);

// allow: SIZE_OK — DmaRings owns one coupled DMA descriptor lifecycle: allocation,
// cache maintenance, producer/consumer ownership transfer, and hardware tails.
pub(super) struct DmaRings {
    _descriptor_frame: Arc<FrameTracker>,
    descriptor_base: usize,
    rx_frames: Vec<Arc<FrameTracker>>,
    tx_frames: Vec<Arc<FrameTracker>>,
    rx_index: usize,
    tx_index: usize,
}

#[derive(Clone, Copy)]
pub(super) struct RingKtestResult {
    pub(super) tx_submitted: bool,
    pub(super) tx_own_cleared: bool,
    pub(super) rx_writeback: bool,
    pub(super) rx_descriptor_valid: bool,
    pub(super) dma_status: u32,
    pub(super) cur_rx_desc: u32,
    pub(super) mac_config: u32,
    pub(super) gmac_debug: u32,
    pub(super) rxq_ctrl0: u32, // 0x00a0
    pub(super) mtl_rxq_op: u32, // 0x0d30
    pub(super) dma_rx_ctrl: u32, // 0x1108
}

impl DmaRings {
    pub(super) fn allocate() -> Result<Self, GmacJh7110Error> {
        let descriptor_frame = frame_alloc().ok_or(GmacJh7110Error::OutOfMemory)?;
        let descriptor_base = dma_address(&descriptor_frame)?;
        let mut rx_frames = Vec::new();
        rx_frames
            .try_reserve_exact(RX_DESC_COUNT)
            .map_err(|_| GmacJh7110Error::OutOfMemory)?;
        let mut tx_frames = Vec::new();
        tx_frames
            .try_reserve_exact(TX_DESC_COUNT)
            .map_err(|_| GmacJh7110Error::OutOfMemory)?;
        for _ in 0..RX_DESC_COUNT {
            let frame = frame_alloc().ok_or(GmacJh7110Error::OutOfMemory)?;
            dma_address(&frame)?;
            rx_frames.push(frame);
        }
        for _ in 0..TX_DESC_COUNT {
            let frame = frame_alloc().ok_or(GmacJh7110Error::OutOfMemory)?;
            dma_address(&frame)?;
            tx_frames.push(frame);
        }
        let rings = Self {
            _descriptor_frame: descriptor_frame,
            descriptor_base,
            rx_frames,
            tx_frames,
            rx_index: 0,
            tx_index: 0,
        };
        rings.initialize()?;
        Ok(rings)
    }

    pub(super) fn rx_descriptor_base(&self) -> usize {
        self.descriptor_base + RX_DESC_OFFSET
    }

    pub(super) fn tx_descriptor_base(&self) -> usize {
        self.descriptor_base + TX_DESC_OFFSET
    }

    pub(super) fn rx_descriptor_end(&self) -> usize {
        self.rx_descriptor_base() + (RX_DESC_COUNT - 1) * DESC_SIZE
    }

    fn rx_descriptor_address(&self, index: usize) -> usize {
        self.rx_descriptor_base() + index * DESC_SIZE
    }

    fn tx_descriptor_address(&self, index: usize) -> usize {
        self.tx_descriptor_base() + index * DESC_SIZE
    }

    fn rx_descriptor(&self, index: usize) -> *mut DmaDesc {
        PhysAddr(self.rx_descriptor_address(index))
            .direct_map_ptr()
            .cast::<DmaDesc>()
    }

    fn tx_descriptor(&self, index: usize) -> *mut DmaDesc {
        PhysAddr(self.tx_descriptor_address(index))
            .direct_map_ptr()
            .cast::<DmaDesc>()
    }

    fn initialize(&self) -> Result<(), GmacJh7110Error> {
        for index in 0..RX_DESC_COUNT {
            let buffer = dma_address(&self.rx_frames[index])?;
            clean_dma_range(buffer, DMA_BUFFER_SIZE);
            // DWMAC5 normal (16-byte) RX descriptor: des0/des1 = buffer address, des2 = buffer size.
            let value = DmaDesc {
                des0: buffer as u32,
                des1: 0,
                des2: DMA_BUFFER_SIZE as u32 & BUF_SIZE_MASK,
                des3: DESC_OWN | BUF1_VALID,
            };
            unsafe { core::ptr::write_volatile(self.rx_descriptor(index), value) };
        }
        for index in 0..TX_DESC_COUNT {
            unsafe { core::ptr::write_volatile(self.tx_descriptor(index), core::mem::zeroed()) };
        }
        clean_dma_range(
            self.descriptor_base,
            (RX_DESC_COUNT + TX_DESC_COUNT) * DESC_SIZE,
        );
        dma_barrier();
        Ok(())
    }

    pub(super) fn receive(&mut self, regs: GmacMmio, output: &mut [u8]) -> Option<usize> {
        for _ in 0..RX_DESC_COUNT {
            let index = self.rx_index;
            let descriptor = self.rx_descriptor(index);
            clean_dma_range(self.rx_descriptor_address(index), DESC_SIZE);
            dma_barrier();
            let des3 = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*descriptor).des3)) };
            if des3 & DESC_OWN != 0 {
                return None;
            }
            let raw_len = (des3 & RX_FRAME_LEN_MASK) as usize;
            let valid = des3 & RX_ERROR == 0
                && des3 & RX_FIRST != 0
                && des3 & RX_LAST != 0
                && raw_len >= 14
                && raw_len <= DMA_BUFFER_SIZE;
            let length = raw_len.min(output.len());
            let buffer = match dma_address(&self.rx_frames[index]) {
                Ok(address) => address,
                Err(_) => return None,
            };
            if valid && length > 0 {
                clean_dma_range(buffer, length);
                dma_barrier();
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        PhysAddr(buffer).direct_map_ptr().cast_const(),
                        output.as_mut_ptr(),
                        length,
                    )
                };
            }
            // Recycle descriptor: restore des0/des1 (buffer addr) and des2 (buffer size).
            unsafe {
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!((*descriptor).des0),
                    buffer as u32,
                );
                core::ptr::write_volatile(core::ptr::addr_of_mut!((*descriptor).des2), DMA_BUFFER_SIZE as u32 & BUF_SIZE_MASK);
            }
            dma_barrier();
            clean_dma_range(buffer, DMA_BUFFER_SIZE);
            self.rx_index = (index + 1) % RX_DESC_COUNT;
            unsafe {
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!((*descriptor).des3),
                    DESC_OWN | BUF1_VALID,
                );
            }
            clean_dma_range(self.rx_descriptor_address(index), DESC_SIZE);
            dma_barrier();
            regs.write(DMA_CH0_RX_END, self.rx_descriptor_address(index) as u32);
            if valid {
                return Some(length);
            }
        }
        None
    }

    pub(super) fn transmit(&mut self, regs: GmacMmio, input: &[u8]) -> Option<usize> {
        if input.is_empty() || input.len() > DMA_BUFFER_SIZE {
            return None;
        }
        let index = self.tx_index;
        let descriptor = self.tx_descriptor(index);
        clean_dma_range(self.tx_descriptor_address(index), DESC_SIZE);
        dma_barrier();
        if unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*descriptor).des3)) } & DESC_OWN != 0 {
            return None;
        }
        let buffer = match dma_address(&self.tx_frames[index]) {
            Ok(address) => address,
            Err(_) => return None,
        };
        let length = input.len().max(60);
        unsafe {
            let buffer_ptr = PhysAddr(buffer).direct_map_ptr();
            core::ptr::copy_nonoverlapping(input.as_ptr(), buffer_ptr, input.len());
            if length > input.len() {
                core::ptr::write_bytes(
                    buffer_ptr.add(input.len()),
                    0,
                    length - input.len(),
                );
            }
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*descriptor).des0),
                buffer as u32,
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*descriptor).des1),
                0,
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*descriptor).des2),
                length as u32 & BUF_SIZE_MASK,
            );
        }
        clean_dma_range(buffer, length);
        dma_barrier();
        unsafe {
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!((*descriptor).des3),
                DESC_OWN | TX_FIRST | TX_LAST | (length as u32 & RX_FRAME_LEN_MASK),
            );
        }
        clean_dma_range(self.tx_descriptor_address(index), DESC_SIZE);
        dma_barrier();
        self.tx_index = (index + 1) % TX_DESC_COUNT;
        regs.write(
            DMA_CH0_TX_END,
            self.tx_descriptor_address(self.tx_index) as u32,
        );
        Some(index)
    }

    pub(super) fn ktest_probe(&mut self, regs: GmacMmio, frame: &[u8]) -> RingKtestResult {
        let tx_index = self.transmit(regs, frame);
        let tx_own_cleared = match tx_index {
            Some(index) => {
                let start = get_time();
                let timeout = (get_clock_freq() / 10).max(1);
                loop {
                    let descriptor = self.tx_descriptor(index);
                    clean_dma_range(self.tx_descriptor_address(index), DESC_SIZE);
                    dma_barrier();
                    let des3 = unsafe {
                        core::ptr::read_volatile(core::ptr::addr_of!((*descriptor).des3))
                    };
                    if des3 & DESC_OWN == 0 {
                        break true;
                    }
                    if get_time().wrapping_sub(start) >= timeout {
                        break false;
                    }
                    core::hint::spin_loop();
                }
            }
            None => false,
        };

        let (rx_writeback, rx_descriptor_valid) = if tx_own_cleared {
            let start = get_time();
            let timeout = (get_clock_freq() / 10).max(1);
            let mut rx_writeback = false;
            loop {
                let mut rx_descriptor_valid = false;
                for index in 0..RX_DESC_COUNT {
                    let descriptor = self.rx_descriptor(index);
                    clean_dma_range(self.rx_descriptor_address(index), DESC_SIZE);
                    dma_barrier();
                    let des3 = unsafe {
                        core::ptr::read_volatile(core::ptr::addr_of!((*descriptor).des3))
                    };
                    let frame_length = (des3 & RX_FRAME_LEN_MASK) as usize;
                    let observed_writeback = des3 & DESC_OWN == 0;
                    let observed_descriptor_valid = observed_writeback
                        && des3 & RX_ERROR == 0
                        && des3 & RX_FIRST != 0
                        && des3 & RX_LAST != 0
                        && (14..=DMA_BUFFER_SIZE).contains(&frame_length);
                    rx_writeback |= observed_writeback;
                    if observed_descriptor_valid {
                        rx_descriptor_valid = true;
                        break;
                    }
                }
                if rx_descriptor_valid {
                    break (rx_writeback, true);
                }
                if get_time().wrapping_sub(start) >= timeout {
                    break (rx_writeback, false);
                }
                core::hint::spin_loop();
            }
        } else {
            (false, false)
        };
        let rxq_ctrl0 = regs.read(GMAC_RXQ_CTRL0);
        let mtl_rxq_op = regs.read(MTL_RXQ0_OP_MODE);
        let dma_rx_ctrl = regs.read(DMA_CH0_RX_CONTROL);
        let dma_status = regs.read(DMA_CH0_STATUS);
        let cur_rx_desc = regs.read(DMA_CH0_CUR_RX_DESC);
        let mac_config = regs.read(GMAC_CONFIG);
        let gmac_debug = regs.read(GMAC_DEBUG);

        RingKtestResult {
            tx_submitted: tx_index.is_some(),
            tx_own_cleared,
            rx_writeback,
            rx_descriptor_valid,
            dma_status,
            cur_rx_desc,
            mac_config,
            gmac_debug,
            rxq_ctrl0,
            mtl_rxq_op,
            dma_rx_ctrl,
        }
    }
}
