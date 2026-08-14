//! 内核地址空间和内核动态映射。
//!
//! 启动时本模块建立内核代码、只读数据、数据段、BSS、物理内存 direct map、
//! MMIO 和 trampoline 的映射；运行时还负责内核栈与内核程序页的 framed 映射。
//!
//! # TLB
//!
//! PTE 修改通过 `KernelMapper` 进入页表实现。新增直接页表操作时必须保持架构层
//! 的 TLB 刷新契约。

#[cfg(not(target_arch = "riscv64"))]
use super::frame_allocator::for_each_usable_frame_region;
use super::kernel_mapper::KernelMapper;
use super::{
    frame_alloc, FrameTracker, MapPermission, MemoryError, PageTable, PhysAddr, PhysPageNum,
    VPNRange, VirtAddr, VirtPageNum,
};
use crate::config::*;
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
    #[cfg(not(target_arch = "riscv64"))]
    fn ekernel();
    fn strampoline();
}

/// Return permissions for a kernel identity mapping.
///
/// RISC-V global TLB entries ignore SATP.ASID.  A low identity mapping whose
/// virtual range overlaps user space therefore cannot be global: a user page
/// table may legally map the same VA to a different physical page.  Keep such
/// mappings in the reserved kernel ASID 0 while retaining `G` for disjoint
/// kernel-only ranges.  Other architectures keep their established policy.
#[cfg(not(target_arch = "riscv64"))]
fn kernel_identity_permissions(
    _start: usize,
    _end: usize,
    permissions: MapPermission,
) -> MapPermission {
    let _ = (_start, _end);
    permissions | MapPermission::G
}

