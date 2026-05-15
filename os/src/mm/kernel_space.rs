use super::kernel_mapper::KernelMapper;
use super::{
    frame_alloc, FrameTracker, MapPermission, MemoryError, PageTable, PhysAddr, PhysPageNum, VirtAddr,
    VirtPageNum, VPNRange,
};
use crate::config::*;
use crate::hal::MMIO;
use crate::should_map_trampoline;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::*;
use log::info;
use spin::Mutex;

extern "C" {
    fn stext();
    fn etext();
    fn srodata();
    fn erodata();
    fn sdata();
    fn edata();
    fn sbss_with_stack();
    fn ebss();
    fn ekernel();
    fn strampoline();
}

lazy_static! {
    /// 内核空间
    pub static ref KERNEL_SPACE: Arc<Mutex<KernelSpace<crate::mm::KernelPageTableImpl>>> =
        Arc::new(Mutex::new(KernelSpace::new()));
}

/// Return the root PPN of kernel space
pub fn kernel_token() -> usize {
    KERNEL_SPACE.lock().token()
}

pub struct KernelSpace<T: PageTable> {
    pub(super) page_table: T,
    kernel_mappings: KernelMappingSet,
}

struct KernelMappingSet {
    mappings: Vec<KernelMapping>,
}

struct KernelMapping {
    vpn_range: VPNRange,
    frames: BTreeMap<VirtPageNum, Arc<FrameTracker>>,
}

impl KernelMappingSet {
    fn new() -> Self {
        Self {
            mappings: Vec::new(),
        }
    }

    fn try_reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        self.mappings
            .try_reserve(additional)
            .map_err(|_| MemoryError::OutOfMemory)
    }

    fn last(&self) -> Option<&KernelMapping> {
        self.mappings.last()
    }

    fn has_overlap(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) -> bool {
        self.mappings
            .iter()
            .any(|mapping| mapping.overlaps(start_vpn, end_vpn))
    }

    fn insert(&mut self, mapping: KernelMapping) -> Result<(), MemoryError> {
        if mapping.is_empty() {
            return Ok(());
        }
        if self.has_overlap(mapping.start(), mapping.end()) {
            return Err(MemoryError::AlreadyMapped);
        }
        self.try_reserve(1)?;
        self.mappings.push(mapping);
        Ok(())
    }

    fn remove_with_start(&mut self, start_vpn: VirtPageNum) -> Option<KernelMapping> {
        self.mappings
            .iter()
            .position(|mapping| mapping.start() == start_vpn)
            .map(|idx| self.mappings.remove(idx))
    }

    fn mapped_frame(&self, vpn: VirtPageNum) -> Option<Arc<FrameTracker>> {
        self.mappings
            .iter()
            .rev()
            .find_map(|mapping| mapping.frames.get(&vpn).cloned())
    }
}

impl KernelMapping {
    fn new(vpn_range: VPNRange, frames: BTreeMap<VirtPageNum, Arc<FrameTracker>>) -> Self {
        Self { vpn_range, frames }
    }

    fn start(&self) -> VirtPageNum {
        self.vpn_range.get_start()
    }

    fn end(&self) -> VirtPageNum {
        self.vpn_range.get_end()
    }

    fn end_va(&self) -> VirtAddr {
        self.end().into()
    }

    fn is_empty(&self) -> bool {
        self.start() == self.end()
    }

    fn overlaps(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) -> bool {
        self.start() < end_vpn && start_vpn < self.end()
    }
}

impl<T: PageTable> KernelSpace<T> {
    pub fn new() -> Self {
        let mut kernel_space = Self {
            page_table: T::new_kern_space(),
            kernel_mappings: KernelMappingSet::new(),
        };
        // map trampoline
        if should_map_trampoline!() {
            kernel_space.map_trampoline();
        }
        // map kernel sections
        println!(".text [{:#x}, {:#x})", stext as usize, etext as usize);
        println!(".rodata [{:#x}, {:#x})", srodata as usize, erodata as usize);
        println!(".data [{:#x}, {:#x})", sdata as usize, edata as usize);
        println!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as usize, ebss as usize
        );
        macro_rules! kernel_identical_map {
            ($begin:expr,$end:expr,$permission:expr) => {
                KernelMapper::new(&mut kernel_space.page_table)
                    .map_identical_range(
                        ($begin as usize).into(),
                        ($end as usize).into(),
                        $permission,
                    )
                    .unwrap();
            };
            ($name:literal,$begin:expr,$end:expr,$permission:expr) => {
                println!("mapping {}", $name);
                kernel_identical_map!($begin, $end, $permission);
            };
        }
        kernel_identical_map!(
            ".text section",
            stext,
            etext,
            MapPermission::R | MapPermission::X
        );
        kernel_identical_map!(".rodata section", srodata, erodata, MapPermission::R); // read only section
        kernel_identical_map!(
            ".data section",
            sdata,
            edata,
            MapPermission::R | MapPermission::W
        );
        kernel_identical_map!(
            ".bss section",
            sbss_with_stack,
            ebss,
            MapPermission::R | MapPermission::W
        );
        kernel_identical_map!(
            "physical memory",
            ekernel,
            MEMORY_END,
            MapPermission::R | MapPermission::W
        );

