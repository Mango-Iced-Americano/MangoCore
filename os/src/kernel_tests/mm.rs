//! L3 tests for the page frame allocator.

use alloc::vec;
use alloc::vec::Vec;
use crate::kernel_tests::runner::KernelTest;
use crate::mm;

/// Returns all mm-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("mm::alloc_free_one_page", test_alloc_free_one_page),
        KernelTest::new("mm::alloc_contiguous_pages", test_alloc_contiguous_pages),
    ]
}

/// Allocate a single frame, then drop (free) it.
fn test_alloc_free_one_page() -> Result<(), &'static str> {
    let frame = mm::frame_alloc().ok_or("frame_alloc returned None")?;
    let _ppn = frame.ppn;
    drop(frame);
    Ok(())
}

/// Allocate several contiguous pages, then free them.
fn test_alloc_contiguous_pages() -> Result<(), &'static str> {
    const N: usize = 4;
    let frames = mm::frames_alloc(N).ok_or("frames_alloc(4) returned None")?;
    if frames.len() != N {
        return Err("wrong frame count");
    }
    drop(frames);
    Ok(())
}
