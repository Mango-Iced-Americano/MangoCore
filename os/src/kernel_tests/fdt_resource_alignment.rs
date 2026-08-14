//! 早期固件资源收集的页对齐与合并行为测试。
//!
//! 回归覆盖：VF2 vendor DTB 中 `dc8200@29400000` 在同一 4 KiB 页内声明两个
//! `reg` 子区间，raw 字节粒度下 `push_range` 不会合并，内核按页映射第二段时
//! 对同一 VPN 重复映射导致 `AlreadyMapped` panic（实板启动即崩）。修复要求
//! 所有进入 MMIO/reserved 列表的区间先页对齐再合并。

use alloc::vec;
use alloc::vec::Vec;

use crate::hal::firmware::{page_align_range, push_range};
use crate::kernel_tests::runner::KernelTest;

pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "fdt_resource_alignment::merges_same_page_multi_reg_vf2_dc8200",
            test_merges_same_page_multi_reg_vf2_dc8200,
        ),
        KernelTest::new(
            "fdt_resource_alignment::merges_distinct_devices_sharing_a_page",
            test_merges_distinct_devices_sharing_a_page,
        ),
        KernelTest::new(
            "fdt_resource_alignment::page_aligns_partial_pages_and_preserves_aligned",
            test_page_aligns_partial_pages_and_preserves_aligned,
        ),
        KernelTest::new(
            "fdt_resource_alignment::rejects_top_address_overflow",
            test_rejects_top_address_overflow,
        ),
        KernelTest::new(
            "fdt_resource_alignment::reserved_ranges_merge_at_page_granularity",
            test_reserved_ranges_merge_at_page_granularity,
        ),
    ]
}

fn test_merges_same_page_multi_reg_vf2_dc8200() -> Result<(), &'static str> {
    let vf2_dc8200_raw = [
        (0x2940_0000usize, 0x2940_0100usize),
        (0x2940_0800usize, 0x2940_2800usize),
    ];
    let mut mmio = [(0usize, 0usize); 8];
    let mut count = 0;
    for &(start, end) in &vf2_dc8200_raw {
        let aligned = page_align_range(start, end).ok_or("page alignment overflowed")?;
        if !push_range(&mut mmio, &mut count, aligned) {
            return Err("push_range rejected a valid page-aligned range");
        }
    }
    if count != 1 {
        return Err("same-page reg entries did not merge into one page range");
    }
    if mmio[0] != (0x2940_0000, 0x2940_3000) {
        return Err("merged dc8200 range is not the page-rounded union");
    }
    Ok(())
}

fn test_merges_distinct_devices_sharing_a_page() -> Result<(), &'static str> {
    let same_page_devices = [
        (0x1000_0000usize, 0x1000_0010usize),
        (0x1000_0800usize, 0x1000_0f00usize),
    ];
    let mut mmio = [(0usize, 0usize); 8];
    let mut count = 0;
    for &(start, end) in &same_page_devices {
        let aligned = page_align_range(start, end).ok_or("page alignment overflowed")?;
        if !push_range(&mut mmio, &mut count, aligned) {
            return Err("push_range rejected a valid page-aligned range");
        }
    }
    if count != 1 || mmio[0] != (0x1000_0000, 0x1000_1000) {
        return Err("distinct same-page devices were not merged at page granularity");
    }
    Ok(())
}

fn test_page_aligns_partial_pages_and_preserves_aligned() -> Result<(), &'static str> {
    let partial = page_align_range(0x1000_0100usize, 0x1000_0800usize);
    if partial != Some((0x1000_0000, 0x1000_1000)) {
        return Err("partial-page range was not rounded to the enclosing page");
    }
    let aligned = page_align_range(0x1000_0000usize, 0x1000_1000usize);
    if aligned != Some((0x1000_0000, 0x1000_1000)) {
        return Err("already page-aligned range was changed");
    }
    Ok(())
}

fn test_rejects_top_address_overflow() -> Result<(), &'static str> {
    if page_align_range(usize::MAX - 0x100, usize::MAX).is_some() {
        return Err("page alignment at the top of the address space must fail closed");
    }
    Ok(())
}

fn test_reserved_ranges_merge_at_page_granularity() -> Result<(), &'static str> {
    let mut reserved = [(0usize, 0usize); 8];
    let mut count = 0;
    for &(start, end) in &[(0x4000_0000usize, 0x4000_0080usize), (0x4000_0800usize, 0x4000_1000usize)]
    {
        let aligned = page_align_range(start, end).ok_or("page alignment overflowed")?;
        if !push_range(&mut reserved, &mut count, aligned) {
            return Err("push_range rejected a valid reserved page range");
        }
    }
    if count != 1 || reserved[0] != (0x4000_0000, 0x4000_1000) {
        return Err("reserved ranges did not merge at page granularity");
    }
    Ok(())
}
