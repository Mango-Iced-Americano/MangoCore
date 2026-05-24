use super::mapper::translate_page;
use super::page_table::{FaultAccess, PageTable, UserAccess};
use super::user_mapper::UserMapper;
use super::vma::*;
use super::vma_set::VmaSet;
use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum, KERNEL_SPACE};
use crate::config::*;
use crate::hal::TrapContext;
use crate::hal::TICKS_PER_SEC;
use crate::should_map_trampoline;
use crate::syscall::errno::*;
use crate::task::{
    current_task, trap_cx_bottom_from_slot, ustack_bottom_from_slot, AuxvEntry, AuxvType, ELFInfo,
};
use crate::fs::vfs::IndexNode;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use log::{debug, error, trace, warn};

extern "C" {
    fn strampoline();
    fn ssignaltrampoline();
}

#[allow(unused)]
#[derive(Debug)]
pub enum MemoryError {
    BadAddress,
    AreaNotFound,
    AlreadyMapped,
    NotMapped,
    NoPermission,
    NotInMemory,
    NotCompressed,
    NotSwappedOut,
    AlreadyAllocated,
    SharedPage,
    ZramIsFull,
    SwapIsFull,
    BeyondEOF,
    OutOfMemory,
    BackingStoreFailure,
}

/// The user memory address space.
pub struct AddressSpace<T: PageTable> {
    /// 页表实现
    pub(super) page_table: T,
    /// 用户 VMA 集合，负责用户区间查找、插入、拆分和空洞管理。
    pub(super) vmas: VmaSet,
    /// 进程堆起始位置，由 ELF 加载阶段初始化。
    pub(super) heap_bottom: usize,
    /// 当前 program break。该状态属于地址空间，CLONE_VM 线程自然共享。
    pub(super) heap_pt: usize,
}

