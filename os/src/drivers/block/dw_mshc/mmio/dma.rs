use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::TryFrom;

use crate::config::PAGE_SIZE;
use crate::hal::platform::jh7110_cache::jh7110_l2cc_flush_range;
use crate::mm::{frames_alloc_fresh_contiguous, FrameTracker};
use crate::timer;

use super::super::DwMshcError;
use super::transfer::transfer_command;
use super::{
    idmac_control, DwMshcHost, IdmacDirection, BMOD, BMOD_RESET, CTRL, DBADDR, IDINTEN,
    IDMAC_BOUNCE_BYTES, IDSTS, PLDMND, RINTSTS,
};

// allow: SIZE_OK — descriptor ownership, cache maintenance, and completion
// form one indivisible IDMAC transfer state machine.
const IDMAC_DESCRIPTOR_BYTES: usize = 16;
const IDMAC_DESCRIPTOR_COUNT: usize = PAGE_SIZE / IDMAC_DESCRIPTOR_BYTES;
const IDMAC_BUFFER_BYTES: usize = PAGE_SIZE;
const IDMAC_BOUNCE_PAGES: usize = 16;
const DMA32_EXCLUSIVE_END: usize = 1usize << 32;
const _: [(); IDMAC_BOUNCE_PAGES * PAGE_SIZE] = [(); IDMAC_BOUNCE_BYTES];

const IDMAC_DES0_ER: u32 = 1 << 5;
const IDMAC_DES1_BS1_MASK: u32 = 0x1fff;

const BMOD_FIXED_BURST: u32 = 1 << 1;
const BMOD_ENABLE: u32 = 1 << 7;
const CTRL_DMA_RESET: u32 = 1 << 2;
const CTRL_DMA_ENABLE: u32 = 1 << 5;
const CTRL_USE_IDMAC: u32 = 1 << 25;
const IDSTS_TI: u32 = 1 << 0;
const IDSTS_RI: u32 = 1 << 1;
const IDSTS_FBE: u32 = 1 << 2;
const IDSTS_DU: u32 = 1 << 4;
const IDSTS_CES: u32 = 1 << 5;
const IDSTS_NI: u32 = 1 << 8;
const IDSTS_AI: u32 = 1 << 9;
const IDSTS_COMPLETED: u32 = IDSTS_TI | IDSTS_RI;
const IDSTS_ERRORS: u32 = IDSTS_AI | IDSTS_CES | IDSTS_DU | IDSTS_FBE;
const IDSTS_ALL: u32 = IDSTS_COMPLETED | IDSTS_ERRORS | IDSTS_NI;

/// The 32-bit DesignWare IDMAC descriptor layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct IdmacDescriptor {
    control: u32,
    size: u32,
    buffer: u32,
    next: u32,
}

const _: [(); IDMAC_DESCRIPTOR_BYTES] = [(); core::mem::size_of::<IdmacDescriptor>()];

/// Permanently owned, physically contiguous IDMAC storage.
pub(super) struct DmaResources {
    descriptor_frame: Arc<FrameTracker>,
    descriptor_pa: u32,
    bounce_frames: Vec<Arc<FrameTracker>>,
    bounce_pa: u32,
}

impl DmaResources {
    pub(super) fn new() -> Option<Self> {
        let descriptor_frame = frames_alloc_fresh_contiguous(1)?.into_iter().next()?;
        let descriptor_start = descriptor_frame.ppn.start_addr().0;
        if descriptor_start.checked_add(PAGE_SIZE)? > DMA32_EXCLUSIVE_END {
            return None;
        }
        let descriptor_pa = u32::try_from(descriptor_start).ok()?;
        let bounce_frames = frames_alloc_fresh_contiguous(IDMAC_BOUNCE_PAGES)?;
        let bounce_start = bounce_frames.first()?.ppn.start_addr().0;
        if bounce_start.checked_add(IDMAC_BOUNCE_BYTES)? > DMA32_EXCLUSIVE_END {
            return None;
        }
        let bounce_pa = u32::try_from(bounce_start).ok()?;
        let resources = Self {
            descriptor_frame,
            descriptor_pa,
            bounce_frames,
            bounce_pa,
        };
        resources.initialize_ring();
        Some(resources)
    }

    fn initialize_ring(&self) {
        // SAFETY: [Category 13 — library unsafe contract] `descriptor_frame`
        // is uniquely owned by this DmaResources instance and no DMA transfer
        // is active while its descriptor ring is initialized.
        unsafe {
            self.descriptor_frame.ppn.with_bytes_mut(|ring| {
                ring.fill(0);
                for index in 0..IDMAC_DESCRIPTOR_COUNT {
                    let next = if index + 1 == IDMAC_DESCRIPTOR_COUNT {
                        self.descriptor_pa
                    } else {
                        self.descriptor_pa + ((index + 1) * IDMAC_DESCRIPTOR_BYTES) as u32
                    };
                    write_descriptor_word(ring, index, 3, next);
                }
                write_descriptor_word(ring, IDMAC_DESCRIPTOR_COUNT - 1, 0, IDMAC_DES0_ER);
            })
        };
    }

