//! Trusted platform entropy sources.
//!
//! QEMU guests use the standard VirtIO entropy device. The 2K1000LA board
//! reads its integrated APB RNG register. This layer only transfers entropy;
//! conditioning and user-visible random streams belong to `crate::random`.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntropySource {
    Virtio,
    Loongson2k1000,
}

impl EntropySource {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Virtio => "virtio-rng",
            Self::Loongson2k1000 => "2k1000-rng",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntropyError {
    DeviceUnavailable,
    DeviceInit,
    DeviceRead,
    ShortRead,
}

#[cfg(feature = "boot_la_uboot_dmw")]
pub fn fill_entropy(dst: &mut [u8]) -> Result<EntropySource, EntropyError> {
    use core::ptr::read_volatile;
    use core::sync::atomic::{compiler_fence, Ordering};

    let register = crate::hal::arch::loongarch64::board::RNG_BASE as *const u32;
    for chunk in dst.chunks_mut(core::mem::size_of::<u32>()) {
        // The 2K1000LA manual defines each volatile read as one fresh 32-bit
        // random result. The APB register is reached through the inherited DMW.
        let word = unsafe { read_volatile(register) }.to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
        compiler_fence(Ordering::SeqCst);
    }
    Ok(EntropySource::Loongson2k1000)
}

#[cfg(feature = "boot_la_qemu")]
pub fn fill_entropy(dst: &mut [u8]) -> Result<EntropySource, EntropyError> {
    use crate::drivers::block::virtio_blk_pci::{enumerate_virtio_pci, VirtioHal};
    use virtio_drivers::device::rng::VirtIORng;
    use virtio_drivers::transport::DeviceType;

    if dst.is_empty() {
        return Ok(EntropySource::Virtio);
    }
    let transport =
        enumerate_virtio_pci(DeviceType::EntropySource).ok_or(EntropyError::DeviceUnavailable)?;
    let mut rng =
        VirtIORng::<VirtioHal, _>::new(transport).map_err(|_| EntropyError::DeviceInit)?;
    let mut offset = 0usize;
    while offset < dst.len() {
        let count = rng
            .request_entropy(&mut dst[offset..])
            .map_err(|_| EntropyError::DeviceRead)?;
        if count == 0 || count > dst.len() - offset {
            return Err(EntropyError::ShortRead);
        }
        offset += count;
    }
    Ok(EntropySource::Virtio)
}

#[cfg(target_arch = "riscv64")]
pub fn fill_entropy(dst: &mut [u8]) -> Result<EntropySource, EntropyError> {
    use crate::drivers::block::virtio_blk::VirtioHal;
    #[cfg(feature = "block_virt_pci")]
    use crate::drivers::block::virtio_blk_pci::VirtioHal;
    use crate::hal::device::DeviceManager;
    use alloc::vec::Vec;
    use core::ptr::NonNull;
    use virtio_drivers::device::rng::VirtIORng;
    use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};
    use virtio_drivers::transport::{DeviceType, Transport};

    let platform = crate::hal::platform::platform_info();
    let manager = DeviceManager::new(platform.devices.clone());
    let mut candidates: Vec<_> = manager
        .find_enabled_by_compatible("virtio,mmio")
        .into_iter()
        .filter(|device| device.mmio_range(0).is_some())
        .collect();
    candidates
        .sort_by_key(|device| device.mmio_range(0).map(|range| range.base).unwrap_or(usize::MAX));

    for candidate in candidates {
        let Some(range) = candidate.mmio_range(0) else {
            continue;
        };
        if range.base % core::mem::align_of::<VirtIOHeader>() != 0 {
            continue;
        }
        let Some(header) = NonNull::new(range.base as *mut VirtIOHeader) else {
            continue;
        };
        // SAFETY: [Categories 6 and 13 — aligned access and library contract]
        // The checked, FDT-derived MMIO range remains mapped for the kernel
        // lifetime, which is the platform driver's transport invariant.
        let Ok(transport) = (unsafe { MmioTransport::new(header, range.size) }) else {
            continue;
        };
        if transport.device_type() != DeviceType::EntropySource {
            continue;
        }
        let Ok(mut rng) = VirtIORng::<VirtioHal, _>::new(transport) else {
            continue;
        };
        let mut offset = 0usize;
        while offset < dst.len() {
            let count = rng
                .request_entropy(&mut dst[offset..])
                .map_err(|_| EntropyError::DeviceRead)?;
            if count == 0 || count > dst.len() - offset {
                return Err(EntropyError::ShortRead);
            }
            offset += count;
        }
        return Ok(EntropySource::Virtio);
    }

    Err(EntropyError::DeviceUnavailable)
}

#[cfg(not(any(
    feature = "boot_la_uboot_dmw",
    feature = "boot_la_qemu",
    target_arch = "riscv64"
)))]
pub fn fill_entropy(_dst: &mut [u8]) -> Result<EntropySource, EntropyError> {
    Err(EntropyError::DeviceUnavailable)
}
