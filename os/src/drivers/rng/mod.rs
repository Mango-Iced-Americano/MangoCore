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

#[cfg(feature = "board_2k1000")]
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

#[cfg(feature = "board_laqemu")]
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

#[cfg(feature = "board_rvqemu")]
pub fn fill_entropy(dst: &mut [u8]) -> Result<EntropySource, EntropyError> {
    use crate::drivers::block::virtio_blk::VirtioHal;
    use core::ptr::NonNull;
    use virtio_drivers::device::rng::VirtIORng;
    use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};

    const VIRTIO_RNG_BASE: usize = 0x1000_3000;

    if dst.is_empty() {
        return Ok(EntropySource::Virtio);
    }
    let header = NonNull::new(VIRTIO_RNG_BASE as *mut VirtIOHeader)
        .ok_or(EntropyError::DeviceUnavailable)?;
    let transport = unsafe { MmioTransport::new(header, 0x1000) }
        .map_err(|_| EntropyError::DeviceUnavailable)?;
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

#[cfg(not(any(
    feature = "board_2k1000",
    feature = "board_laqemu",
    feature = "board_rvqemu"
)))]
pub fn fill_entropy(_dst: &mut [u8]) -> Result<EntropySource, EntropyError> {
    Err(EntropyError::DeviceUnavailable)
}
