pub mod address;
mod address_space;
mod filemap;
mod frame_store;
mod frame_allocator;
mod heap_allocator;
mod kernel_mapper;
mod kernel_space;
mod vma;
mod mapper;
mod mmap;
mod page_fault;
mod page_table;
mod uaccess;
mod user_mapper;
mod vma_set;
#[cfg(feature = "zram")]
mod zram;
pub use crate::hal::{KernelPageTableImpl, PageTableImpl};
pub use address::PPNRange;
use address::VPNRange;
pub use address::{PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum};
pub use frame_allocator::{
    frame_alloc, frame_alloc_uninit, frame_dealloc, frame_reserve, frames_alloc,
    unallocated_frames, FrameTracker,
};
pub use frame_store::Frame;
pub use heap_allocator::{heap_free_histogram, heap_stats};
pub use vma::{MapFlags, MapPermission};
pub use address_space::{AddressSpace, MemoryError};
pub use kernel_space::{kernel_token, KernelSpace, KERNEL_SPACE};
pub use page_table::{FaultAccess, PageTable, UserAccess};
type MmResult<T> = Result<T, MemoryError>;
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
    UserBufferReader,
    UserBufferWriter,
    UserCString,
    UserIoVec,
    UserPtr,
    UserPtrMut,
    UserSlice,
    // UserBufferIterator,
};

pub fn init() {
    heap_allocator::init_heap();
    frame_allocator::init_frame_allocator();
    KERNEL_SPACE.lock().activate();
}
pub use crate::hal::tlb_invalidate;