impl<T: PageTable> AddressSpace<T> {
    /// Create a new struct with no information at all.
    pub fn new_bare() -> Self {
        Self {
            page_table: T::new(),
            vmas: VmaSet::with_capacity(16),
            heap_bottom: 0,
            heap_pt: 0,
        }
    }
    /// Getter to the token of current memory space, or "this" page table.
    pub fn token(&self) -> usize {
        self.page_table.token()
    }
    /// Insert an anonymous segment containing the space between `start_va.floor()` to `end_va.ceil()`.
    /// The space is allocated and added to the current address space.
    /// # Prerequisite
    /// Assuming no conflicts. In other words, the space is NOT checked for space validity or overlap.
    /// It is merely mapped, pushed into the current address space.
    /// Since CoW is implemented, the space is NOT allocated until a page fault is triggered.
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) {
        let mut area = Vma::new(start_va, end_va, permission, None, 0);
        area.flags = MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS;
        self.push(area, None).unwrap();
    }
    pub fn remove_area_with_start_vpn(
        &mut self,
        start_vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        self.vmas
            .remove_area_with_start(&mut self.page_table, start_vpn)
    }
    /// Push a not-yet-mapped VMA into current address space and copy the data into it if any.
    fn push(
        &mut self,
        mut vma: Vma,
        data: Option<&[u8]>,
    ) -> Result<(), (MemoryError, VirtPageNum)> {
        let start_vpn = vma.inner.vpn_range.get_start();
        self.vmas
            .try_reserve(1)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        match data {
            Some(data) => {
                let mut start = 0;
                let len = data.len();
                for vpn in vma.inner.vpn_range {
                    let ppn = vma.map_one(&mut self.page_table, vpn)?;
                    let end = start + PAGE_SIZE;
                    let src = &data[start..len.min(end)];
                    ppn.get_bytes_array()[..src.len()].copy_from_slice(src);
                    start = end;
                }
            }
            None => {
                for vpn in vma.inner.vpn_range {
                    vma.map_one(&mut self.page_table, vpn)?;
                }
            }
        }
        self.vmas
            .push(vma)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        Ok(())
    }
    /// other parts will be zeroed
    fn push_with_offset(
        &mut self,
        mut vma: Vma,
        offset: usize,
        data: &[u8],
    ) -> Result<(), (MemoryError, VirtPageNum)> {
        let start_vpn = vma.inner.vpn_range.get_start();
        self.vmas
            .try_reserve(1)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        let len = data.len();
        let mut vpn_iter = vma.inner.vpn_range.into_iter();
        if let Some(vpn) = vpn_iter.next() {
            // special treatment for first page
            let first_ppn = vma.map_one(&mut self.page_table, vpn)?;
            let first_dst = first_ppn.get_bytes_array();
            first_dst[..offset].fill(0);
            let first_src = &data[..len.min(PAGE_SIZE - offset)];
            first_dst[offset..offset + first_src.len()].copy_from_slice(first_src);

            let mut start = PAGE_SIZE - offset;
            for vpn in vpn_iter {
                let ppn = vma.map_one(&mut self.page_table, vpn)?;
                let dst = ppn.get_bytes_array();
                let end = start + PAGE_SIZE;
                if start < len {
                    if len >= end {
                        let src = &data[start..end];
                        dst[..src.len()].copy_from_slice(src);
                    } else {
                        let src = &data[start..len];
                        dst[..src.len()].copy_from_slice(src);
                        dst[src.len()..].fill(0);
                    }
                } else {
                    dst.fill(0);
                }
                start = end;
            }
        }
        self.vmas
            .push(vma)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        Ok(())
    }

    /// Push the map area into the memory set without copying or allocation.
    pub fn push_no_alloc(&mut self, vma: Vma) -> Result<(), ()> {
        self.vmas.try_reserve(1).map_err(|_| ())?;
        let mut mapper = UserMapper::new(&mut self.page_table);
        for vpn in vma.inner.vpn_range {
            let frame = vma.inner.get_in_memory(&vpn).unwrap();
            if !mapper.is_mapped(vpn) {
                //if not mapped
                mapper
                    .map_user_page(vpn, frame.ppn.clone(), vma.map_perm)
                    .map_err(|_| ())?;
            } else {
                return Err(());
            }
        }
        self.vmas.push(vma).map_err(|_| ())?;
        Ok(())
    }
    pub fn contains_valid_buffer(&self, buf: usize, size: usize, perm: MapPermission) -> bool {
        let Some(end) = buf.checked_add(size) else {
            return false;
        };
        let start_vpn = VirtAddr::from(buf).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        self.vmas
            .iter()
            .find(|area| {
                // If there is such a page in user space, and the addr is in the vpn range
                area.map_perm.contains(perm | MapPermission::U)
                    && area.get_start::<T>() <= start_vpn
                    && end_vpn <= area.get_end::<T>()
            })
            .is_some()
    }

    pub fn validate_msync_range(
        &self,
        addr: usize,
        len: usize,
        invalidate: bool,
    ) -> Result<(), isize> {
        if len == 0 {
            return Ok(());
        }
        let rounded_len = len
            .checked_add(PAGE_SIZE - 1)
            .map(|len| len & !(PAGE_SIZE - 1))
            .ok_or(ENOMEM)?;
        let end = addr.checked_add(rounded_len).ok_or(ENOMEM)?;
        if end > USER_VA_END {
            return Err(ENOMEM);
        }
        if self.heap_bottom != 0 {
            let heap_limit = self.heap_bottom.saturating_add(USER_HEAP_SIZE);
            if addr < heap_limit && end > self.heap_pt && end > self.heap_bottom {
                return Err(ENOMEM);
            }
        }
        let start_vpn = VirtAddr::from(addr).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        if !self.vmas.covers_user_range(start_vpn, end_vpn) {
            return Err(ENOMEM);
        }
        if invalidate
            && self.vmas.iter().any(|area| {
                area.vm_is_user()
                    && area.vm_overlaps(start_vpn, end_vpn)
                    && area.flags.contains(MapFlags::MAP_LOCKED)
            })
        {
            return Err(EBUSY);
        }
        Ok(())
    }

    pub fn proc_maps_content(&self) -> String {
        let mut s = String::with_capacity(self.vmas.len() * 80);
        for vma in self.vmas.iter().filter(|vma| vma.vm_is_user()) {
            let start = vma.vm_start().0 * PAGE_SIZE;
            let end = vma.vm_end().0 * PAGE_SIZE;
            let perm = vma.vm_perm();
            let mapping = if vma.vm_mapping() == VmAreaMapping::Shared {
                's'
            } else {
                'p'
            };
            let _ = writeln!(
                s,
                "{:016x}-{:016x} {}{}{}{} {:08x} 00:00 0",
                start,
                end,
                if perm.contains(MapPermission::R) { 'r' } else { '-' },
                if perm.contains(MapPermission::W) { 'w' } else { '-' },
                if perm.contains(MapPermission::X) { 'x' } else { '-' },
                mapping,
                vma.map_file_offset,
            );
        }
        s
    }
    /// The REAL handler to page fault.
    /// Handles all types of page fault:(In regex:) "(Store|Load|Instruction)(Page)?Fault"
    /// Checks the permission to decide whether to copy.
    pub fn do_page_fault(
        &mut self,
        addr: VirtAddr,
        access: FaultAccess,
    ) -> Result<PhysAddr, MemoryError> {
        let vpn = addr.floor();
        let area_start = match self.vmas.find_user_vma_key(vpn) {
            Some(start) => Some(start),
            None => self.vmas.expand_growsdown_for_fault(vpn)?,
        };
        if area_start.is_some() {
            let ctx = super::page_fault::FaultContext::new(addr, access);
            let page_table = &mut self.page_table;
            let area = self.vmas.find_user_vma_mut(vpn).unwrap();
            super::page_fault::handle_page_fault(area, page_table, ctx)?;
            self.validate_user_fault_result(addr, access)
        } else {
            // In all segments, nothing matches the requirements. Throws.
            error!("[do_page_fault] addr: {:?}, result: bad addr", addr);
            Err(MemoryError::BadAddress)
        }
    }

    /// Fault in one user VA and return the verified physical address.
    ///
    /// This is the single uaccess-facing contract: after success, the VA must
    /// have a valid user PTE, required access permission, and a physical address
    /// inside real memory.
    pub fn fault_in_user_va(
        &mut self,
        addr: VirtAddr,
        access: FaultAccess,
    ) -> Result<PhysAddr, isize> {
        super::frame_reserve(3);
        self.do_page_fault(addr, access).map_err(memory_error_to_errno)
    }

    fn validate_user_fault_result(
        &self,
        addr: VirtAddr,
        access: FaultAccess,
    ) -> Result<PhysAddr, MemoryError> {
        let vpn = addr.floor();
        let pa = self
            .page_table
            .translate_va(addr)
            .ok_or(MemoryError::NotMapped)?;

        if pa.0 < MEMORY_START || pa.0 >= MEMORY_END {
            warn!(
                "[fault_in] translated user va {:#x} to invalid pa {:#x}",
                addr.0, pa.0
            );
            return Err(MemoryError::BadAddress);
        }

        let ok = match access {
            FaultAccess::Load => self
                .page_table
                .user_access_ok(vpn, UserAccess::Read)
                .unwrap_or(false),
            FaultAccess::Store => self
                .page_table
                .user_access_ok(vpn, UserAccess::Write)
                .unwrap_or(false),
            FaultAccess::Execute => {
                self.page_table.is_valid(vpn).unwrap_or(false)
                    && self.page_table.executable(vpn).unwrap_or(false)
            }
        };

        if !ok {
            warn!(
                "[fault_in] user va {:#x} failed post-fault permission check: {:?}",
                addr.0, access
            );
            return Err(MemoryError::NoPermission);
        }

        Ok(pa)
    }
    #[cfg(feature = "loongarch64")]
    #[cfg(feature = "oom_handler")]
    pub fn do_shallow_clean(&mut self) -> usize {
        let page_table = &mut self.page_table;
        self.vmas
            .iter_mut()
            .filter(|area| {
                let start_vpn = area.get_start::<T>();
                start_vpn.0 >= (USR_MMAP_BASE >> PAGE_SIZE_BITS)
                    && start_vpn.0 < (USR_MMAP_END >> PAGE_SIZE_BITS)
                    && area.map_file.is_none()
            })
            .map(|area| area.do_oom(page_table))
            .sum()
    }
    #[cfg(feature = "riscv")]
    #[cfg(feature = "oom_handler")]
    pub fn do_shallow_clean(&mut self) -> usize {
        let page_table = &mut self.page_table;
        self.vmas
            .iter_mut()
            .filter(|area| {
                let start_vpn = area.get_start::<T>();
                start_vpn.0 >= (MMAP_BASE >> PAGE_SIZE_BITS)
                    && start_vpn.0 < (TASK_SIZE >> PAGE_SIZE_BITS)
                    && area.map_file.is_none()
            })
            .map(|area| area.do_oom(page_table))
            .sum()
    }
    #[cfg(feature = "loongarch64")]
    #[cfg(feature = "oom_handler")]
    pub fn do_deep_clean(&mut self) -> usize {
        let page_table = &mut self.page_table;
        self.vmas
            .iter_mut()
            .filter(|area| {
                area.get_start::<T>().0 < (USER_VA_END >> PAGE_SIZE_BITS) && area.map_file.is_none()
            })
            .map(|area| {
                if area.get_start::<T>().0 < USR_MMAP_BASE >> PAGE_SIZE_BITS {
                    area.force_swap(page_table)
                } else {
                    area.do_oom(page_table)
                }
            })
            .sum()
    }
    #[cfg(feature = "riscv")]
    #[cfg(feature = "oom_handler")]
    pub fn do_deep_clean(&mut self) -> usize {
        let page_table = &mut self.page_table;
        self.vmas
            .iter_mut()
            .filter(|area| {
                area.get_start::<T>().0 < (TASK_SIZE >> PAGE_SIZE_BITS) && area.map_file.is_none()
            })
            .map(|area| {
                if area.get_start::<T>().0 < MMAP_BASE >> PAGE_SIZE_BITS {
                    area.force_swap(page_table)
                } else {
                    area.do_oom(page_table)
                }
            })
            .sum()
    }
    /// Mention that trampoline is not collected by areas.
    fn map_trampoline(&mut self) {
        UserMapper::new(&mut self.page_table)
            .map_privileged_user_page(
                VirtAddr::from(TRAMPOLINE).into(),
                PhysAddr::from(strampoline as usize).into(),
                MapPermission::R | MapPermission::X,
            )
            .unwrap();
    }

    /// Can be accessed in user mode.
    fn map_signaltrampoline(&mut self) {
        UserMapper::new(&mut self.page_table)
            .map_user_page(
                VirtAddr::from(SIGNAL_TRAMPOLINE).into(),
                PhysAddr::from(ssignaltrampoline as usize).into(),
                MapPermission::R | MapPermission::X | MapPermission::U,
            )
            .unwrap();
    }

    pub fn map_elf(&mut self, elf: &xmas_elf::ElfFile) -> Result<(usize, ELFInfo), isize> {
        let bias = match elf.header.pt2.type_().as_type() {
            // static
            xmas_elf::header::Type::Executable => 0,
            xmas_elf::header::Type::SharedObject => {
                match elf
                    .program_iter()
                    .filter(|ph| ph.get_type().unwrap() == xmas_elf::program::Type::Interp)
                    .count()
                {
                    // It's a loader!
                    0 => ELF_DYN_BASE,
                    // It's a dynamically linked ELF.
                    1 => 0,
                    // Emmm, It has multiple interpreters.
                    _ => return Err(EINVAL),
                }
            }
            _ => return Err(ENOEXEC),
        };

        let mut program_break: Option<usize> = None;
        let mut interp_entry: Option<usize> = None;
        let mut interp_base: Option<usize> = None;
        let mut load_addr: Option<usize> = None; // top va of ELF which points to ELF header

        for ph in elf.program_iter() {
            // Map only when the sections that is to be loaded.
            match ph.get_type().unwrap() {
                xmas_elf::program::Type::Load => {
                    // 防御性检查：拒绝超大段（> 1GB），避免 ELF 加载路径一次性映射过大。
                    if ph.mem_size() as usize > 1024 * 1024 * 1024 {
                        log::error!("[map_elf] Segment too large: {} bytes", ph.mem_size());
                        return Err(ENOMEM);
                    }
                    let start_va: VirtAddr = (ph.virtual_addr() as usize + bias).into();
                    let end_va: VirtAddr =
                        ((ph.virtual_addr() + ph.mem_size()) as usize + bias).into();
                    let start_va_page_offset = start_va.page_offset();

                    let map_perm = MapPermission::from_ph_flags(ph.flags());
                    if load_addr.is_none() {
                        load_addr = Some(start_va.into());
                    }
                    let mut vma =
                        match Vma::try_new(start_va, end_va, map_perm, None, 0) {
                            Ok(area) => area,
                            Err(e) => return Err(e),
                        };
                    vma.flags = MapFlags::MAP_PRIVATE;
                    // Virtual addr is 4K-aligned
                    if (start_va_page_offset & (PAGE_SIZE - 1)) == 0
                    // Physical addr is 4K-aligned
                        && (ph.offset() as usize & (PAGE_SIZE - 1)) == 0
                        && ph.file_size() != 0
                        && !map_perm.contains(MapPermission::W)
                    {
                        // Size in virtual addr is equal to size in physical addr
                        assert_eq!(
                            VirtAddr::from(ph.file_size() as usize).ceil().0,
                            vma.get_end::<T>().0 - vma.get_start::<T>().0
                        );

                        let kernel_start_vpn =
                            (VirtAddr::from(elf.input.as_ptr() as usize + (ph.offset() as usize)))
                                .floor();
                        vma
                            .map_from_kernel_area(&mut self.page_table, kernel_start_vpn)
                            .unwrap();
                        self.vmas.push(vma).map_err(|_| ENOMEM)?;
                    } else {
                        if let Err(_) = self.push_with_offset(
                            vma,
                            start_va_page_offset,
                            &elf.input
                                [ph.offset() as usize..(ph.offset() + ph.file_size()) as usize],
                        ) {
                            panic!("[map_elf] Target addr already mapped.")
                        };
                    }
                    program_break = Some(VirtAddr::from(end_va.ceil()).0);
                    trace!(
                        "[map_elf] start_va = 0x{:X}; end_va = 0x{:X}, offset = 0x{:X}",
                        start_va.0,
                        end_va.0,
                        start_va_page_offset
                    );
                }
                xmas_elf::program::Type::Interp => {
                    //assert!(elf.input[(ph.offset() + ph.file_size()) as usize] == b'\0');
                    let path = String::from_utf8_lossy(
                        &elf.input
                            [ph.offset() as usize..(ph.offset() + ph.file_size() - 1) as usize],
                    );
                    debug!("[map_elf] Found interpreter path: {}", path);
                    let interp_data = crate::task::load_elf_interp(&path)?;
                    let interp = xmas_elf::ElfFile::new(interp_data).map_err(|_| ENOEXEC)?;
                    let (_, interp_info) = self.map_elf(&interp)?;
                    interp_entry = Some(interp_info.entry);
                    interp_base = Some(interp_info.base);
                    KERNEL_SPACE
                        .lock()
                        .remove_area_with_start_vpn(
                            VirtAddr::from(interp_data.as_ptr() as usize).ceil(),
                        )
                        .unwrap();
                }
                _ => {}
            }
        }
        match (program_break, load_addr) {
            (Some(program_break), Some(load_addr)) => Ok((
                program_break,
                ELFInfo {
                    entry: elf.header.pt2.entry_point() as usize + bias,
                    interp_entry,
                    base: if let Some(interp_base) = interp_base {
                        interp_base
                    } else {
                        bias
                    },
                    phnum: elf.header.pt2.ph_count() as usize,
                    phent: elf.header.pt2.ph_entry_size() as usize,
                    phdr: load_addr + elf.header.pt2.ph_offset() as usize,
                },
            )),
            _ => Err(ENOEXEC),
        }
    }
    /// Include sections in elf and trampoline and TrapContext and user stack,
    /// also returns user_sp and entry point.
    pub fn from_elf(elf_data: &[u8]) -> Result<(Self, usize, ELFInfo), isize> {
        let mut address_space = Self::new_bare();
        // map trampoline
        if should_map_trampoline!() {
            address_space.map_trampoline();
        }
        // map signaltrampoline
        address_space.map_signaltrampoline();
        let elf = xmas_elf::ElfFile::new(elf_data).map_err(|_| {
            log::warn!("[from_elf] invalid ELF: {} bytes, first 16: {:02x?}",
                elf_data.len(), &elf_data[..16.min(elf_data.len())]);
            ENOEXEC
        })?;
        let (program_break, elf_info) = address_space.map_elf(&elf)?;
        address_space.heap_bottom = program_break;
        address_space.heap_pt = program_break;

        Ok((address_space, program_break, elf_info))
    }
    pub fn from_existing_user(user_space: &mut AddressSpace<T>) -> Result<AddressSpace<T>, isize> {
        let mut address_space = Self::new_bare();
        // map trampoline
        if should_map_trampoline!() {
            address_space.map_trampoline();
        }
        // map signaltrampoline
        address_space.map_signaltrampoline();
        address_space.heap_bottom = user_space.heap_bottom;
        address_space.heap_pt = user_space.heap_pt;
        // map data sections/user heap/mmap area/user stack
        if address_space
            .vmas
            .try_reserve(user_space.vmas.len())
            .is_err()
        {
            return Err(crate::syscall::errno::ENOMEM);
        }
        for area in user_space.vmas.iter().filter(|area| area.vm_is_user()) {
            let new_area = if area.wipe_on_fork {
                Vma::from_another(area)
            } else {
                let mut cloned = area.try_clone()?;
                cloned
                    .map_from_existing_page_table(
                        &mut address_space.page_table,
                        &mut user_space.page_table,
                    )
                    .map_err(|_| crate::syscall::errno::ENOMEM)?;
                cloned
            };
            address_space.vmas.push(new_area)?;
            debug!(
                "[fork] map shared area: {:?}",
                area.inner.vpn_range
            );
        }
        // copy trap context area
        let trap_cx_area = user_space
            .vmas
            .last_non_user()
            .ok_or(crate::syscall::errno::EINVAL)?;
        let area = Vma::from_another(trap_cx_area);
        let vpn = trap_cx_area.get_start::<T>();
        address_space
            .push(
                area,
                Some(
                    user_space
                        .translate(vpn)
                        .unwrap()
                        .start_addr()
                        .get_bytes_ref::<TrapContext>(),
                ),
            )
            .unwrap();

        debug!(
            "[fork] copy trap_cx area: {:?}",
            trap_cx_area.inner.vpn_range
        );
        Ok(address_space)
    }
    pub fn activate(&self) {
        self.page_table.activate()
    }
    /// Translate the `vpn` into its corresponding `Some(PageTableEntry)` in the current memory set if exists
    /// `None` is returned if nothing is found.
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        translate_page(&self.page_table, vpn)
    }
    pub fn recycle_data_pages(&mut self) {
        //*self = Self::new_bare();
        self.vmas.clear();
    }
    pub fn sbrk(&mut self, increment: isize) -> usize {
        super::mmap::do_sbrk(self, increment)
    }

    pub fn mmap(
        &mut self,
        start: usize,
        len: usize,
        prot: MapPermission,
        flags: MapFlags,
        offset: usize,
        map_file: Option<Arc<dyn IndexNode>>,
        may_write: bool,
    ) -> isize {
        super::mmap::do_mmap(self, start, len, prot, flags, offset, map_file, may_write)
    }

    pub fn munmap(&mut self, start: usize, len: usize) -> Result<(), isize> {
        super::mmap::do_munmap(self, start, len)
    }

    pub fn mprotect(&mut self, addr: usize, len: usize, prot: MapPermission) -> Result<(), isize> {
        super::mmap::do_mprotect(self, addr, len, prot)
    }

    pub fn madvise(&mut self, start: usize, len: usize, advice: usize) -> Result<(), isize> {
        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(start + len).ceil();
        self.vmas
            .advise_range(&mut self.page_table, start_vpn, end_vpn, advice)
    }

    pub fn mincore(&self, start: usize, len: usize, residency: &mut [u8]) -> Result<(), isize> {
        let end = start.checked_add(len).ok_or(ENOMEM)?;
        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        self.vmas
            .mincore_range(&self.page_table, start_vpn, end_vpn, residency)
    }

    fn user_lock_range(
        &self,
        start: usize,
        len: usize,
    ) -> Result<(VirtPageNum, VirtPageNum, usize), isize> {
        if len == 0 {
            return Ok((VirtPageNum(0), VirtPageNum(0), 0));
        }
        let end = start.checked_add(len).ok_or(ENOMEM)?;
        if end > USER_VA_END {
            return Err(ENOMEM);
        }
        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        if !self.vmas.covers_user_range(start_vpn, end_vpn) {
            return Err(ENOMEM);
        }
        let locked_len = (end_vpn.0 - start_vpn.0).saturating_mul(PAGE_SIZE);
        Ok((start_vpn, end_vpn, locked_len))
    }

    pub fn mlock(&mut self, start: usize, len: usize) -> Result<usize, isize> {
        let (start_vpn, end_vpn, locked_len) = self.user_lock_range(start, len)?;
        let mut vpn = start_vpn;
        while vpn < end_vpn {
            self.fault_in_user_va(VirtAddr::from(vpn), FaultAccess::Load)?;
            vpn.0 += 1;
        }
        Ok(locked_len)
    }

    pub fn mlock_onfault(&self, start: usize, len: usize) -> Result<usize, isize> {
        self.user_lock_range(start, len)
            .map(|(_, _, locked_len)| locked_len)
    }

    pub fn munlock(&self, start: usize, len: usize) -> Result<(), isize> {
        self.user_lock_range(start, len).map(|_| ())
    }

    pub fn user_mapped_bytes(&self) -> usize {
        self.vmas.user_mapped_bytes()
    }

    pub fn create_elf_tables(
        &self,
        mut user_sp: usize,
        argv_vec: &Vec<String>,
        envp_vec: &Vec<String>,
        elf_info: &ELFInfo,
    ) -> Result<usize, isize> {
        // go down to the stack page (important!) and align
        user_sp -= 2 * core::mem::size_of::<usize>();
        // because size of parameters is almost never more than PAGE_SIZE,
        // so I decide to use physical address directly for better performance
        let mut phys_user_sp = T::from_token(self.token())
            .translate_va(VirtAddr::from(user_sp))
            .unwrap()
            .0;
        let virt_phys_offset = user_sp - phys_user_sp;
        let phys_start = phys_user_sp;
        // unsafe code is efficient code! here we go!
        fn copy_to_user_string_unchecked(src: &str, dst: *mut u8) {
            let size = src.len();
            unsafe {
                core::slice::from_raw_parts_mut(dst, size)
                    .copy_from_slice(core::slice::from_raw_parts(src.as_ptr(), size));
                // adapt to C-style string
                *dst.add(size) = b'\0';
            }
        }

        // we don't care about the order of env...
        let mut envp_user = Vec::<*const u8>::new();
        if envp_user
            .try_reserve(envp_vec.len().saturating_add(1))
            .is_err()
        {
            return Err(crate::syscall::errno::ENOMEM);
        }
        for env in envp_vec.iter() {
            phys_user_sp -= env.len() + 1;
            envp_user.push((phys_user_sp + virt_phys_offset) as *const u8);
            copy_to_user_string_unchecked(env, phys_user_sp as *mut u8);
        }
        envp_user.push(core::ptr::null());

        // we don't care about the order of arg, too...
        let mut argv_user = Vec::<*const u8>::new();
        if argv_user
            .try_reserve(argv_vec.len().saturating_add(1))
            .is_err()
        {
            return Err(crate::syscall::errno::ENOMEM);
        }
        for arg in argv_vec.iter() {
            phys_user_sp -= arg.len() + 1;
            argv_user.push((phys_user_sp + virt_phys_offset) as *const u8);
            copy_to_user_string_unchecked(arg, phys_user_sp as *mut u8);
        }
        argv_user.push(core::ptr::null());
        // align downward to usize (64bit)
        phys_user_sp &= !0x7;

        // 16 random bytes
        phys_user_sp -= 2 * core::mem::size_of::<usize>();
        // should be virt addr!
        let random_bits_ptr = phys_user_sp + virt_phys_offset;
        unsafe {
            *(phys_user_sp as *mut usize) = 0xdeadbeefcafebabe;
            *(phys_user_sp as *mut usize).add(1) = 0xdeadbeefcafebabe;
        }
        // padding
        phys_user_sp -= core::mem::size_of::<usize>();
        unsafe {
            *(phys_user_sp as *mut usize) = 0x0000000000000000;
        }
        let auxv = [
            // AuxvEntry::new(AuxvType::SYSINFO_EHDR, vDSO_mapping);
            // AuxvEntry::new(AuxvType::L1I_CACHESIZE, 0);
            // AuxvEntry::new(AuxvType::L1I_CACHEGEOMETRY, 0);
            // AuxvEntry::new(AuxvType::L1D_CACHESIZE, 0);
            // AuxvEntry::new(AuxvType::L1D_CACHEGEOMETRY, 0);
            // AuxvEntry::new(AuxvType::L2_CACHESIZE, 0);
            // AuxvEntry::new(AuxvType::L2_CACHEGEOMETRY, 0);
            // `0x112d` means IMADZifenciC, aka gc
            AuxvEntry::new(AuxvType::HWCAP, 0x112d),
            AuxvEntry::new(AuxvType::PAGESZ, PAGE_SIZE),
            AuxvEntry::new(AuxvType::CLKTCK, TICKS_PER_SEC),
            AuxvEntry::new(AuxvType::PHDR, elf_info.phdr),
            AuxvEntry::new(AuxvType::PHENT, elf_info.phent),
            AuxvEntry::new(AuxvType::PHNUM, elf_info.phnum),
            AuxvEntry::new(AuxvType::BASE, elf_info.base),
            AuxvEntry::new(AuxvType::FLAGS, 0),
            AuxvEntry::new(AuxvType::ENTRY, elf_info.entry),
            AuxvEntry::new(AuxvType::UID, 0),
            AuxvEntry::new(AuxvType::EUID, 0),
            AuxvEntry::new(AuxvType::GID, 0),
            AuxvEntry::new(AuxvType::EGID, 0),
            AuxvEntry::new(AuxvType::SECURE, 0),
            AuxvEntry::new(AuxvType::RANDOM, random_bits_ptr as usize),
            AuxvEntry::new(
                AuxvType::EXECFN,
                argv_user.first().copied().unwrap() as usize,
            ),
            AuxvEntry::new(AuxvType::NULL, 0),
        ];
        phys_user_sp -= auxv.len() * core::mem::size_of::<AuxvEntry>();
        unsafe {
            core::slice::from_raw_parts_mut(phys_user_sp as *mut AuxvEntry, auxv.len())
                .copy_from_slice(auxv.as_slice());
        }
        phys_user_sp -= envp_user.len() * core::mem::size_of::<usize>();
        unsafe {
            core::slice::from_raw_parts_mut(phys_user_sp as *mut *const u8, envp_user.len())
                .copy_from_slice(envp_user.as_slice());
        }
        phys_user_sp -= argv_user.len() * core::mem::size_of::<usize>();
        unsafe {
            core::slice::from_raw_parts_mut(phys_user_sp as *mut *const u8, argv_user.len())
                .copy_from_slice(argv_user.as_slice());
        }
        phys_user_sp -= core::mem::size_of::<usize>();
        unsafe {
            *(phys_user_sp as *mut usize) = argv_vec.len();
        }

        user_sp = phys_user_sp + virt_phys_offset;

        // unlikely, if `start` and `end` are in different pages, we should panic
        assert_eq!(phys_start & !0xfff, phys_user_sp & !0xfff);

        // print user stack
        // let mut phys_addr = phys_user_sp & !0xf;
        // while phys_start >= phys_addr {
        //     trace!(
        //         "0x{:0>16X}:    {:0>16X}  {:0>16X}",
        //         phys_addr + virt_phys_offset,
        //         unsafe { *(phys_addr as *mut usize) },
        //         unsafe { *((phys_addr + core::mem::size_of::<usize>()) as *mut usize) }
        //     );
        //     phys_addr += 2 * core::mem::size_of::<usize>();
        // }
        Ok(user_sp)
    }
    pub fn alloc_user_res(&mut self, slot: usize, alloc_stack: bool) {
        if alloc_stack {
            let ustack_bottom = ustack_bottom_from_slot(slot);
            let ustack_top = ustack_bottom - USER_STACK_SIZE;
            trace!(
                "[alloc_user_res] slot {}, user stack start_va: {:X}, end_va: {:X}",
                slot,
                ustack_top,
                ustack_bottom
            );
            // alloc user stack
            self.insert_framed_area(
                ustack_top.into(),
                ustack_bottom.into(),
                MapPermission::R | MapPermission::W | MapPermission::U,
            );
            trace!("[alloc_user_res] done");
        } else {
            debug!(
                "[alloc_user_res] user stack is not allocated (stack is designated in sys_clone)"
            );
        }
        // alloc trap_cx
        let trap_cx_bottom = trap_cx_bottom_from_slot(slot);
        let trap_cx_top = trap_cx_bottom + PAGE_SIZE;
        self.insert_framed_area(
            trap_cx_bottom.into(),
            trap_cx_top.into(),
            MapPermission::R | MapPermission::W,
        );
        trace!(
            "[alloc_user_res] slot {}, trap context start_va: {:X}, end_va: {:X}",
            slot,
            trap_cx_bottom,
            trap_cx_top
        );
    }

    pub fn dealloc_user_res(&mut self, slot: usize) {
        // dealloc ustack manually
        let ustack_top_va: VirtAddr = (ustack_bottom_from_slot(slot) - USER_STACK_SIZE).into();
        if let Err(err) = self.remove_area_with_start_vpn(ustack_top_va.into()) {
            match err {
                MemoryError::AreaNotFound => {
                    warn!("[dealloc_user_res] slot {}, user stack is not allocated", slot)
                }
                MemoryError::NotMapped => {
                    warn!(
                        "[dealloc_user_res] slot {}, user stack is partially unmapped, is it caused by oom?",
                        slot
                    )
                }
                _ => {} //忽略非致命错误
            }
        }
        // 处理 trap_cx 回收
        let trap_cx_bottom_va: VirtAddr = trap_cx_bottom_from_slot(slot).into();

        // 改为这种写法，不再使用 unwrap()
        if let Err(err) = self.remove_area_with_start_vpn(trap_cx_bottom_va.into()) {
            match err {
                MemoryError::AreaNotFound => {
                    // 如果没找到该区域，可能是在之前的清理中整个 Area 都删了
                    trace!(
                        "[dealloc_user_res] trap_cx area not found for slot {}",
                        slot
                    );
                }
                MemoryError::NotMapped => {
                    // 如果页面已经不在页表里（被 OOM 换出），这在回收逻辑中是正常的
                    trace!(
                        "[dealloc_user_res] trap_cx already unmapped for slot {}",
                        slot
                    );
                }
                _ => {
                    // 其他错误也可以记录一下，但没必要 Panic 导致整个系统崩溃
                    debug!("[dealloc_user_res] trap_cx dealloc info: {:?}", err);
                }
            }
        }
        // dealloc trap_cx manually
        // let trap_cx_bottom_va: VirtAddr = trap_cx_bottom_from_slot(slot).into();
        // self.remove_area_with_start_vpn(trap_cx_bottom_va.into())
        //     .unwrap();
    }

}

fn memory_error_to_errno(err: MemoryError) -> isize {
    match err {
        MemoryError::BeyondEOF
        | MemoryError::NoPermission
        | MemoryError::BadAddress
        | MemoryError::NotMapped
        | MemoryError::BackingStoreFailure => EFAULT,
        MemoryError::OutOfMemory => ENOMEM,
        other => {
            warn!("[fault_in] unexpected memory error: {:?}", other);
            EFAULT
        }
    }
}

pub(super) fn check_page_fault(addr: VirtAddr, access: FaultAccess) -> Result<PhysAddr, isize> {
    // This is where we handle the page fault.
    let task = match current_task() {
        Some(task) => task,
        None => {
            log::warn!("[check_page_fault] No current task found, page fault in kernel?");
            return Err(EFAULT);
        }
    };
    let vm = task.process.vm();
    let result = vm.lock().fault_in_user_va(addr, access);
    result
}
