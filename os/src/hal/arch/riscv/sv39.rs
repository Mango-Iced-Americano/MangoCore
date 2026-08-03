//! RISC-V SV39 页表实现。
//!
//! 实现 `PageTable` trait，负责三级页表遍历、PTE 权限转换和页表激活；同时管理
//! MM-owned SATP ASID、epoch rollover 与 `sfence.vma` TLB 失效。

use crate::mm::{
    address::*, frame_alloc, FrameTracker, MapPermission, MemoryError, PageTable, UserAccess,
};
use alloc::{sync::Arc, vec::Vec};
use bitflags::*;
use core::arch::asm;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use riscv::register::satp;
use spin::Mutex;

/// 内核固定使用 ASID 0；用户地址空间只从 1 开始分配。
pub const KERN_ASID: u16 = 0;
const USER_ASID_BASE: u32 = 1;
const SATP_ASID_SHIFT: usize = 44;
const SATP_ASID_BITS: u32 = 16;
const SATP_ASID_MASK: usize = (1usize << SATP_ASID_BITS) - 1;
const ASID_CONTEXT_MASK: u64 = (1u64 << SATP_ASID_BITS) - 1;

/// 一个 epoch 内只递增编号；只有全 CPU TLB flush 完成后才允许从 1 复用。
struct AsidAllocator {
    initialized: bool,
    epoch: u64,
    /// 使用 u32，才能表达 16-bit ASID 空间耗尽后的 65536 游标。
    next: u32,
    max: u32,
}

static ASID_ALLOCATOR: Mutex<AsidAllocator> = Mutex::new(AsidAllocator {
    initialized: false,
    epoch: 1,
    next: USER_ASID_BASE,
    max: 0,
});
static ASID_ROLLOVER: AtomicBool = AtomicBool::new(false);
static ASID_ROLLOVERS: AtomicUsize = AtomicUsize::new(0);

/// 在 BSP 已启用 Sv39 后探测 SATP 实际实现的 ASID 位数。
fn probe_asid_width() -> usize {
    let original: usize;
    // Safety: 这里只读取当前 hart 的 SATP，不改变地址翻译。
    unsafe { asm!("csrr {value}, satp", value = out(reg) original, options(nostack)) };
    assert_eq!(
        original >> 60,
        8,
        "RISC-V ASID probe requires an active Sv39 page table"
    );

    let candidate = original | (SATP_ASID_MASK << SATP_ASID_SHIFT);
    let observed: usize;
    // Safety: 保持 MODE 与 PPN 不变，只用 WARL 语义探测 ASID 字段。恢复原值后
    // 全刷一次，清除探测编号可能留下的翻译；此时用户任务尚未启动。
    unsafe {
        asm!(
            "csrw satp, {candidate}",
            "csrr {observed}, satp",
            "csrw satp, {original}",
            "sfence.vma",
            candidate = in(reg) candidate,
            original = in(reg) original,
            observed = out(reg) observed,
            options(nostack)
        )
    };

    let implemented = ((observed >> SATP_ASID_SHIFT) & SATP_ASID_MASK) as u16;
    let width = implemented.count_ones() as usize;
    let contiguous = if width == SATP_ASID_BITS as usize {
        u16::MAX
    } else {
        ((1u32 << width) - 1) as u16
    };
    assert_eq!(
        implemented, contiguous,
        "RISC-V SATP exposes a non-contiguous ASID mask"
    );
    width
}

/// 初始化硬件 ASID 容量；返回可分配给用户 MM 的编号数量。
pub fn init_asid_allocator() -> usize {
    let width = probe_asid_width();
    let max = if width == 0 {
        0
    } else {
        ((1u32 << width) - 1).min(u16::MAX as u32)
    };
    let mut allocator = ASID_ALLOCATOR.lock();
    assert!(
        !allocator.initialized,
        "RISC-V ASID allocator initialized twice"
    );
    allocator.initialized = true;
    allocator.max = max;
    max as usize
}

