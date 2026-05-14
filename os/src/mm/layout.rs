#![allow(dead_code)]

use super::addr::VirtRegion;
use super::VirtAddr;
use crate::config;

pub struct UserLayout;

impl UserLayout {
    pub const BASE: VirtAddr = VirtAddr(config::USER_VA_BASE);
    pub const END: VirtAddr = VirtAddr(config::USER_VA_END);
    pub const TASK_SIZE: usize = config::TASK_SIZE;
    pub const STACK_BASE: VirtAddr = VirtAddr(config::USER_STACK_BASE);
    pub const STACK_SIZE: usize = config::USER_STACK_SIZE;
    pub const HEAP_SIZE: usize = config::USER_HEAP_SIZE;
    pub const ELF_DYN_BASE: VirtAddr = VirtAddr(config::ELF_DYN_BASE);
    pub const MMAP_BASE: VirtAddr = VirtAddr(user_mmap_base());
    pub const MMAP_END: VirtAddr = VirtAddr(user_mmap_end());

    pub fn contains_user_addr(addr: VirtAddr) -> bool {
        Self::BASE <= addr && addr < Self::END
    }

    pub fn checked_user_region(start: VirtAddr, len: usize) -> Option<VirtRegion> {
        let end = VirtAddr(start.0.checked_add(len)?);
        let region = VirtRegion::checked_new(start, end)?;
        let start_ok = if len == 0 {
            Self::BASE <= start && start <= Self::END
        } else {
            Self::contains_user_addr(start)
        };
        (start_ok && end <= Self::END).then_some(region)
    }
}

pub struct KernelLayout;

impl KernelLayout {
    pub const MEMORY_START: VirtAddr = VirtAddr(config::MEMORY_START | config::MEMORY_HIGH_BASE);
    pub const MEMORY_END: VirtAddr = VirtAddr(config::MEMORY_END | config::MEMORY_HIGH_BASE);
    pub const MEMORY_HIGH_BASE: VirtAddr = VirtAddr(config::MEMORY_HIGH_BASE);
    pub const TRAMPOLINE: VirtAddr = VirtAddr(config::TRAMPOLINE);
    pub const SIGNAL_TRAMPOLINE: VirtAddr = VirtAddr(config::SIGNAL_TRAMPOLINE);
    pub const TRAP_CONTEXT_BASE: VirtAddr = VirtAddr(config::TRAP_CONTEXT_BASE);

    pub fn contains_kernel_direct_addr(addr: VirtAddr) -> bool {
        Self::MEMORY_START <= addr && addr < Self::MEMORY_END
    }

    pub fn checked_kernel_direct_region(start: VirtAddr, len: usize) -> Option<VirtRegion> {
        let end = VirtAddr(start.0.checked_add(len)?);
        let region = VirtRegion::checked_new(start, end)?;
        let start_ok = if len == 0 {
            Self::MEMORY_START <= start && start <= Self::MEMORY_END
        } else {
            Self::contains_kernel_direct_addr(start)
        };
        (start_ok && end <= Self::MEMORY_END).then_some(region)
    }
}

#[cfg(feature = "loongarch64")]
const fn user_mmap_base() -> usize {
    config::USR_MMAP_BASE
}

#[cfg(not(feature = "loongarch64"))]
const fn user_mmap_base() -> usize {
    config::MMAP_BASE
}

#[cfg(feature = "loongarch64")]
const fn user_mmap_end() -> usize {
    config::USR_MMAP_END
}

#[cfg(not(feature = "loongarch64"))]
const fn user_mmap_end() -> usize {
    config::MMAP_END
}
