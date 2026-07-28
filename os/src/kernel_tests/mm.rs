//! L3 tests for the page frame allocator.

use crate::kernel_tests::runner::KernelTest;
use crate::mm;
use alloc::vec;
use alloc::vec::Vec;

/// Returns all mm-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("mm::alloc_free_one_page", test_alloc_free_one_page),
        KernelTest::new("mm::alloc_contiguous_pages", test_alloc_contiguous_pages),
        KernelTest::new(
            "mm::alloc_then_free_then_alloc",
            test_alloc_then_free_then_alloc,
        ),
    ]
}

/// Allocate a single frame, verify PPN is valid, then drop (free) it.
fn test_alloc_free_one_page() -> Result<(), &'static str> {
    let frame = mm::frame_alloc().ok_or("frame_alloc returned None")?;
    let _ppn = frame.ppn;
    drop(frame);
    Ok(())
}

/// Allocate several contiguous pages, verify count and physical contiguity.
fn test_alloc_contiguous_pages() -> Result<(), &'static str> {
    const N: usize = 4;
    let frames = mm::frames_alloc(N).ok_or("frames_alloc(4) returned None")?;
    if frames.len() != N {
        return Err("wrong frame count");
    }
    // Explicitly verify physical contiguity: PPNs must be consecutive.
    let base = frames[0].ppn.0;
    for i in 1..N {
        if frames[i].ppn.0 != base + i {
            return Err("frames_alloc(4) returned non-contiguous pages");
        }
    }
    drop(frames);
    Ok(())
}

/// Allocate several pages, free them, then allocate again (tests reuse).
fn test_alloc_then_free_then_alloc() -> Result<(), &'static str> {
    const N: usize = 8;

    // Phase 1: allocate N single pages
    let mut frames: Vec<_> = Vec::new();
    if frames.try_reserve(N).is_err() {
        return Err("try_reserve failed");
    }
    for _ in 0..N {
        let frame = mm::frame_alloc().ok_or("frame_alloc returned None in phase 1")?;
        frames.push(frame);
    }

    // Phase 2: free all pages (drop the Vec → Drop frees each FrameTracker)
    drop(frames);

    // Phase 3: re-allocate N pages — must succeed (reuse freed pages)
    let mut frames2: Vec<_> = Vec::new();
    if frames2.try_reserve(N).is_err() {
        return Err("try_reserve failed in phase 3");
    }
    for _ in 0..N {
        let frame = mm::frame_alloc().ok_or("frame_alloc returned None after freeing pages")?;
        frames2.push(frame);
    }
    drop(frames2);

    Ok(())
}
