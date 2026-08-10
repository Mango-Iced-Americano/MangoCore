//! L3 tests for the page frame allocator.

use crate::config::PAGE_SIZE;
use crate::fs::vfs::{File, FileFlags};
use core::convert::TryInto;
use crate::kernel_tests::runner::KernelTest;
use crate::mm::{
    self, AddressSpace, AddressSpaceInner, FaultAccess, MapFlags, MapPermission, PageTable,
    PageTableImpl, VirtAddr,
};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

/// Returns all mm-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "mm::firmware_memory_reaches_allocator",
            test_firmware_memory_reaches_allocator,
        ),
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
        KernelTest::new(
            "mm::elf_ptload_pages_are_demand_paged",
            test_elf_ptload_pages_are_demand_paged,
        ),
        #[cfg(feature = "oom_handler")]
        KernelTest::new(
            "mm::shared_futex_pin_blocks_reclaim",
            test_shared_futex_pin_blocks_reclaim,
        ),
    ]
}

fn pread_exact(file: &File, mut offset: usize, mut dst: &mut [u8]) -> Result<(), &'static str> {
    while !dst.is_empty() {
        let count = file
            .pread(offset, dst)
            .map_err(|_| "failed to read ELF test fixture")?;
        if count == 0 {
            return Err("short read from ELF test fixture");
        }
        offset = offset
            .checked_add(count)
            .ok_or("ELF test fixture offset overflow")?;
        dst = &mut dst[count..];
    }
    Ok(())
}