        println!("mapping memory-mapped registers");
        for pair in MMIO {
            kernel_identical_map!(
                (*pair).0,
                ((*pair).0 + (*pair).1),
                MapPermission::R | MapPermission::W
            );
        }
        kernel_space
    }

    /// Mention that trampoline is not collected by areas.
    fn map_trampoline(&mut self) {
        KernelMapper::new(&mut self.page_table)
            .map_page(
                VirtAddr::from(TRAMPOLINE).into(),
                PhysAddr::from(strampoline as usize).into(),
                MapPermission::R | MapPermission::X,
            )
            .unwrap();
    }

    pub fn token(&self) -> usize {
        self.page_table.token()
    }

    pub fn activate(&self) {
        self.page_table.activate()
    }

    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) {
        self.try_insert_framed_area(start_va, end_va, permission)
            .unwrap();
    }

    fn try_insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) -> Result<(), MemoryError> {
        let start_vpn = start_va.floor();
        let end_vpn = end_va.ceil();
        if start_vpn >= end_vpn {
            return Err(MemoryError::BadAddress);
        }
        if self.kernel_mappings.has_overlap(start_vpn, end_vpn) {
            return Err(MemoryError::AlreadyMapped);
        }
        self.kernel_mappings.try_reserve(1)?;

        let mut mapped_vpns = Vec::new();
        mapped_vpns
            .try_reserve(end_vpn.0 - start_vpn.0)
            .map_err(|_| MemoryError::OutOfMemory)?;
        let mut frames = BTreeMap::new();
        for vpn in VPNRange::new(start_vpn, end_vpn) {
            let frame = frame_alloc().ok_or_else(|| {
                self.rollback_mapped_pages(&mapped_vpns);
                MemoryError::OutOfMemory
            })?;
            let ppn = frame.ppn;
            if let Err(err) =
                KernelMapper::new(&mut self.page_table).map_page(vpn, ppn, permission)
            {
                self.rollback_mapped_pages(&mapped_vpns);
                return Err(err);
            }
            mapped_vpns.push(vpn);
            frames.insert(vpn, frame);
        }
        self.kernel_mappings
            .insert(KernelMapping::new(VPNRange::new(start_vpn, end_vpn), frames))
    }

    pub fn insert_program_area(
        &mut self,
        start_va: VirtAddr,
        permission: MapPermission,
        frames: Vec<Arc<FrameTracker>>,
    ) -> Result<(), ()> {
        self.try_insert_program_area(start_va, permission, frames)
            .map_err(|_| ())
    }

    fn try_insert_program_area(
        &mut self,
        start_va: VirtAddr,
        permission: MapPermission,
        frames: Vec<Arc<FrameTracker>>,
    ) -> Result<(), MemoryError> {
        let start_vpn = start_va.floor();
        let end_vpn = VirtPageNum::from(start_vpn.0 + frames.len());
        if start_vpn == end_vpn {
            return Ok(());
        }
        if self.kernel_mappings.has_overlap(start_vpn, end_vpn) {
            return Err(MemoryError::AlreadyMapped);
        }
        self.kernel_mappings.try_reserve(1)?;

        let mut mapped_vpns = Vec::new();
        mapped_vpns
            .try_reserve(frames.len())
            .map_err(|_| MemoryError::OutOfMemory)?;
        let mut frame_map = BTreeMap::new();
        for (idx, frame) in frames.into_iter().enumerate() {
            let vpn = VirtPageNum::from(start_vpn.0 + idx);
            let ppn = frame.ppn;
            if let Err(err) =
                KernelMapper::new(&mut self.page_table).map_page(vpn, ppn, permission)
            {
                self.rollback_mapped_pages(&mapped_vpns);
                return Err(err);
            }
            mapped_vpns.push(vpn);
            frame_map.insert(vpn, frame);
        }
        self.kernel_mappings
            .insert(KernelMapping::new(VPNRange::new(start_vpn, end_vpn), frame_map))
    }

    pub fn remove_area_with_start_vpn(
        &mut self,
        start_vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        if let Some(mapping) = self.kernel_mappings.remove_with_start(start_vpn) {
            for vpn in mapping.vpn_range {
                KernelMapper::new(&mut self.page_table).unmap_page_if_mapped(vpn)?;
            }
            Ok(())
        } else {
            Err(MemoryError::AreaNotFound)
        }
    }

    /// 返回最高处地址
    pub fn highest_addr(&self) -> VirtAddr {
        self.kernel_mappings
            .last()
            .map(|mapping| mapping.end_va())
            .unwrap_or_else(|| VirtAddr::from(MMAP_BASE))
    }

    pub fn mapped_frame(&self, vpn: VirtPageNum) -> Option<Arc<FrameTracker>> {
        self.kernel_mappings.mapped_frame(vpn)
    }

    pub fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()> {
        KernelMapper::new(&mut self.page_table)
            .clear_dirty_bit(vpn)
            .map_err(|_| ())
    }

    pub fn is_dirty(&self, ppn: PhysPageNum) -> Option<bool> {
        self.page_table.is_dirty((ppn.0).into())
    }

    fn rollback_mapped_pages(&mut self, mapped_vpns: &[VirtPageNum]) {
        for &vpn in mapped_vpns {
            let _ = KernelMapper::new(&mut self.page_table).unmap_page_if_mapped(vpn);
        }
    }
}

#[allow(unused)]
pub fn remap_test() {
    let kernel_space = KERNEL_SPACE.lock();
    let mid_text: VirtAddr = ((stext as usize + etext as usize) / 2).into();
    let mid_rodata: VirtAddr = ((srodata as usize + erodata as usize) / 2).into();
    let mid_data: VirtAddr = ((sdata as usize + edata as usize) / 2).into();
    assert_eq!(
        kernel_space.page_table.writable(mid_text.floor()).unwrap(),
        false
    );
    assert_eq!(
        kernel_space
            .page_table
            .writable(mid_rodata.floor())
            .unwrap(),
        false,
    );
    assert_eq!(
        kernel_space
            .page_table
            .executable(mid_data.floor())
            .unwrap(),
        false,
    );
    info!("remap_test passed!");
}