    fn prepare(&self, bytes: usize) -> Result<(), DwMshcError> {
        let descriptors = bytes.div_ceil(IDMAC_BUFFER_BYTES);
        if descriptors == 0 || descriptors > IDMAC_DESCRIPTOR_COUNT {
            return Err(DwMshcError::OutOfRange);
        }
        // SAFETY: [Category 13 — library unsafe contract] `prepare()` writes
        // the private descriptor frame before the host is told to start DMA.
        unsafe {
            self.descriptor_frame.ppn.with_bytes_mut(|ring| {
                for index in 0..descriptors {
                    let offset = index * IDMAC_BUFFER_BYTES;
                    let length = (bytes - offset).min(IDMAC_BUFFER_BYTES) as u32;
                    write_descriptor_word(ring, index, 0, idmac_control(index, descriptors));
                    write_descriptor_word(ring, index, 1, length & IDMAC_DES1_BS1_MASK);
                    write_descriptor_word(ring, index, 2, self.bounce_pa + offset as u32);
                }
            })
        };
        Ok(())
    }

    fn copy_to_bounce(&self, source: &[u8]) -> Result<(), DwMshcError> {
        if source.len() > IDMAC_BOUNCE_BYTES {
            return Err(DwMshcError::OutOfRange);
        }
        let mut copied = 0;
        for frame in &self.bounce_frames {
            if copied == source.len() {
                break;
            }
            let count = (source.len() - copied).min(PAGE_SIZE);
            // SAFETY: [Category 13 — library unsafe contract] bounce frames
            // belong exclusively to this request and are filled before DMA.
            unsafe {
                frame.ppn.with_bytes_mut(|page| {
                    page[..count].copy_from_slice(&source[copied..copied + count]);
                })
            };
            copied += count;
        }
        Ok(())
    }

    fn copy_from_bounce(&self, destination: &mut [u8]) -> Result<(), DwMshcError> {
        if destination.len() > IDMAC_BOUNCE_BYTES {
            return Err(DwMshcError::OutOfRange);
        }
        let mut copied = 0;
        for frame in &self.bounce_frames {
            if copied == destination.len() {
                break;
            }
            let count = (destination.len() - copied).min(PAGE_SIZE);
            // SAFETY: [Category 13 — library unsafe contract] completion has
            // quiesced the DMA request before callers copy from bounce frames.
            unsafe {
                frame.ppn.with_bytes(|page| {
                    destination[copied..copied + count].copy_from_slice(&page[..count]);
                })
            };
            copied += count;
        }
        Ok(())
    }
}

impl DwMshcHost {
    pub(super) fn initialize_idmac(&mut self) -> Result<(), DwMshcError> {
        let Some(dma) = self.dma.as_ref() else {
            return Ok(());
        };
        dma.initialize_ring();
        self.write(BMOD, BMOD_RESET);
        self.wait_clear(BMOD, BMOD_RESET, 500, DwMshcError::CoreResetTimeout)?;
        self.write(IDSTS, IDSTS_ALL);
        self.write(IDINTEN, IDSTS_NI | IDSTS_RI | IDSTS_TI);
        self.write(DBADDR, dma.descriptor_pa);
        Ok(())
    }

    pub(crate) fn dma_supported(&self, bytes: usize) -> bool {
        self.dma.is_some() && bytes != 0 && bytes <= IDMAC_BOUNCE_BYTES
    }

    pub(super) fn read_dma_blocks_once(
        &mut self,
        argument: u32,
        sectors: usize,
        out: &mut [u8],
    ) -> Result<(), DwMshcError> {
        let command = transfer_command(sectors, false);
        let result = (|| {
            self.prepare_data_transfer(out.len())?;
            self.start_idmac(out.len())?;
            self.start_data_command(command, argument, false)?;
            self.wait_idmac_complete(command, IdmacDirection::Read)?;
            Ok(())
        })();
        if result.is_err() {
            self.capture_transfer_failure_if_empty();
        }
        self.stop_idmac();
        result?;
        let dma = self.dma.as_ref().ok_or(DwMshcError::DmaFault)?;
        // Device-to-CPU DMA can leave stale bounce lines in the CPU cache.
        // FLUSH64 invalidates them before the CPU copies the card data out.
        jh7110_l2cc_flush_range(dma.bounce_pa as usize, out.len());
        dma.copy_from_bounce(out)?;
        self.finish_data_transfer(sectors)
    }

