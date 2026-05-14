#![allow(dead_code)]

use super::addr::VirtRegion;
use super::{PhysAddr, VirtAddr};
use crate::config;

pub trait MemoryManagementArch {
    fn is_canonical(addr: usize) -> bool;
    fn is_user_addr(addr: VirtAddr) -> bool;
    fn is_user_region(region: VirtRegion) -> bool;
    fn phys_to_virt(pa: PhysAddr) -> VirtAddr;
    fn virt_to_phys(va: VirtAddr) -> Option<PhysAddr>;
    fn flush_tlb(addr: Option<VirtAddr>);
}

pub struct CurrentMmArch;

impl MemoryManagementArch for CurrentMmArch {
    fn is_canonical(addr: usize) -> bool {
        is_canonical_addr(addr)
    }

    fn is_user_addr(addr: VirtAddr) -> bool {
        super::layout::UserLayout::contains_user_addr(addr)
    }

    fn is_user_region(region: VirtRegion) -> bool {
        super::layout::UserLayout::checked_user_region(region.start(), region.len()).is_some()
    }

    fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
        VirtAddr(pa.0 | config::MEMORY_HIGH_BASE)
    }

    fn virt_to_phys(va: VirtAddr) -> Option<PhysAddr> {
        if super::layout::KernelLayout::contains_kernel_direct_addr(va) {
            Some(PhysAddr(va.0 & !config::MEMORY_HIGH_BASE))
        } else {
            None
        }
    }

    fn flush_tlb(_addr: Option<VirtAddr>) {
        crate::hal::tlb_invalidate();
    }
}

#[cfg(feature = "riscv")]
fn is_canonical_addr(addr: usize) -> bool {
    const SV39_BITS: usize = 39;
    const SV39_SIGN_BIT: usize = 1usize << (SV39_BITS - 1);
    const SV39_LOW_MASK: usize = (1usize << SV39_BITS) - 1;
    let high = addr & !SV39_LOW_MASK;
    if addr & SV39_SIGN_BIT == 0 {
        high == 0
    } else {
        high == !SV39_LOW_MASK
    }
}

#[cfg(feature = "loongarch64")]
fn is_canonical_addr(addr: usize) -> bool {
    let high = addr & config::SEG_MASK;
    high == 0 || high == config::SEG_MASK || high == config::HIGH_BASE_EIGHT
}

#[cfg(not(any(feature = "riscv", feature = "loongarch64")))]
fn is_canonical_addr(_addr: usize) -> bool {
    true
}
