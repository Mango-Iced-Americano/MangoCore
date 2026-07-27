//! L3 tests for the page frame allocator.

use crate::config::PAGE_SIZE;
use crate::kernel_tests::runner::KernelTest;
use crate::mm::{
    self, AddressSpace, FaultAccess, MapPermission, PageTable, PageTableImpl, VirtAddr,
};
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
        KernelTest::new(
            "mm::local_tlb_batch_map_protect_unmap",
            test_local_tlb_batch_map_protect_unmap,
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

/// 通过正式 AddressSpace API 覆盖已发布页表的映射、权限修改和解除映射。
fn test_local_tlb_batch_map_protect_unmap() -> Result<(), &'static str> {
    const TEST_BASE: usize = 0x40_0000;
    const TEST_LEN: usize = PAGE_SIZE * 2;

    let mut space = AddressSpace::<PageTableImpl>::new_bare();
    space.publish_local();
    space.insert_framed_area(
        VirtAddr::from(TEST_BASE),
        VirtAddr::from(TEST_BASE + TEST_LEN),
        MapPermission::R | MapPermission::W | MapPermission::U,
    );

    let first_vpn = VirtAddr::from(TEST_BASE).floor();
    let second_vpn = VirtAddr::from(TEST_BASE + PAGE_SIZE).floor();
    if space.translate(first_vpn).is_none() || space.translate(second_vpn).is_none() {
        return Err("batch map did not install both PTEs");
    }
    if PageTableImpl::from_token(space.token()).writable(first_vpn) != Some(true) {
        return Err("new writable mapping has a read-only PTE");
    }
    if space
        .fault_in_user_va(VirtAddr::from(TEST_BASE), FaultAccess::Store)
        .is_err()
    {
        return Err("writable PTE rejected a store before mprotect");
    }

    space
        .mprotect(TEST_BASE, TEST_LEN, MapPermission::R | MapPermission::U)
        .map_err(|_| "batch mprotect failed")?;
    if space.contains_valid_buffer(TEST_BASE, TEST_LEN, MapPermission::W) {
        return Err("mprotect left the VMA writable");
    }
    if !space.contains_valid_buffer(TEST_BASE, TEST_LEN, MapPermission::R) {
        return Err("mprotect removed read permission");
    }
    if PageTableImpl::from_token(space.token()).writable(first_vpn) != Some(false) {
        return Err("mprotect did not clear the PTE write bit");
    }
    if space
        .fault_in_user_va(VirtAddr::from(TEST_BASE), FaultAccess::Load)
        .is_err()
    {
        return Err("read-only PTE rejected a load after mprotect");
    }
    if space
        .fault_in_user_va(VirtAddr::from(TEST_BASE), FaultAccess::Store)
        .is_ok()
    {
        return Err("read-only PTE accepted a store after mprotect");
    }

    space
        .munmap(TEST_BASE, TEST_LEN)
        .map_err(|_| "batch munmap failed")?;
    if space.translate(first_vpn).is_some() || space.translate(second_vpn).is_some() {
        return Err("batch munmap left a valid PTE");
    }

    // 再次使用相同 VPN，验证解除映射并释放 frame 后不会残留页表状态。
    space.insert_framed_area(
        VirtAddr::from(TEST_BASE),
        VirtAddr::from(TEST_BASE + PAGE_SIZE),
        MapPermission::R | MapPermission::W | MapPermission::U,
    );
    space
        .munmap(TEST_BASE, PAGE_SIZE)
        .map_err(|_| "single-page batch munmap failed")?;
    if space.translate(first_vpn).is_some() {
        return Err("single-page batch left a valid PTE");
    }
    Ok(())
}
