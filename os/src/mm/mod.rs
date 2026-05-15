pub mod addr;
pub mod address;
mod arch;
mod error;
mod filemap;
mod frame_store;
mod frame_allocator;
mod heap_allocator;
mod kernel_mapper;
mod layout;
mod vma;
mod mapper;
mod memory_set;
mod mmap;
pub mod page;
mod page_fault;
mod page_table;
mod uaccess;
mod vma_set;
#[cfg(feature = "zram")]
mod zram;
pub use crate::hal::{KernelPageTableImpl, PageTableImpl};
#[allow(unused_imports)]
pub use addr::{PhysRegion, VirtRegion};
pub use address::PPNRange;
use address::VPNRange;
pub use address::{PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum};
#[allow(unused_imports)]
pub use arch::{CurrentMmArch, MemoryManagementArch};
#[allow(unused_imports)]
pub use error::{MmError, MmResult};
pub use frame_allocator::{
    frame_alloc, frame_alloc_uninit, frame_dealloc, frame_reserve, frames_alloc,
    unallocated_frames, FrameTracker,
};
pub use heap_allocator::heap_stats;
#[allow(unused_imports)]
pub use layout::{KernelLayout, UserLayout};
pub use frame_store::Frame;
pub use vma::{MapFlags, MapPermission};
#[allow(unused_imports)]
pub use mapper::PageMapper;
pub use memory_set::kernel_token;
pub use memory_set::MemoryError;
pub use memory_set::{MemorySet, KERNEL_SPACE};
#[allow(unused_imports)]
pub use page::{MemAttr, PageFaultKind, PageProt};
pub use page_table::{FaultAccess, PageTable, UserAccess};
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
    UserBuffer,
    // UserBufferIterator,
};

pub fn init() {
    heap_allocator::init_heap();
    frame_allocator::init_frame_allocator();
    KERNEL_SPACE.lock().activate();
}
pub use crate::hal::tlb_invalidate;

#[macro_export]
/// Convert user pointer trg to `Some(*trg)` or `None` if null.
macro_rules! move_ptr_to_opt {
    ($trg:ident) => {
        if $trg != null() {
            let t = *translated_ref(current_user_token(), $trg);
            Some(t)
        } else {
            None
        }
    };
    ($token:ident,$trg:ident) => {
        if $trg != null() {
            let t = *translated_ref($token, $trg);
            Some(t)
        } else {
            None
        }
    };
}

#[macro_export]
/// Convert user pointer `trg:*const T` to `Some(trg as & T)` or `None` if null.
macro_rules! ptr_to_opt_ref {
    ($trg:ident) => {
        if $trg != null() {
            Some(translated_ref(current_user_token(), $trg))
        } else {
            None
        }
    };
    ($token:ident,$trg:ident) => {
        if $trg != null() {
            Some(translated_ref($token, $trg))
        } else {
            None
        }
    };
}
