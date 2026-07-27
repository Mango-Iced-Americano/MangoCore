//! 内存管理子系统入口。
//!
//! 管理物理页帧、用户地址空间、VMA、页表、内核堆、用户指针访问和
//! mmap/page fault 路径。架构相关页表实现经由 `PageTableImpl` 注入，
//! 上层通过本模块导出的 trait 和辅助函数操作地址空间。
//!
//! # TLB
//!
//! 用户 PTE 修改统一经 `TlbBatch` 收集并提交；内核页表仍由安全的单页接口
//! 立即刷新。`*_no_flush` 仅供 batch 内部使用，普通调用方不得直接绕过提交协议。
//!
//! # Locking
//!
//! 用户地址访问会通过当前任务的 `AddressSpace` 锁 fault-in 页面。
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
mod page_fault;
mod page_table;
mod slab;
mod sysctl;
mod tlb_batch;
mod uaccess;
mod user_mapper;
mod vma;
mod vma_set;
#[cfg(feature = "zram")]
mod zram;
pub use crate::hal::{KernelPageTableImpl, PageTableImpl};
pub use address::PPNRange;
use address::VPNRange;
pub use address::{PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum};
pub use address_space::{AddressSpace, MemoryError};
pub use frame_allocator::{
    frame_alloc, frame_alloc_uninit, frame_dealloc, frame_frag_diag, frame_reclaim_linker_range,
    frame_reserve, frames_alloc, frames_alloc_any, frames_alloc_fresh_contiguous,
    is_allocatable_ram_phys_addr, is_ram_phys_addr, unallocated_frames, FrameTracker,
};
pub use frame_store::Frame;
pub use heap_allocator::{
    heap_free_histogram, heap_stats, KERNEL_HEAP_CURRENT_BYTES, KERNEL_HEAP_MAX_BYTES,
};
pub use kernel_space::{kernel_token, KernelSpace, KERNEL_SPACE};
pub use page_table::{FaultAccess, PageTable, UserAccess};
pub use sysctl::{
    commit_limit_kbytes, committed_as_kbytes, free_memory_kbytes, max_map_count, min_free_kbytes,
    overcommit_allows, overcommit_memory, overcommit_ratio, panic_on_oom, set_max_map_count,
    set_min_free_kbytes, set_overcommit_memory, set_overcommit_ratio, set_panic_on_oom,
    total_memory_kbytes,
};
pub(crate) use tlb_batch::{TlbBatch, TlbPublication};
pub use vma::{MapFlags, MapPermission};
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
    get_from_user,
    translate_user_va_checked,
    translated_byte_buffer,
    translated_byte_buffer_append_to_existing_vec,
    translated_ref,
    translated_ref_write,
    translated_refmut,
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
    // UserBufferIterator,
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
    KERNEL_SPACE.lock().activate();
}
pub use crate::hal::tlb_invalidate;
