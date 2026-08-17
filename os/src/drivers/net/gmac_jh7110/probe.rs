use super::mmio::{DMA_CH0_STATUS, JH7110_GMAC0_BASE};
use super::GmacJh7110;
use crate::drivers::net::NetDevice;
use crate::hal::device::DeviceManager;
use crate::hal::platform::info::{DeviceInfo, RawPropertyValidity, ResourceValidity};
use alloc::sync::Arc;
use core::convert::TryInto;

const GMAC1_BASE: usize = 0x1604_0000;
const GMAC_REQUIRED_MMIO_SIZE: usize = DMA_CH0_STATUS + core::mem::size_of::<u32>();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GmacResources {
    pub(crate) base: usize,
    pub(crate) irq: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GmacDiscoveryError {
    Ineligible,
    MissingMmioRange,
    UnsupportedGmac1,
    UnsupportedInstance(usize),
    ShortMmioRange(usize),
    MissingInterrupt,
    InvalidInterrupt,
}

/// Finds the first fully described JH7110 GMAC0 node without accessing its MMIO range.
pub(crate) fn discover_gmac0_resources(dm: &DeviceManager) -> Option<GmacResources> {
    dm.all_devices()
        .iter()
        .find_map(|device| gmac_resources(device).ok().flatten())
}

/// Instantiates the first usable JH7110 GMAC0 described by firmware.
pub(crate) fn probe_gmac_from_device_manager(dm: &DeviceManager) -> Option<Arc<dyn NetDevice>> {
    for device in dm.all_devices() {
        let resources = match gmac_resources(device) {
            Ok(Some(resources)) => resources,
            Ok(None) => continue,
            Err(GmacDiscoveryError::UnsupportedGmac1) => {
                println!(
                    "[gmac-jh7110] skip GMAC1 node={} mmio={:#x}: L1 supports GMAC0 only",
                    device.node_path, GMAC1_BASE
                );
                continue;
            }
            Err(error) => {
                if is_gmac_compatible(device) {
                    println!(
                        "[gmac-jh7110] FDT node={} rejected error={:?}",
                        device.node_path, error
                    );
                }
                continue;
            }
        };
        match GmacJh7110::new(resources.base, resources.irq) {
            Ok(net_device) => {
                println!(
                    "[gmac-jh7110] FDT node={} mmio={:#x} irq={}",
                    device.node_path, resources.base, resources.irq
                );
                return Some(Arc::new(net_device));
            }
            Err(error) => println!(
                "[gmac-jh7110] probe failed node={} error={:?}",
                device.node_path, error
            ),
        }
    }
    None
}

fn gmac_resources(device: &DeviceInfo) -> Result<Option<GmacResources>, GmacDiscoveryError> {
    if !is_gmac_compatible(device) {
        return Ok(None);
    }
    if !device.is_enabled()
        || device.resource_validity != ResourceValidity::Valid
        || device.raw_property_validity != RawPropertyValidity::Valid
    {
        return Err(GmacDiscoveryError::Ineligible);
    }
    let range = device
        .mmio_range(0)
        .ok_or(GmacDiscoveryError::MissingMmioRange)?;
    if range.base == GMAC1_BASE {
        return Err(GmacDiscoveryError::UnsupportedGmac1);
    }
    if range.base != JH7110_GMAC0_BASE {
        return Err(GmacDiscoveryError::UnsupportedInstance(range.base));
    }
    if range.size < GMAC_REQUIRED_MMIO_SIZE {
        return Err(GmacDiscoveryError::ShortMmioRange(range.size));
    }
    let interrupt: [u8; 4] = device
        .raw_property("interrupts")
        .map_err(|_| GmacDiscoveryError::MissingInterrupt)?
        .get(..core::mem::size_of::<u32>())
        .ok_or(GmacDiscoveryError::MissingInterrupt)?
        .try_into()
        .map_err(|_| GmacDiscoveryError::MissingInterrupt)?;
    let irq = u32::from_be_bytes(interrupt) as usize;
    if irq == 0 {
        return Err(GmacDiscoveryError::InvalidInterrupt);
    }
    Ok(Some(GmacResources {
        base: range.base,
        irq,
    }))
}

fn is_gmac_compatible(device: &DeviceInfo) -> bool {
    let compatible = |value| device.compatible.iter().any(|candidate| candidate == value);
    compatible("starfive,jh7110-eqos-5.20")
        || compatible("starfive,jh7110-dwmac")
        || (compatible("starfive,dwmac") && compatible("snps,dwmac-5.10a"))
}