#[cfg(target_arch = "riscv64")]
fn riscv_physmap_addr(address: usize) -> usize {
    assert!(
        address <= MEMORY_HIGH_SIZE,
        "physical address {:#x} exceeds the 64 GiB RISC-V physmap",
        address
    );
    MEMORY_HIGH_BASE + address
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

#[cfg(target_arch = "riscv64")]
pub const fn kernel_program_base() -> usize {
    crate::config::KERNEL_PROGRAM_BASE
}

#[cfg(not(target_arch = "riscv64"))]
pub const fn kernel_program_base() -> usize {
    crate::config::KERNEL_PROGRAM_BASE
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
    kind: KernelMappingKind,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum KernelMappingKind {
    KernelStack,
    Program,
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

    fn highest_program_end(&self) -> Option<VirtAddr> {
        self.mappings
            .iter()
            .filter(|mapping| mapping.kind == KernelMappingKind::Program)
            .map(KernelMapping::end_va)
            .max()
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
    fn new(
        vpn_range: VPNRange,
        frames: BTreeMap<VirtPageNum, Arc<FrameTracker>>,
        kind: KernelMappingKind,
    ) -> Self {
        Self {
            vpn_range,
            frames,
            kind,
        }
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
        let stext = stext as *const () as usize;
        let etext = etext as *const () as usize;
        let srodata = srodata as *const () as usize;
        let erodata = erodata as *const () as usize;
        let sdata = sdata as *const () as usize;
        let edata = edata as *const () as usize;
        let sbss_with_stack = sbss_with_stack as *const () as usize;
        let ebss = ebss as *const () as usize;
        #[cfg(not(target_arch = "riscv64"))]
        let ekernel = ekernel as *const () as usize;
        boot_trace!(".text [{:#x}, {:#x})", stext, etext);
        boot_trace!(".rodata [{:#x}, {:#x})", srodata, erodata);
        boot_trace!(".data [{:#x}, {:#x})", sdata, edata);
        boot_trace!(".bss [{:#x}, {:#x})", sbss_with_stack, ebss);
        macro_rules! kernel_map {
            ($begin:expr,$end:expr,$permission:expr) => {
                KernelMapper::new(&mut kernel_space.page_table)
                    .map_range(
                        ($begin as usize).into(),
                        crate::mm::PhysAddr::from(crate::hal::boot::kernel_linked_to_phys(
                            $begin as usize,
                        ))
                        .floor(),
                        ($end as usize).into(),
                        $permission,
                    )
                    .unwrap();
            };
            ($name:literal,$begin:expr,$end:expr,$permission:expr) => {
                boot_trace!("mapping {}", $name);
                kernel_map!($begin, $end, $permission);
            };
        }
        #[cfg(not(target_arch = "riscv64"))]
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
                boot_trace!("mapping {}", $name);
                kernel_identical_map!($begin, $end, $permission);
            };
        }
        kernel_map!(
            ".text section",
            stext,
            etext,
            MapPermission::R | MapPermission::X | MapPermission::G
        );
        kernel_map!(
            ".rodata section",
            srodata,
            erodata,
            MapPermission::R | MapPermission::G
        );
        kernel_map!(
            ".data section",
            sdata,
            edata,
            MapPermission::R | MapPermission::W | MapPermission::G
        );
        kernel_map!(
            ".bss section",
            sbss_with_stack,
            ebss,
            MapPermission::R | MapPermission::W | MapPermission::G
        );
        #[cfg(target_arch = "riscv64")]
        for &(start, end) in crate::hal::firmware::memory_regions() {
            boot_trace!(
                "mapping physical memory [{:#x}, {:#x}) at [{:#x}, {:#x})",
                start,
                end,
                riscv_physmap_addr(start),
                riscv_physmap_addr(end)
            );
            KernelMapper::new(&mut kernel_space.page_table)
                .map_unmapped_range(
                    VirtAddr::from(riscv_physmap_addr(start)),
                    PhysAddr::from(start).floor(),
                    VirtAddr::from(riscv_physmap_addr(end)),
                    MapPermission::R | MapPermission::W | MapPermission::G,
                )
                .unwrap();
        }
        #[cfg(not(target_arch = "riscv64"))]
        for_each_usable_frame_region(|start, end| {
            let start = start.start_addr().0;
            let end = end.start_addr().0;
            kernel_identical_map!(
                "physical memory region",
                start,
                end,
                kernel_identity_permissions(start, end, MapPermission::R | MapPermission::W)
            );
        });

        boot_trace!("mapping memory-mapped registers");
        for &(base, end) in crate::hal::firmware::early_mmio_ranges() {
            #[cfg(target_arch = "riscv64")]
            KernelMapper::new(&mut kernel_space.page_table)
                .map_unmapped_range(
                    VirtAddr::from(riscv_physmap_addr(base)),
                    PhysAddr::from(base).floor(),
                    VirtAddr::from(riscv_physmap_addr(end)),
                    MapPermission::R | MapPermission::W | MapPermission::G,
                )
                .unwrap();
            #[cfg(not(target_arch = "riscv64"))]
            KernelMapper::new(&mut kernel_space.page_table)
                .map_unmapped_identical_range(
                    VirtAddr::from(base),
                    VirtAddr::from(end),
                    kernel_identity_permissions(base, end, MapPermission::R | MapPermission::W),
                )
                .unwrap();
        }

        // Map firmware reserved regions (DTB, initrd) as read-only.
        // These pages are not in the frame allocator but must remain
        // accessible for post-heap firmware description parsing.
        #[cfg(not(target_arch = "riscv64"))]
        for &(base, end) in crate::hal::firmware::firmware_reserved_regions() {
            // QEMU can place the DTB entirely inside the kernel BSS. That
            // range is already mapped above with kernel write permissions.
            let kernel_start = crate::hal::boot::kernel_linked_to_phys(stext);
            let kernel_end = crate::hal::boot::kernel_linked_to_phys(ekernel);
            if kernel_start <= base && end <= kernel_end {
                continue;
            }
            kernel_identical_map!(
                base,
                end,
                kernel_identity_permissions(base, end, MapPermission::R)
            );
        }
        #[cfg(target_arch = "riscv64")]
        {
            // User roots copy only top-level entries.  Pre-create the roots of
            // every dynamic shared arena before the first process page table
            // exists, so later stack/program PTE updates remain visible.
            kernel_space
                .page_table
                .prepare_shared_kernel_range(
                    VirtAddr::from(KERNEL_PROGRAM_BASE),
                    VirtAddr::from(KERNEL_PROGRAM_END),
                )
                .unwrap();
            kernel_space
                .page_table
                .prepare_shared_kernel_range(
                    VirtAddr::from(KERNEL_STACK_BOTTOM),
                    VirtAddr::from(KERNEL_STACK_TOP),
                )
                .unwrap();
        }
        kernel_space
    }

    /// Mention that trampoline is not collected by areas.
    fn map_trampoline(&mut self) {
        KernelMapper::new(&mut self.page_table)
            .map_page(
                VirtAddr::from(TRAMPOLINE).into(),
                PhysAddr::from(crate::hal::boot::kernel_linked_to_phys(
                    strampoline as *const () as usize,
                ))
                .into(),
                MapPermission::R | MapPermission::X | MapPermission::G,
            )
            .unwrap();
    }

    pub fn token(&self) -> usize {
        self.page_table.token()
    }

    pub fn activate(&self) {
        self.page_table.activate()
    }

    pub fn insert_kernel_stack_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) {
        self.try_insert_framed_area(start_va, end_va, permission, KernelMappingKind::KernelStack)
            .unwrap();
    }

    fn try_insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        kind: KernelMappingKind,
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
            if let Err(err) = KernelMapper::new(&mut self.page_table).map_page(vpn, ppn, permission)
            {
                self.rollback_mapped_pages(&mapped_vpns);
                return Err(err);
            }
            mapped_vpns.push(vpn);
            frames.insert(vpn, frame);
        }
        self.kernel_mappings.insert(KernelMapping::new(
            VPNRange::new(start_vpn, end_vpn),
            frames,
            kind,
        ))
    }

    pub fn insert_program_area(
        &mut self,
        start_va: VirtAddr,
        permission: MapPermission,
        frames: Vec<Arc<FrameTracker>>,
    ) -> Result<(), MemoryError> {
        self.try_insert_program_area(start_va, permission, frames)
    }

    fn try_insert_program_area(
        &mut self,
        start_va: VirtAddr,
        permission: MapPermission,
        frames: Vec<Arc<FrameTracker>>,
    ) -> Result<(), MemoryError> {
        if frames.is_empty() {
            return Ok(());
        }
        let start_vpn = start_va.floor();
        let end_vpn = VirtPageNum::from(
            start_vpn
                .0
                .checked_add(frames.len())
                .ok_or(MemoryError::BadAddress)?,
        );
        // 程序载荷只会在此临时映射到完成 ELF 解析。LoongArch 上若允许该区间越过
        // KERNEL_PROGRAM_END，就可能命中与高地址内核栈相同的低 39 位 PGDH 索引。
        // 普通区间重叠检查无法发现这种架构别名，因此必须在安装任何 PTE 前拒绝该
        // 范围。上方 checked_add 还可防止 end_vpn 回绕后绕过边界比较。
        let arena_start = VirtAddr::from(kernel_program_base()).floor();
        let arena_end = VirtAddr::from(KERNEL_PROGRAM_END).floor();
        if start_vpn < arena_start || end_vpn > arena_end {
            return Err(MemoryError::BadAddress);
        }
        if self.kernel_mappings.has_overlap(start_vpn, end_vpn) {
            return Err(MemoryError::AlreadyMapped);
        }
        self.kernel_mappings.try_reserve(1)?;

        #[cfg(target_arch = "riscv64")]
        let permission = permission | MapPermission::G;
        let mut mapped_vpns = Vec::new();
        mapped_vpns
            .try_reserve(frames.len())
            .map_err(|_| MemoryError::OutOfMemory)?;
        let mut frame_map = BTreeMap::new();
        for (idx, frame) in frames.into_iter().enumerate() {
            let vpn = VirtPageNum::from(start_vpn.0 + idx);
            let ppn = frame.ppn;
            if let Err(err) = KernelMapper::new(&mut self.page_table).map_page(vpn, ppn, permission)
            {
                self.rollback_mapped_pages(&mapped_vpns);
                return Err(err);
            }
            mapped_vpns.push(vpn);
            frame_map.insert(vpn, frame);
        }
        self.kernel_mappings.insert(KernelMapping::new(
            VPNRange::new(start_vpn, end_vpn),
            frame_map,
            KernelMappingKind::Program,
        ))
    }

    /// 从共享内核页表摘下映射并清除 PTE，但继续持有映射内的全部 frame。
    ///
    /// 返回值绝不能在跨核 shootdown 完成前析构。该函数本身只在下方统一的
    /// `remove_kernel_mapping_synchronized()` 中使用，避免普通调用方绕过协议。
    fn detach_area_with_start_vpn(
        &mut self,
        start_vpn: VirtPageNum,
    ) -> Result<KernelMapping, MemoryError> {
        if let Some(mapping) = self.kernel_mappings.remove_with_start(start_vpn) {
            for vpn in mapping.vpn_range {
                KernelMapper::new(&mut self.page_table)
                    .unmap_page_no_flush(vpn)
                    .expect("tracked kernel mapping lost its PTE before detach");
            }
            Ok(mapping)
        } else {
            Err(MemoryError::AreaNotFound)
        }
    }

    /// 返回最高处地址
    pub fn highest_addr(&self) -> VirtAddr {
        self.kernel_mappings
            .highest_program_end()
            .unwrap_or_else(|| VirtAddr::from(kernel_program_base()))
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
        #[cfg(target_arch = "riscv64")]
        let vpn = VirtAddr::from(PhysAddr::from(ppn).direct_map_addr()).floor();
        #[cfg(not(target_arch = "riscv64"))]
        let vpn = (ppn.0).into();
        self.page_table.is_dirty(vpn)
    }

    fn rollback_mapped_pages(&mut self, mapped_vpns: &[VirtPageNum]) {
        for &vpn in mapped_vpns {
            let _ = KernelMapper::new(&mut self.page_table).unmap_page_if_mapped(vpn);
        }
    }
}

