#![allow(dead_code)]

pub use super::address::*;
use crate::config::PAGE_SIZE;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VirtRegion {
    start: VirtAddr,
    end: VirtAddr,
}

impl VirtRegion {
    pub fn new(start: VirtAddr, end: VirtAddr) -> Self {
        assert!(start <= end, "virt region start must not exceed end");
        Self { start, end }
    }

    pub fn checked_new(start: VirtAddr, end: VirtAddr) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    pub fn start(&self) -> VirtAddr {
        self.start
    }

    pub fn end(&self) -> VirtAddr {
        self.end
    }

    pub fn len(&self) -> usize {
        self.end.0 - self.start.0
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn contains(&self, addr: VirtAddr) -> bool {
        self.start <= addr && addr < self.end
    }

    pub fn overlaps(&self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn page_count(&self) -> usize {
        if self.is_empty() {
            return 0;
        }
        let start_page = self.start.0 / PAGE_SIZE;
        let end_page = self.end.0.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
        end_page.saturating_sub(start_page)
    }

    pub fn align_down(&self) -> Self {
        Self::new(
            VirtAddr(self.start.0 & !(PAGE_SIZE - 1)),
            VirtAddr(self.end.0 & !(PAGE_SIZE - 1)),
        )
    }

    pub fn align_up(&self) -> Self {
        Self::new(
            VirtAddr(align_up_page(self.start.0)),
            VirtAddr(align_up_page(self.end.0)),
        )
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PhysRegion {
    start: PhysAddr,
    end: PhysAddr,
}

impl PhysRegion {
    pub fn new(start: PhysAddr, end: PhysAddr) -> Self {
        assert!(start <= end, "phys region start must not exceed end");
        Self { start, end }
    }

    pub fn checked_new(start: PhysAddr, end: PhysAddr) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    pub fn start(&self) -> PhysAddr {
        self.start
    }

    pub fn end(&self) -> PhysAddr {
        self.end
    }

    pub fn len(&self) -> usize {
        self.end.0 - self.start.0
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn contains(&self, addr: PhysAddr) -> bool {
        self.start <= addr && addr < self.end
    }

    pub fn overlaps(&self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn page_count(&self) -> usize {
        if self.is_empty() {
            return 0;
        }
        let start_page = self.start.0 / PAGE_SIZE;
        let end_page = self.end.0.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
        end_page.saturating_sub(start_page)
    }

    pub fn align_down(&self) -> Self {
        Self::new(
            PhysAddr(self.start.0 & !(PAGE_SIZE - 1)),
            PhysAddr(self.end.0 & !(PAGE_SIZE - 1)),
        )
    }

    pub fn align_up(&self) -> Self {
        Self::new(
            PhysAddr(align_up_page(self.start.0)),
            PhysAddr(align_up_page(self.end.0)),
        )
    }
}

fn align_up_page(addr: usize) -> usize {
    addr.checked_add(PAGE_SIZE - 1)
        .map(|addr| addr & !(PAGE_SIZE - 1))
        .unwrap_or(usize::MAX & !(PAGE_SIZE - 1))
}