/// Direct exec must reserve only PT_LOAD VMAs. Target frames, file overlays and
/// BSS zeros are materialized by the first fault, not by ELF construction.
fn test_elf_ptload_pages_are_demand_paged() -> Result<(), &'static str> {
    #[derive(Clone, Copy)]
    struct Load {
        flags: u32,
        offset: usize,
        vaddr: usize,
        filesz: usize,
        memsz: usize,
    }

    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no ELF fixture")?;
    let file =
        File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open ELF test fixture")?;
    let mut ehdr = [0u8; 64];
    pread_exact(&file, 0, &mut ehdr)?;
    if &ehdr[..4] != b"\x7fELF" || ehdr[4] != 2 || ehdr[5] != 1 {
        return Err("ELF test fixture is not little-endian ELF64");
    }
    let read_u16 = |offset: usize| u16::from_le_bytes([ehdr[offset], ehdr[offset + 1]]) as usize;
    let read_u64 =
        |offset: usize| usize::from_le_bytes(ehdr[offset..offset + 8].try_into().unwrap());
    let elf_entry = read_u64(24);
    let phoff = read_u64(32);
    let phentsize = read_u16(54);
    let phnum = read_u16(56);
    if phentsize < 56 || phnum == 0 {
        return Err("ELF test fixture has invalid program headers");
    }
    let phdr_bytes = phentsize
        .checked_mul(phnum)
        .ok_or("ELF program-header size overflow")?;
    let mut phdrs = vec![0u8; phdr_bytes];
    pread_exact(&file, phoff, &mut phdrs)?;
    let mut loads = Vec::new();
    loads
        .try_reserve(phnum)
        .map_err(|_| "failed to reserve ELF test load headers")?;
    for index in 0..phnum {
        let ph = &phdrs[index * phentsize..];
        let u32_at = |offset: usize| u32::from_le_bytes(ph[offset..offset + 4].try_into().unwrap());
        let usize_at =
            |offset: usize| usize::from_le_bytes(ph[offset..offset + 8].try_into().unwrap());
        if u32_at(0) == 1 {
            loads.push(Load {
                flags: u32_at(4),
                offset: usize_at(8),
                vaddr: usize_at(16),
                filesz: usize_at(32),
                memsz: usize_at(40),
            });
        }
    }

    let (inner, _program_break, info) =
        AddressSpaceInner::<PageTableImpl>::from_elf_inode(file.clone())
            .map_err(|_| "direct ELF loader rejected the ktest fixture")?;
    let bias = info
        .entry
        .checked_sub(elf_entry)
        .ok_or("ELF runtime entry is below its link-time entry")?;
    let entry_load = loads
        .iter()
        .find(|load| {
            let start = load.vaddr.saturating_add(bias);
            info.entry >= start && info.entry < start.saturating_add(load.filesz)
        })
        .ok_or("ELF entry is not covered by file-backed PT_LOAD bytes")?;
    let entry_file_offset = entry_load
        .offset
        .checked_add(info.entry - entry_load.vaddr.saturating_add(bias))
        .ok_or("ELF entry file offset overflow")?;
    let mut expected_entry = [0u8; 1];
    pread_exact(&file, entry_file_offset, &mut expected_entry)?;

    let mut bss_addr = None;
    for load in loads
        .iter()
        .filter(|load| load.flags & 4 != 0 && load.memsz > load.filesz)
    {
        let mut candidate = load
            .vaddr
            .checked_add(bias)
            .and_then(|start| start.checked_add(load.filesz))
            .ok_or("ELF BSS start overflow")?;
        let end = load
            .vaddr
            .checked_add(bias)
            .and_then(|start| start.checked_add(load.memsz))
            .ok_or("ELF BSS end overflow")?;
        while candidate < end {
            let covering_end = loads
                .iter()
                .filter_map(|other| {
                    let start = other.vaddr.checked_add(bias)?;
                    let file_end = start.checked_add(other.filesz)?;
                    (start <= candidate && candidate < file_end).then_some(file_end)
                })
                .max();
            match covering_end {
                Some(next) => candidate = next,
                None => {
                    bss_addr = Some(candidate);
                    break;
                }
            }
        }
        if bss_addr.is_some() {
            break;
        }
    }
    let bss_addr = bss_addr.ok_or("ELF test fixture has no unobscured readable BSS byte")?;

    let space = AddressSpace::new(inner);
    let entry_vpn = VirtAddr::from(info.entry).floor();
    let bss_vpn = VirtAddr::from(bss_addr).floor();
    if space
        .read(|inner| inner.translate(entry_vpn).is_some() || inner.translate(bss_vpn).is_some())
    {
        return Err("direct ELF loader eagerly allocated a PT_LOAD target frame");
    }

    let entry_pa = space
        .fault_in_user_va_retry(VirtAddr::from(info.entry), FaultAccess::Execute)
        .map_err(|_| "ELF entry demand fault failed")?;
    let entry_byte = unsafe {
        entry_pa
            .floor()
            .with_bytes(|page| page[entry_pa.page_offset()])
    };
    if entry_byte != expected_entry[0] {
        return Err("ELF entry fault copied the wrong file byte");
    }
    if entry_vpn != bss_vpn && space.read(|inner| inner.translate(bss_vpn).is_some()) {
        return Err("faulting the ELF entry populated an unrelated BSS page");
    }

    let bss_pa = space
        .fault_in_user_va_retry(VirtAddr::from(bss_addr), FaultAccess::Load)
        .map_err(|_| "ELF BSS demand fault failed")?;
    let bss_byte = unsafe { bss_pa.floor().with_bytes(|page| page[bss_pa.page_offset()]) };
    if bss_byte != 0 {
        return Err("ELF BSS demand fault did not zero-fill memory");
    }
    Ok(())
}

