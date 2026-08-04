//! 内存管理子系统入口。
//!
//! 管理物理页帧、用户地址空间、VMA、页表、内核堆、用户指针访问和
//! mmap/page fault 路径。架构相关页表实现经由 `PageTableImpl` 注入，
//! 上层通过本模块导出的 trait 和辅助函数操作地址空间。
//!
//! # TLB
//!
//! 用户 PTE 修改由 `UserMapper` 写入，并由 `MmuGather` 收集失效范围与退休 frame；
//! 地址空间解锁后，`TlbFlush` 才执行本地/远端失效。内核页表仍走独立同步路径。
//!
//! # Locking
//!
//! 用户地址访问会通过当前任务的外层 `AddressSpace` 串行化并 fault-in 页面。
//! 调用方不得在持有同一地址空间锁时再次进入 uaccess 路径。

pub mod address;
mod address_space;
mod filemap;
mod frame_allocator;
mod frame_store;
mod heap_allocator;
#[cfg(feature = "heap_trace")]
pub mod heap_trace;
mod kernel_mapper;
mod kernel_space;
mod mapper;
mod mmap;
mod mmu_gather;
mod page_fault;
mod page_table;
mod slab;
mod sysctl;
mod tlb;
mod uaccess;
mod user_mapper;
mod vma;
mod vma_set;
#[cfg(feature = "zram")]
mod zram;
pub use crate::hal::{KernelPageTableImpl, PageTableImpl};
pub use address::PPNRange;
pub(crate) use address::VPNRange;
pub use address::{PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum};
pub(crate) use address_space::UserVmContext;
pub use address_space::{AddressSpace, AddressSpaceInner, FaultOutcome, MemoryError, RetryWait};
pub use frame_allocator::{
    frame_alloc, frame_alloc_uninit, frame_dealloc, frame_frag_diag, frame_reclaim_linker_range,
    frame_reserve, frames_alloc, frames_alloc_any, frames_alloc_fresh_contiguous,
    is_allocatable_ram_phys_addr, is_ram_phys_addr, try_unallocated_frames, unallocated_frames,
    FrameTracker,
};
pub use frame_store::Frame;
pub use heap_allocator::{
    heap_free_histogram, heap_stats, try_heap_stats, KERNEL_HEAP_CURRENT_BYTES,
    KERNEL_HEAP_MAX_BYTES,
};
pub(crate) use kernel_space::remove_kernel_mapping_synchronized;
pub use kernel_space::{kernel_token, KernelSpace, KERNEL_SPACE};
pub(crate) use mmu_gather::{FlushRange, MmuGather};
pub use page_table::{FaultAccess, PageTable, UserAccess};
pub use sysctl::{
    commit_limit_kbytes, committed_as_kbytes, free_memory_kbytes, max_map_count, min_free_kbytes,
    overcommit_allows, overcommit_memory, overcommit_ratio, panic_on_oom, set_max_map_count,
    set_min_free_kbytes, set_overcommit_memory, set_overcommit_ratio, set_panic_on_oom,
    total_memory_kbytes,
};
pub(crate) use tlb::{TlbContext, TlbFlush};
pub use vma::{MapFlags, MapPermission};
pub(crate) use vma::{FileVmaRmap, FileVmaSnapshot, Vma};
type MmResult<T> = Result<T, MemoryError>;

/// Stack alignment required whenever the kernel enters userspace.
///
/// Both the RISC-V ELF psABI and the LoongArch ELF ABI require a 16-byte
/// aligned stack pointer at function entry. LLVM relies on this invariant
/// when folding address additions into alignment-sensitive instructions.
pub const USER_STACK_ABI_ALIGN: usize = 16;
#[allow(unused_imports)]
pub use uaccess::{
    check_user_range,
    copy_from_user,
    copy_from_user_array,
    copy_to_user,
    copy_to_user_array,
    copy_to_user_string,
    fault_in_user_range,
    get_from_user,
    translated_str,
    try_get_from_user,
    user_accessible_len,
    UserBuffer,
    UserBufferReader,
    UserBufferWriter,
    UserCString,
    UserIoVec,
    UserPtr,
    UserPtrMut,
    UserSlice,
};

/// 初始化内核堆、物理页帧分配器并激活内核页表。
///
/// # Semantics
///
/// 该函数只在启动阶段调用一次。调用完成后，`KERNEL_SPACE` 成为当前页表，
/// 后续内存分配和用户地址空间创建才能安全执行。
pub fn init() {
    heap_allocator::init_heap();
    #[cfg(feature = "heap_trace")]
    heap_trace::enable();
    frame_allocator::init_frame_allocator();
    activate_kernel_page_table();
}

/// 在当前 CPU 安装 BSP 已构造完成的内核页表。
///
/// BSP 负责唯一一次堆、帧分配器和页表构造；AP 只写本 CPU 的地址翻译控制
/// 寄存器并刷新本地 TLB，绝不能重复执行 [`init`]。
pub(crate) fn activate_kernel_page_table() {
    KERNEL_SPACE.lock().activate();
}
pub use crate::hal::tlb_invalidate;