/// 为一个 MM 取得当前 epoch 的 ASID context；换代中或耗尽时返回 `None`。
pub fn try_assign_asid(current: u64) -> Option<u64> {
    let mut allocator = ASID_ALLOCATOR.lock();
    assert!(
        allocator.initialized,
        "RISC-V ASID allocator used before init"
    );
    if allocator.max == 0 {
        // ASIDLEN=0 的实现继续使用旧的 ASID 0 + 每次 SATP 切换全刷路径。
        return Some(0);
    }
    if ASID_ROLLOVER.load(Ordering::Acquire) {
        return None;
    }

    let current_epoch = current >> SATP_ASID_BITS;
    let current_asid = (current & ASID_CONTEXT_MASK) as u32;
    if current_epoch == allocator.epoch
        && current_asid >= USER_ASID_BASE
        && current_asid <= allocator.max
    {
        return Some(current);
    }
    if allocator.next > allocator.max {
        return None;
    }

    let asid = allocator.next;
    allocator.next += 1;
    Some((allocator.epoch << SATP_ASID_BITS) | u64::from(asid))
}

/// 从软件 context 中提取写入 SATP 的硬件 ASID。
pub const fn hardware_asid(context: u64) -> u16 {
    (context & ASID_CONTEXT_MASK) as u16
}

/// ASID 耗尽后先把所有在线 CPU 拉回内核并完成全刷，再发布新 epoch。
pub fn rollover_asids() {
    {
        let allocator = ASID_ALLOCATOR.lock();
        if allocator.max == 0 || allocator.next <= allocator.max {
            return;
        }
    }

    if ASID_ROLLOVER
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // 等待者必须能处理 leader 发来的 TLB IPI，否则两个 CPU 会互等 ack。
        let irq_was_enabled = crate::hal::local_irq_save();
        crate::hal::local_irq_restore(true);
        while ASID_ROLLOVER.load(Ordering::Acquire) {
            spin_loop();
        }
        let _ = crate::hal::local_irq_save();
        crate::hal::local_irq_restore(irq_was_enabled);
        return;
    }

    let old_epoch = {
        let allocator = ASID_ALLOCATOR.lock();
        if allocator.next <= allocator.max {
            ASID_ROLLOVER.store(false, Ordering::Release);
            return;
        }
        allocator.epoch
    };

    // 软件 IPI 的 ack 同时证明目标 CPU 已离开旧用户 SATP；它再次返回用户态
    // 前会经过 activate_on()，因此新 epoch 发布后不会继续使用旧 context。
    if let Err(error) =
        crate::smp::synchronize_user_tlb(crate::smp::online_cpu_mask(), KERN_ASID, None, None)
    {
        panic!("RISC-V ASID rollover TLB flush failed: {:?}", error);
    }

    {
        let mut allocator = ASID_ALLOCATOR.lock();
        assert_eq!(
            allocator.epoch, old_epoch,
            "two RISC-V ASID rollover leaders"
        );
        let new_epoch = old_epoch
            .checked_add(1)
            .expect("RISC-V ASID epoch exhausted");
        assert!(
            new_epoch <= (u64::MAX >> SATP_ASID_BITS),
            "RISC-V ASID context encoding exhausted"
        );
        allocator.epoch = new_epoch;
        allocator.next = USER_ASID_BASE;
    }
    ASID_ROLLOVERS.fetch_add(1, Ordering::Relaxed);
    ASID_ROLLOVER.store(false, Ordering::Release);
}

pub fn asid_capacity() -> usize {
    ASID_ALLOCATOR.lock().max as usize
}

pub fn asid_rollover_count() -> usize {
    ASID_ROLLOVERS.load(Ordering::Acquire)
}

/// 在保留页表根和 Sv39 MODE 的同时，把 MM-owned ASID 编入 SATP。
pub const fn satp_with_asid(page_table_token: usize, asid: u16) -> usize {
    (page_table_token & !(SATP_ASID_MASK << SATP_ASID_SHIFT)) | ((asid as usize) << SATP_ASID_SHIFT)
}

/// 将 vpn 转换为字节地址后做单页 TLB 刷新
macro_rules! tlb_invalidate_vpn {
    ($vpn:expr) => {
        $crate::hal::arch::riscv::sv39::tlb_invalidate_addr(usize::from(
            $crate::mm::VirtAddr::from($vpn),
        ))
    };
}