/// 安全撤销一段所有 CPU 共享的动态内核映射。
///
/// 资源释放顺序是本接口的核心契约：PTE 在 `KERNEL_SPACE` 锁内清除；锁释放后
/// 才等待本地与远端 TLB 同步；保存 frame 强引用的 `retired` 最后才析构。
/// 因此 shootdown 失败会 fail-stop，而不会把仍可能被旧 TLB 命中的物理页交回
/// frame allocator。
pub(crate) fn remove_kernel_mapping_synchronized(
    start_vpn: VirtPageNum,
) -> Result<(), MemoryError> {
    let retired = {
        let mut kernel_space = KERNEL_SPACE.lock();
        kernel_space.detach_area_with_start_vpn(start_vpn)?
    };

    crate::smp::synchronize_kernel_mapping_all().unwrap_or_else(|error| {
        panic!(
            "failed to retire kernel mapping at {:?}: {:?}",
            start_vpn, error
        )
    });
    drop(retired);
    Ok(())
}

#[allow(unused)]
pub fn remap_test() {
    let kernel_space = KERNEL_SPACE.lock();
    let mid_text: VirtAddr =
        ((stext as *const () as usize + etext as *const () as usize) / 2).into();
    let mid_rodata: VirtAddr =
        ((srodata as *const () as usize + erodata as *const () as usize) / 2).into();
    let mid_data: VirtAddr =
        ((sdata as *const () as usize + edata as *const () as usize) / 2).into();
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