/// 验证运行期固件拓扑已经贯穿 RAM 判定和内核恒等映射元数据。
///
/// 该测试在默认 1 GiB 配置下同样有效；当 `QEMU_MEMORY=8G` 使固件末端超过
/// 编译期 `MEMORY_END` 时，还会明确证明静态上界之外的最后一个可用页已被接入。
fn test_firmware_memory_reaches_allocator() -> Result<(), &'static str> {
    let mut last_usable = None;
    crate::hal::firmware::for_each_usable_ram_range(&[], |start, end| {
        last_usable = Some((start, end));
    });
    let (_, usable_end) = last_usable.ok_or("firmware published no usable RAM")?;
    let probe_addr = usable_end
        .checked_sub(PAGE_SIZE)
        .ok_or("last usable RAM range is smaller than one page")?;
    if !mm::is_ram_phys_addr(probe_addr) || !mm::is_allocatable_ram_phys_addr(probe_addr) {
        return Err("last firmware RAM page is not allocatable");
    }

    let probe_ppn = mm::PhysAddr::from(probe_addr).floor();
    if mm::KERNEL_SPACE.lock().is_dirty(probe_ppn).is_none() {
        return Err("last firmware RAM page lacks kernel mapping metadata");
    }

    let total = crate::hal::firmware::usable_memory_size();
    if total / 1024 != mm::total_memory_kbytes() {
        return Err("ABI memory total differs from firmware usable RAM");
    }
    crate::println!(
        "[memory-test] usable={} MiB highest_page={:#x} dynamic_above_static={}",
        total / (1024 * 1024),
        probe_addr,
        usable_end > crate::config::MEMORY_END,
    );
    Ok(())
}

/// 共享 futex 的 resident-frame key 依赖队列 pin 保持身份稳定。
///
/// 第一次深度回收必须跳过被 pin 的页并保留候选；pin 解除后第二次回收应能压缩
/// 同一页。固定到 mmap 区以下，确保覆盖过去会进入 `force_swap` 的分支。
#[cfg(feature = "oom_handler")]
fn test_shared_futex_pin_blocks_reclaim() -> Result<(), &'static str> {
    const TEST_BASE: usize = crate::config::ELF_PIE_BASE + 0x20_0000;
    let test_vpn = VirtAddr::from(TEST_BASE).floor();

    let space = AddressSpace::new(AddressSpaceInner::<PageTableImpl>::new_bare());
    let frame = mm::frame_alloc().ok_or("failed to allocate shared futex frame")?;
    let original_ppn = frame.ppn;
    space.write(|inner| {
        let mapped = inner.shm_mmap(
            TEST_BASE,
            PAGE_SIZE,
            MapPermission::R | MapPermission::W | MapPermission::U,
            MapFlags::MAP_SHARED | MapFlags::MAP_ANONYMOUS | MapFlags::MAP_FIXED_NOREPLACE,
            core::slice::from_ref(&frame),
            true,
        );
        if mapped != TEST_BASE as isize {
            return Err("failed to create fixed shared futex mapping");
        }
        inner
            .fault_in_user_va(VirtAddr::from(TEST_BASE), FaultAccess::Store)
            .map(|_| ())
            .map_err(|_| "failed to install shared futex PTE")
    })?;
    drop(frame);

    let pin = space
        .read(|inner| {
            inner
                .futex_shared_backing(VirtAddr::from(TEST_BASE))
                .ok()
                .flatten()
        })
        .ok_or("failed to resolve shared futex backing")?;
    if space.write(|inner| inner.do_deep_clean()) != 0 {
        return Err("deep reclaim replaced a pinned shared futex page");
    }
    space.read(|inner| {
        let current = inner
            .futex_shared_backing(VirtAddr::from(TEST_BASE))
            .ok()
            .flatten()
            .ok_or("pinned shared futex backing disappeared")?;
        if !Arc::ptr_eq(&pin, &current)
            || inner.translate(test_vpn).map(|ppn| ppn.0) != Some(original_ppn.0)
        {
            return Err("pinned shared futex backing identity changed");
        }
        Ok(())
    })?;

    drop(pin);
    if space.write(|inner| inner.do_deep_clean()) != 1 {
        return Err("unpinned shared futex page was not reconsidered for reclaim");
    }
    if space.read(|inner| inner.translate(test_vpn).is_some()) {
        return Err("compressed shared futex page kept a stale PTE");
    }
    space.write(|inner| {
        inner
            .fault_in_user_va(VirtAddr::from(TEST_BASE), FaultAccess::Load)
            .map(|_| ())
            .map_err(|_| "failed to restore reclaimed shared futex page")
    })?;
    Ok(())
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
