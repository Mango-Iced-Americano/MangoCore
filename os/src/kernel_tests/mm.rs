//! L3 tests for the page frame allocator.

use crate::config::PAGE_SIZE;
use crate::kernel_tests::runner::KernelTest;
use crate::mm::{
    self, AddressSpace, AddressSpaceInner, FaultAccess, MapPermission, PageTable, PageTableImpl,
    VirtAddr,
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
            "mm::local_mmu_gather_map_protect_unmap",
            test_local_mmu_gather_map_protect_unmap,
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

/// 通过正式 `AddressSpace::write()` 边界覆盖映射、权限修改和解除映射。
fn test_local_mmu_gather_map_protect_unmap() -> Result<(), &'static str> {
    const TEST_BASE: usize = 0x40_0000;
    const TEST_LEN: usize = PAGE_SIZE * 2;

    let space = AddressSpace::new(AddressSpaceInner::<PageTableImpl>::new_bare());
    space.activate_on(crate::smp::cpu_id());
    space.write(|inner| {
        inner.insert_framed_area(
            VirtAddr::from(TEST_BASE),
            VirtAddr::from(TEST_BASE + TEST_LEN),
            MapPermission::R | MapPermission::W | MapPermission::U,
        );
    });

    let first_vpn = VirtAddr::from(TEST_BASE).floor();
    let second_vpn = VirtAddr::from(TEST_BASE + PAGE_SIZE).floor();
    space.read(|inner| {
        if inner.translate(first_vpn).is_none() || inner.translate(second_vpn).is_none() {
            return Err("gathered map did not install both PTEs");
        }
        if PageTableImpl::from_token(inner.token()).writable(first_vpn) != Some(true) {
            return Err("new writable mapping has a read-only PTE");
        }
        Ok(())
    })?;
    space.write(|inner| {
        inner
            .fault_in_user_va(VirtAddr::from(TEST_BASE), FaultAccess::Store)
            .map(|_| ())
            .map_err(|_| "writable PTE rejected a store before mprotect")
    })?;

    space.write(|inner| {
        inner
            .mprotect(TEST_BASE, TEST_LEN, MapPermission::R | MapPermission::U)
            .map_err(|_| "gathered mprotect failed")
    })?;
    space.write(|inner| {
        if inner.contains_valid_buffer(TEST_BASE, TEST_LEN, MapPermission::W) {
            return Err("mprotect left the VMA writable");
        }
        if !inner.contains_valid_buffer(TEST_BASE, TEST_LEN, MapPermission::R) {
            return Err("mprotect removed read permission");
        }
        if PageTableImpl::from_token(inner.token()).writable(first_vpn) != Some(false) {
            return Err("mprotect did not clear the PTE write bit");
        }
        if inner
            .fault_in_user_va(VirtAddr::from(TEST_BASE), FaultAccess::Load)
            .is_err()
        {
            return Err("read-only PTE rejected a load after mprotect");
        }
        if inner
            .fault_in_user_va(VirtAddr::from(TEST_BASE), FaultAccess::Store)
            .is_ok()
        {
            return Err("read-only PTE accepted a store after mprotect");
        }
        Ok(())
    })?;

    space.write(|inner| {
        inner
            .munmap(TEST_BASE, TEST_LEN)
            .map_err(|_| "gathered munmap failed")
    })?;
    space.read(|inner| {
        if inner.translate(first_vpn).is_some() || inner.translate(second_vpn).is_some() {
            return Err("gathered munmap left a valid PTE");
        }
        Ok(())
    })?;

    // 再次使用相同 VPN，验证解除映射并释放 frame 后不会残留页表状态。
    space.write(|inner| {
        inner.insert_framed_area(
            VirtAddr::from(TEST_BASE),
            VirtAddr::from(TEST_BASE + PAGE_SIZE),
            MapPermission::R | MapPermission::W | MapPermission::U,
        );
    });
    space.write(|inner| {
        inner
            .munmap(TEST_BASE, PAGE_SIZE)
            .map_err(|_| "single-page gathered munmap failed")
    })?;
    if space.read(|inner| inner.translate(first_vpn).is_some()) {
        return Err("single-page gather left a valid PTE");
    }
    Ok(())
}