#[inline(always)]
pub fn tlb_invalidate() {
    let start = crate::task::perf::perf_time_now();
    // Safety: `sfence.vma` serializes address-translation updates on the current
    // hart and does not access memory through Rust references.
    unsafe {
        asm!("sfence.vma");
    }
    let elapsed = crate::task::perf::perf_time_now().wrapping_sub(start);
    crate::task::perf::record_tlb_full_flush_cycles(elapsed);
    crate::task::perf::record_tlb_full();
}

/// 只刷指定虚拟地址对应的 TLB 条目，不影响其他条目
#[inline(always)]
pub fn tlb_invalidate_addr(vaddr: usize) {
    let start = crate::task::perf::perf_time_now();
    // Safety: the instruction only uses `vaddr` as the architectural fence
    // operand; it does not dereference the address.
    unsafe {
        asm!("sfence.vma {}, zero", in(reg) vaddr);
    }
    let elapsed = crate::task::perf::perf_time_now().wrapping_sub(start);
    crate::task::perf::record_tlb_page_flush_cycles(elapsed);
    crate::task::perf::record_tlb_page();
}

/// 只失效指定用户地址空间中的一个虚拟页，不触碰 global 映射。
#[inline(always)]
pub fn tlb_invalidate_addr_asid(vaddr: usize, asid: u16) {
    let start = crate::task::perf::perf_time_now();
    let asid = asid as usize;
    // Safety: VA 与 ASID 都只是 `sfence.vma` 的筛选操作数，不会被解引用。
    // 即使 ASIDLEN=0，也必须让 rs2 使用普通寄存器而不是 x0；用户 PTE 均非 global。
    unsafe {
        asm!(
            "sfence.vma {vaddr}, {asid}",
            vaddr = in(reg) vaddr,
            asid = in(reg) asid,
            options(nostack)
        );
    }
    let elapsed = crate::task::perf::perf_time_now().wrapping_sub(start);
    crate::task::perf::record_tlb_page_flush_cycles(elapsed);
    crate::task::perf::record_tlb_page();
}
bitflags! {
    /// Page Table Entry flags
    pub struct PTEFlags: u8 {
    /// Valid Bit
        const V = 1 << 0;
    /// Readable Bit
        const R = 1 << 1;
    /// Writable Bit
        const W = 1 << 2;
    /// Executable Bit
        const X = 1 << 3;
    /// User Space Bit, true if it can be accessed from user space.
        const U = 1 << 4;
        const G = 1 << 5;
        const A = 1 << 6;
    /// Dirty Bit, true if it is modified.
        const D = 1 << 7;
    }
}

/// Page Table Entry
#[derive(Copy, Clone)]
#[repr(C)]
pub struct Sv39PageTableEntry {
    pub bits: usize,
}

impl Sv39PageTableEntry {
    const PPN_MASK: usize = ((1usize << 44) - 1) << 10;
    pub fn new(ppn: PhysPageNum, flags: PTEFlags) -> Self {
        Sv39PageTableEntry {
            bits: ppn.0 << 10 | flags.bits as usize,
        }
    }
    pub fn empty() -> Self {
        Sv39PageTableEntry { bits: 0 }
    }
    pub fn ppn(&self) -> PhysPageNum {
        ((self.bits & Self::PPN_MASK) >> 10).into()
    }
    pub fn flags(&self) -> PTEFlags {
        PTEFlags::from_bits(self.bits as u8).unwrap()
    }
    pub fn is_valid(&self) -> bool {
        (self.flags() & PTEFlags::V) != PTEFlags::empty()
    }
    pub fn is_dirty(&self) -> bool {
        (self.flags() & PTEFlags::D) != PTEFlags::empty()
    }
    pub fn readable(&self) -> bool {
        (self.flags() & PTEFlags::R) != PTEFlags::empty()
    }
    pub fn writable(&self) -> bool {
        (self.flags() & PTEFlags::W) != PTEFlags::empty()
    }
    pub fn executable(&self) -> bool {
        (self.flags() & PTEFlags::X) != PTEFlags::empty()
    }
    pub fn clear_access(&mut self) {
        self.bits &= !(PTEFlags::A.bits() as usize);
    }
    pub fn clear_dirty(&mut self) {
        self.bits &= !(PTEFlags::D.bits() as usize);
    }
    pub fn revoke_read(&mut self) {
        self.bits &= !(PTEFlags::R.bits() as usize);
    }
    pub fn revoke_write(&mut self) {
        self.bits &= !(PTEFlags::W.bits() as usize);
    }
    pub fn revoke_execute(&mut self) {
        self.bits &= !(PTEFlags::X.bits() as usize);
    }
    pub fn set_permission(&mut self, flags: MapPermission) {
        self.bits = (self.bits & 0xffff_ffff_ffff_ffe1) | (flags.bits() as usize)
        // | ((PTEFlags::A.bits() | PTEFlags::D.bits()) as usize)
    }
    pub fn set_ppn(&mut self, ppn: PhysPageNum) {
        self.bits = (self.bits & !Self::PPN_MASK) | ((ppn.0 << 10) & Self::PPN_MASK)
    }
}

