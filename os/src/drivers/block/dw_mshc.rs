//! DesignWare Mobile Storage Host Controller block driver for JH7110 SDIO1.

mod jh7110;
mod ktest;
mod mmio;
mod sd;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::TryFrom;
use spin::Mutex;

use crate::drivers::block::{
    validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceResult,
};
use crate::hal::device::DeviceManager;
use crate::hal::BLOCK_SZ;

use jh7110::Jh7110MshcConfig;
use mmio::DwMshcHost;
use sd::SdCardInfo;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DwMshcError {
    MalformedFdt,
    UnsupportedController,
    ClockResetTimeout,
    CoreResetTimeout,
    CommandTimeout(u8),
    ResponseCrc(u8),
    CardStatus(u8, u32),
    DataTimeout,
    DataCrc,
    FifoRun,
    StartBit,
    EndBit,
    HardwareLocked,
    ShortTransfer,
    UnsupportedCard,
    OutOfRange,
}

/// A serialized, read-only SD card backed by the JH7110 SDIO1 controller.
pub struct DwMshc(Mutex<DwMshcInner>);

struct DwMshcInner {
    host: DwMshcHost,
    card: SdCardInfo,
}

impl DwMshc {
    fn try_new(config: Jh7110MshcConfig) -> Result<Self, DwMshcError> {
        jh7110::enable_clocks_and_release_reset(config.reset_id)?;
        let mut host = DwMshcHost::new(config);
        host.initialize()?;
        let card = sd::initialize_card(&mut host)?;
        let mut sector_zero = [0u8; 512];
        host.read_sector(&card, 0, &mut sector_zero)?;
        Ok(Self(Mutex::new(DwMshcInner { host, card })))
    }
}

/// Probe only the validated VF2 SDIO1 binding. Failed cards are never published.
pub(crate) fn probe_from_device_manager(manager: &DeviceManager) -> Vec<Arc<dyn BlockDevice>> {
    let mut devices = manager.find_enabled_by_compatible("snps,dw-mshc");
    devices.sort_by_key(|device| device.mmio_range(0).map(|range| range.base).unwrap_or(usize::MAX));
    let mut found = Vec::new();
    for device in devices {
        let Ok(config) = jh7110::discover_v1(device) else {
            continue;
        };
        match DwMshc::try_new(config) {
            Ok(driver) => found.push(Arc::new(driver) as Arc<dyn BlockDevice>),
            Err(error) => crate::println!("[dw_mshc] SDIO1 probe skipped: {:?}", error),
        }
    }
    found
}

impl BlockDevice for DwMshc {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        let first_sector = block_first_sector(block_id).ok_or(BlockDeviceError::OutOfBounds)?;
        let count = buf.len() / 512;
        let end_sector = first_sector.checked_add(count).ok_or(BlockDeviceError::OutOfBounds)?;
        let mut inner = self.0.lock();
        if u64::try_from(end_sector).map_or(true, |end| end > inner.card.capacity_sectors) {
            return Err(BlockDeviceError::OutOfBounds);
        }
        let card = inner.card;
        for (offset, sector) in (first_sector..end_sector).enumerate() {
            inner
                .host
                .read_sector(&card, sector as u64, &mut buf[offset * 512..(offset + 1) * 512])
                .map_err(|_| BlockDeviceError::DeviceError)?;
        }
        Ok(())
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> BlockDeviceResult {
        Err(BlockDeviceError::DeviceError)
    }

    fn size_bytes(&self) -> Option<u64> {
        let sectors = self.0.lock().card.capacity_sectors;
        sectors.checked_mul(512).map(|bytes| bytes / BLOCK_SZ as u64 * BLOCK_SZ as u64)
    }
}

pub(crate) fn ktests() -> Vec<crate::kernel_tests::runner::KernelTest> {
    ktest::tests()
}

pub(crate) const fn block_first_sector(block_id: usize) -> Option<usize> {
    block_id.checked_mul(BLOCK_SZ / 512)
}