    pub(super) fn write_dma_blocks_once(
        &mut self,
        argument: u32,
        sectors: usize,
        data: &[u8],
    ) -> Result<(), DwMshcError> {
        let dma = self.dma.as_ref().ok_or(DwMshcError::DmaFault)?;
        dma.copy_to_bounce(data)?;
        // CPU-to-device DMA must observe the freshly copied bounce payload
        // before ownership transfers to IDMAC.
        jh7110_l2cc_flush_range(dma.bounce_pa as usize, data.len());
        let command = transfer_command(sectors, true);
        let result = (|| {
            self.prepare_data_transfer(data.len())?;
            self.start_idmac(data.len())?;
            self.start_data_command(command, argument, true)?;
            self.wait_idmac_complete(command, IdmacDirection::Write)
        })();
        if result.is_err() {
            self.capture_transfer_failure_if_empty();
        }
        self.stop_idmac();
        result?;
        self.finish_data_transfer(sectors)
    }

    fn start_idmac(&mut self, bytes: usize) -> Result<(), DwMshcError> {
        let dma = self.dma.as_ref().ok_or(DwMshcError::DmaFault)?;
        dma.prepare(bytes)?;
        // Descriptor ownership and addresses are CPU writes. Flush the complete
        // ring before PLDMND so IDMAC cannot consume stale descriptor words.
        jh7110_l2cc_flush_range(dma.descriptor_pa as usize, PAGE_SIZE);
        self.write(CTRL, self.read(CTRL) | CTRL_DMA_RESET);
        self.wait_clear(CTRL, CTRL_DMA_RESET, 500, DwMshcError::CoreResetTimeout)?;
        self.write(BMOD, BMOD_RESET);
        self.wait_clear(BMOD, BMOD_RESET, 500, DwMshcError::CoreResetTimeout)?;
        self.write(IDSTS, IDSTS_ALL);
        self.write(IDINTEN, IDSTS_NI | IDSTS_RI | IDSTS_TI);
        self.write(DBADDR, dma.descriptor_pa);
        self.write(CTRL, self.read(CTRL) | CTRL_USE_IDMAC | CTRL_DMA_ENABLE);
        self.write(BMOD, self.read(BMOD) | BMOD_ENABLE | BMOD_FIXED_BURST);
        self.write(PLDMND, 1);
        Ok(())
    }

    pub(super) fn stop_idmac(&mut self) {
        if self.dma.is_none() {
            return;
        }
        self.write(
            CTRL,
            self.read(CTRL) & !(CTRL_USE_IDMAC | CTRL_DMA_ENABLE) | CTRL_DMA_RESET,
        );
        let _ = self.wait_clear(CTRL, CTRL_DMA_RESET, 500, DwMshcError::CoreResetTimeout);
        self.write(
            BMOD,
            self.read(BMOD) & !(BMOD_ENABLE | BMOD_FIXED_BURST) | BMOD_RESET,
        );
        let _ = self.wait_clear(BMOD, BMOD_RESET, 500, DwMshcError::CoreResetTimeout);
        self.write(IDSTS, IDSTS_ALL);
    }

    fn wait_idmac_complete(
        &mut self,
        command: u8,
        direction: IdmacDirection,
    ) -> Result<(), DwMshcError> {
        let deadline = timer::get_time_ms().saturating_add(500);
        let mut dma_done = false;
        let mut data_done = false;
        loop {
            let idsts = self.read(IDSTS);
            if idsts & IDSTS_ERRORS != 0 {
                self.capture_transfer_failure();
                self.write(IDSTS, idsts);
                return Err(DwMshcError::DmaFault);
            }
            match idmac_completion_matches(direction, idsts) {
                Ok(true) => {
                    self.write(IDSTS, idsts & (IDSTS_COMPLETED | IDSTS_NI));
                    dma_done = true;
                }
                Ok(false) => {}
                Err(error) => {
                    self.capture_transfer_failure();
                    self.write(IDSTS, idsts & (IDSTS_COMPLETED | IDSTS_NI));
                    return Err(error);
                }
            }

            let status = self.read(RINTSTS);
            self.check_data_status(command, status)?;
            self.acknowledge_data_status(status, 0);
            if status & super::INT_DATA_OVER != 0 {
                self.write(RINTSTS, super::INT_DATA_OVER);
                data_done = true;
            }
            if dma_done && data_done {
                return Ok(());
            }
            if timer::get_time_ms() >= deadline {
                self.capture_transfer_failure();
                return Err(DwMshcError::DataTimeout);
            }
            core::hint::spin_loop();
        }
    }
}

pub(crate) const fn idmac_completion_matches(
    direction: IdmacDirection,
    idsts: u32,
) -> Result<bool, DwMshcError> {
    let (expected, opposite) = match direction {
        IdmacDirection::Read => (IDSTS_RI, IDSTS_TI),
        IdmacDirection::Write => (IDSTS_TI, IDSTS_RI),
    };
    if idsts & opposite != 0 {
        Err(DwMshcError::DmaDirectionMismatch)
    } else {
        Ok(idsts & expected != 0)
    }
}

fn write_descriptor_word(ring: &mut [u8], index: usize, word: usize, value: u32) {
    let offset = index * IDMAC_DESCRIPTOR_BYTES + word * core::mem::size_of::<u32>();
    ring[offset..offset + core::mem::size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}