pub struct Sv39PageTable {
    root_ppn: PhysPageNum,
    frames: Vec<Arc<FrameTracker>>,
}

/// Assume that it won't encounter oom when creating/mapping.
impl Sv39PageTable {
    /// Find the page in the page table, creating the page on the way if not exists.
    /// Note: It does NOT create the terminal node. The caller must verify its validity and create according to his own needs.
    fn find_pte_create(
        &mut self,
        vpn: VirtPageNum,
    ) -> Result<&mut Sv39PageTableEntry, MemoryError> {
        let idxs: [usize; 3] = vpn.indexes();
        let mut ppn = self.root_ppn;
        for i in 0..3 {
            // Safety: 可变 self 独占本页表；PPN 只来自根或有效非叶子 PTE，
            // 新建的中间页也由当前对象的 frame 列表保持存活。
            let pte = &mut unsafe { ppn.get_pte_array_mut() }[idxs[i]];
            if i == 2 {
                // this condition is used to make sure the
                //returning predication is put before validity to quit before creating the terminal page entry.
                return Ok(pte);
            }
            if !pte.is_valid() {
                self.frames
                    .try_reserve(1)
                    .map_err(|_| MemoryError::OutOfMemory)?;
                let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
                // 非叶子页表项只设置 V，保持 R/W/X 为 0；A/D 只对叶子 PTE 有意义。
                *pte = Sv39PageTableEntry::new(frame.ppn, PTEFlags::V);
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        Err(MemoryError::BadAddress)
    }
    /// Find the page table entry denoted by vpn, returning Some(&_) if found or None if not.
    pub fn find_pte(&self, vpn: VirtPageNum) -> Option<&Sv39PageTableEntry> {
        let idxs: [usize; 3] = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&Sv39PageTableEntry> = None;
        for i in 0..3 {
            // Safety: 页表对象保持中间页存活；共享借用只建立只读 PTE 视图。
            let pte = &unsafe { ppn.get_pte_array::<Sv39PageTableEntry>() }[idxs[i]];
            if !pte.is_valid() {
                return None;
            }
            if i == 2 {
                result = Some(pte);
                break;
            }
            ppn = pte.ppn();
        }
        result
    }
    /// Find and return reference the page table entry denoted by `vpn`, `None` if not found.
    fn find_pte_refmut(&mut self, vpn: VirtPageNum) -> Option<&mut Sv39PageTableEntry> {
        let idxs: [usize; 3] = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut Sv39PageTableEntry> = None;
        for i in 0..3 {
            // Safety: 可变 self 将返回 PTE 的生命周期绑定到页表独占借用；
            // 遍历到的 PPN 只来自当前对象管理的有效页表链。
            let pte = &mut unsafe { ppn.get_pte_array_mut::<Sv39PageTableEntry>() }[idxs[i]];
            if !pte.is_valid() {
                return None;
            }
            if i == 2 {
                result = Some(pte);
                break;
            }
            ppn = pte.ppn();
        }
        result
    }
}
/// Assume that it won't encounter oom when creating/mapping.
impl PageTable for Sv39PageTable {
    fn new_kern_space() -> Self
    where
        Self: Sized,
    {
        let frame = frame_alloc().unwrap();
        Sv39PageTable {
            root_ppn: frame.ppn,
            frames: {
                let mut vec = Vec::with_capacity(32);
                vec.push(frame);
                vec
            },
        }
    }
    fn new() -> Self {
        let frame = frame_alloc().unwrap();
        Sv39PageTable {
            root_ppn: frame.ppn,
            frames: {
                let mut vec = Vec::with_capacity(32);
                vec.push(frame);
                vec
            },
        }
    }
    /// Create an empty page table from `satp`
    /// # Argument
    /// * `satp` Supervisor Address Translation & Protection reg. that points to the physical page containing the root page.
    fn from_token(satp: usize) -> Self {
        Self {
            root_ppn: PhysPageNum::from(satp & ((1usize << 44) - 1)),
            frames: Vec::new(),
        }
    }
    /// Predicate for the valid bit.
    fn is_mapped(&self, vpn: VirtPageNum) -> bool {
        if let Some(i) = self.find_pte(vpn) {
            if i.is_valid() {
                true
            } else {
                false
            }
        } else {
            false
        }
    }
    /// Find the page in the page table, creating the page on the way if not exists.
    /// Note: It does NOT create the terminal node. The caller must verify its validity and create according to his own needs.
    #[allow(unused)]
    /// Map the `vpn` to `ppn` with the `flags`.
    /// # Note
    /// Allocation should be done elsewhere.
    /// # Exceptions
    /// Panics if the `vpn` is mapped.
    fn try_map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> Result<(), MemoryError> {
        self.try_map_no_flush(vpn, ppn, flags)?;
        tlb_invalidate_vpn!(vpn);
        Ok(())
    }
    fn try_map_no_flush(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> Result<(), MemoryError> {
        let pte = self.find_pte_create(vpn)?;
        if pte.is_valid() {
            return Err(MemoryError::AlreadyMapped);
        }
        let mut pte_flags =
            PTEFlags::from_bits(flags.bits()).unwrap() | PTEFlags::V | PTEFlags::A | PTEFlags::D;
        if flags.contains(MapPermission::G) {
            pte_flags |= PTEFlags::G;
        }
        *pte = Sv39PageTableEntry::new(ppn, pte_flags);
        Ok(())
    }
    #[allow(unused)]
    /// Unmap the `vpn` to `ppn` with the `flags`.
    /// # Exceptions
    /// Panics if the `vpn` is NOT mapped (invalid).
    fn unmap(&mut self, vpn: VirtPageNum) {
        self.unmap_no_flush(vpn);
        tlb_invalidate_vpn!(vpn);
    }
    fn unmap_no_flush(&mut self, vpn: VirtPageNum) {
        let pte = self.find_pte_refmut(vpn).unwrap();
        assert!(pte.is_valid(), "vpn {:?} is invalid before unmapping", vpn);
        *pte = Sv39PageTableEntry::empty();
    }
    /// Translate the `vpn` into its corresponding `Some(PageTableEntry)` if exists
    /// `None` is returned if nothing is found.
    fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        // This is not the same map as we defined just now...
        // It is the map for func. programming.
        self.find_pte(vpn).map(|pte| pte.ppn())
    }
    /// Translate the virtual address into its corresponding `PhysAddr` if mapped in current page table.
    /// `None` is returned if nothing is found.
    fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        self.find_pte(va.clone().floor()).map(|pte| {
            let aligned_pa: PhysAddr = pte.ppn().into();
            let offset = va.page_offset();
            let aligned_pa_usize: usize = aligned_pa.into();
            (aligned_pa_usize + offset).into()
        })
    }
    fn block_and_ret_mut(&mut self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.revoke_write();
            tlb_invalidate_vpn!(vpn);
            Some(pte.ppn())
        } else {
            None
        }
    }
    fn block_and_ret_mut_no_flush(&mut self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.revoke_write();
            Some(pte.ppn())
        } else {
            None
        }
    }
    fn flush_tlb_page(&self, vpn: VirtPageNum) {
        tlb_invalidate_vpn!(vpn);
    }
    fn flush_tlb(&self) {
        tlb_invalidate();
    }
    /// Return the physical token to current page.
    fn token(&self) -> usize {
        8usize << 60 | self.root_ppn.0
    }
    fn revoke_read(&mut self, vpn: VirtPageNum) -> Result<(), ()> {
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.revoke_read();
            tlb_invalidate_vpn!(vpn);
            Ok(())
        } else {
            Err(())
        }
    }
    fn revoke_write(&mut self, vpn: VirtPageNum) -> Result<(), ()> {
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.revoke_write();
            tlb_invalidate_vpn!(vpn);
            Ok(())
        } else {
            Err(())
        }
    }
    fn revoke_execute(&mut self, vpn: VirtPageNum) -> Result<(), ()> {
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.revoke_execute();
            tlb_invalidate_vpn!(vpn);
            Ok(())
        } else {
            Err(())
        }
    }
    fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> Result<(), ()> {
        self.set_ppn_no_flush(vpn, ppn)?;
        tlb_invalidate_vpn!(vpn);
        Ok(())
    }
    fn set_ppn_no_flush(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> Result<(), ()> {
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.set_ppn(ppn);
            Ok(())
        } else {
            Err(())
        }
    }
    fn set_pte_flags(&mut self, vpn: VirtPageNum, flags: MapPermission) -> Result<(), ()> {
        self.set_pte_flags_no_flush(vpn, flags)?;
        tlb_invalidate_vpn!(vpn);
        Ok(())
    }
    fn set_pte_flags_no_flush(&mut self, vpn: VirtPageNum, flags: MapPermission) -> Result<(), ()> {
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.set_permission(flags);
            Ok(())
        } else {
            Err(())
        }
    }
    fn set_dirty_bit_no_flush(&mut self, vpn: VirtPageNum) -> Result<(), ()> {
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.bits |= PTEFlags::D.bits() as usize;
            Ok(())
        } else {
            Err(())
        }
    }
    fn clear_access_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()> {
        tlb_invalidate();
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.clear_access();
            Ok(())
        } else {
            Err(())
        }
    }
    fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()> {
        tlb_invalidate();
        if let Some(pte) = self.find_pte_refmut(vpn) {
            pte.clear_dirty();
            Ok(())
        } else {
            Err(())
        }
    }
    fn activate(&self) {
        let satp = self.token();
        debug_assert_eq!(
            (satp >> SATP_ASID_SHIFT) & SATP_ASID_MASK,
            KERN_ASID as usize,
            "kernel page table must use reserved ASID 0"
        );
        // Safety: satp 由当前页表根 PPN 构造。该低层入口只用于安装内核页表，
        // 写入后全量刷新，确保后续地址翻译不会沿用旧页表根的缓存。
        unsafe {
            satp::write(satp);
            let start = crate::task::perf::perf_time_now();
            asm!("sfence.vma");
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(start);
            crate::task::perf::record_tlb_activate_cycles(elapsed);
            crate::task::perf::record_tlb_activate();
        };
    }
    fn is_valid(&self, vpn: VirtPageNum) -> Option<bool> {
        self.find_pte(vpn).map(|pte| pte.is_valid())
    }
    fn is_dirty(&self, vpn: VirtPageNum) -> Option<bool> {
        self.find_pte(vpn).map(|pte| pte.is_dirty())
    }
    fn readable(&self, vpn: VirtPageNum) -> Option<bool> {
        self.find_pte(vpn).map(|pte| pte.readable())
    }
    fn writable(&self, vpn: VirtPageNum) -> Option<bool> {
        self.find_pte(vpn).map(|pte| pte.writable())
    }
    fn executable(&self, vpn: VirtPageNum) -> Option<bool> {
        self.find_pte(vpn).map(|pte| pte.executable())
    }
    fn user_access_ok(&self, vpn: VirtPageNum, access: UserAccess) -> Option<bool> {
        self.find_pte(vpn).map(|pte| {
            let flags = pte.flags();
            if !pte.is_valid() || !flags.contains(PTEFlags::U) {
                return false;
            }
            (!access.needs_read() || pte.readable()) && (!access.needs_write() || pte.writable())
        })
    }

    fn take_frames(&mut self) -> Vec<Arc<FrameTracker>> {
        core::mem::take(&mut self.frames)
    }
}
