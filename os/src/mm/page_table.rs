//! 架构无关页表接口。
//!
//! `PageTable` 描述 MangoCore 内存子系统需要的最小页表能力，具体位布局和
//! 刷新指令由 `hal/arch/*` 实现。
//!
//! # TLB
//!
//! 默认 PTE 修改接口必须自行刷新受影响的 TLB 条目。名字带 `_no_flush`
//! 的原始接口只允许 `UserMapper` 调用；`MmuGather` 负责把 frame 保留到失效完成。

use super::{
    FrameTracker, MapPermission, MemoryError, PhysAddr, PhysPageNum, VirtAddr, VirtPageNum,
};
use alloc::{sync::Arc, vec::Vec};

/// 用户地址访问方向。
///
/// # Semantics
///
/// `Read` 表示内核从用户页读数据，`Write` 表示内核向用户页写数据，
/// `ReadWrite` 用于需要同时验证读写权限的可变引用路径。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserAccess {
    Read,
    Write,
    ReadWrite,
}

impl UserAccess {
    #[inline(always)]
    pub fn needs_read(self) -> bool {
        matches!(self, UserAccess::Read | UserAccess::ReadWrite)
    }

    #[inline(always)]
    pub fn needs_write(self) -> bool {
        matches!(self, UserAccess::Write | UserAccess::ReadWrite)
    }
}

/// 缺页处理的访问类型。
///
/// # Semantics
///
/// page fault 路径使用该类型决定要补齐的权限、是否触发 CoW，以及最终应向
/// 用户态报告哪类 fault。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultAccess {
    Load,
    Store,
    Execute,
}

#[allow(unused)]
/// 地址空间页表操作接口。
///
/// # Semantics
///
/// 实现方负责创建/销毁页表页、转换虚拟地址、修改叶子 PTE 权限和切换当前
/// 硬件页表。调用方负责管理映射对应的物理页生命周期。
///
/// # TLB
///
/// 不带 `_no_flush` 的修改接口必须刷新当前 hart/core 上对应的 TLB 条目。
/// 原始接口不会刷新，只能由 `UserMapper` 在 `MmuGather` 保护下使用。
pub trait PageTable {
    /// 将 `vpn` 映射到 `ppn`。
    ///
    /// # Errors
    ///
    /// 返回 `MemoryError::AlreadyMapped` 表示目标 VPN 已存在有效映射。
    /// 页表页分配失败时返回对应的 `MemoryError`。
    ///
    /// # TLB
    ///
    /// 成功写入 PTE 后必须刷新该 VPN 对应的 TLB 条目。
    fn try_map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> Result<(), MemoryError>;
    /// 建立映射但不刷新 TLB；仅供 `UserMapper` 使用。
    fn try_map_no_flush(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> Result<(), MemoryError>;
    fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.try_map(vpn, ppn, flags).unwrap();
    }
    #[inline(always)]
    fn map_identical(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.map(vpn, ppn, flags)
    }
    /// 建立一个 2 MiB 的恒等映射。
    ///
    /// 默认实现保留 4 KiB 语义；SV39 覆盖此入口以使用二级叶子 PTE。
    fn try_map_identical_2m(
        &mut self,
        start_vpn: VirtPageNum,
        flags: MapPermission,
    ) -> Result<(), MemoryError> {
        const PAGES_PER_2M: usize = 512;
        for offset in 0..PAGES_PER_2M {
            let vpn = VirtPageNum(start_vpn.0 + offset);
            self.try_map(vpn, PhysPageNum(vpn.0), flags)?;
        }
        Ok(())
    }
    /// Unmap the `vpn` to `ppn` with the `flags`.
    ///
    /// # Panics
    ///
    /// Panics if the `vpn` is not currently mapped.
    ///
    /// # TLB
    ///
    /// 成功清除 PTE 后必须刷新该 VPN 对应的 TLB 条目。
    #[allow(unused)]
    fn unmap(&mut self, vpn: VirtPageNum);
    /// 清除映射但不刷新 TLB；仅供 `UserMapper` 使用。
    fn unmap_no_flush(&mut self, vpn: VirtPageNum);
    #[inline(always)]
    fn unmap_identical(&mut self, vpn: VirtPageNum) {
        self.unmap(vpn)
    }
    /// Translate the `vpn` into its corresponding `Some(PageTableEntry)` if exists
    /// `None` is returned if nothing is found.
    fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum>;
    /// Translate the virtual address into its corresponding `PhysAddr` if mapped in current page table.
    /// `None` is returned if nothing is found.
    fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr>;
    /// 撤销 `vpn` 的写权限并返回原映射的物理页。
    ///
    /// # TLB
    ///
    /// 成功修改 PTE 后必须立即刷新该 VPN，保证后续写访问重新缺页。
    fn block_and_ret_mut(&mut self, vpn: VirtPageNum) -> Option<PhysPageNum>;

    /// 撤销写权限并返回 PPN，但不刷新 TLB；仅供 `UserMapper` 使用。
    fn block_and_ret_mut_no_flush(&mut self, vpn: VirtPageNum) -> Option<PhysPageNum>;

    /// 刷新当前 CPU 上一个虚拟页对应的 TLB 条目。
    ///
    /// 架构若无法从裸页表对象定位外层地址空间 ASID，可以保守地清除本核全部
    /// non-global 项，但不能缩小到错误的当前 ASID。
    fn flush_tlb_page(&self, vpn: VirtPageNum);

    /// 刷新当前页表相关的 TLB 状态。
    ///
    /// # Semantics
    ///
    /// 用于 `_no_flush` 批量修改完成后的收尾，也可用于架构无法精确单页刷新时
    /// 执行全量刷新。
    fn flush_tlb(&self);

    /// Return the physical token to current page.
    fn token(&self) -> usize;

    /// 撤销 `vpn` 的读权限。
    ///
    /// # TLB
    ///
    /// 成功修改 PTE 后必须刷新该 VPN。
    fn revoke_read(&mut self, vpn: VirtPageNum) -> Result<(), ()>;

    /// 撤销 `vpn` 的写权限。
    ///
    /// # TLB
    ///
    /// 成功修改 PTE 后必须刷新该 VPN。
    fn revoke_write(&mut self, vpn: VirtPageNum) -> Result<(), ()>;

    /// 撤销 `vpn` 的执行权限。
    ///
    /// # TLB
    ///
    /// 成功修改 PTE 后必须刷新该 VPN。
    fn revoke_execute(&mut self, vpn: VirtPageNum) -> Result<(), ()>;

    /// 修改 `vpn` 指向的物理页。
    ///
    /// # TLB
    ///
    /// 成功修改 PPN 后必须刷新该 VPN。
    fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> Result<(), ()>;
    /// 修改 PPN 但不刷新 TLB；仅供 `UserMapper` 使用。
    fn set_ppn_no_flush(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> Result<(), ()>;

    /// 覆盖 `vpn` 的 PTE 权限位。
    ///
    /// # TLB
    ///
    /// 成功修改权限后必须刷新该 VPN。
    fn set_pte_flags(&mut self, vpn: VirtPageNum, flags: MapPermission) -> Result<(), ()>;
    /// 修改权限但不刷新 TLB；仅供 `UserMapper` 使用。
    fn set_pte_flags_no_flush(&mut self, vpn: VirtPageNum, flags: MapPermission) -> Result<(), ()>;

    /// 设置硬件 dirty 位但不刷新 TLB；仅用于统一 LA64 的软件 dirty fault 路径。
    fn set_dirty_bit_no_flush(&mut self, vpn: VirtPageNum) -> Result<(), ()>;

    /// 清除硬件访问位。
    ///
    /// # TLB
    ///
    /// 成功修改 PTE 后必须刷新该 VPN 或执行等价的全量刷新。
    fn clear_access_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()>;

    /// 清除硬件 dirty 位。
    ///
    /// # TLB
    ///
    /// 成功修改 PTE 后必须刷新该 VPN 或执行等价的全量刷新。
    fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()>;

    /// 创建空页表。
    fn new() -> Self;
    #[inline(always)]
    fn new_kern_space() -> Self
    where
        Self: Sized,
    {
        Self::new()
    }
    /// 移出页表自身持有的全部页表页，但暂不释放。
    ///
    /// zombie MM 会把这些 frame 和叶子映射 frame 一起交给 TLB retirement；
    /// 只有远端 ack 完成后，返回的 Vec 才能被销毁。
    fn take_frames(&mut self) -> Vec<Arc<FrameTracker>>;
    /// 从硬件页表 token 构造页表视图。
    ///
    /// # Semantics
    ///
    /// 该方法不接管页表页所有权，通常用于临时遍历当前用户页表。
    fn from_token(satp: usize) -> Self;
    /// Predicate for the valid bit.
    fn is_mapped(&self, vpn: VirtPageNum) -> bool;
    fn activate(&self);
    fn is_valid(&self, vpn: VirtPageNum) -> Option<bool>;
    fn is_dirty(&self, vpn: VirtPageNum) -> Option<bool>;
    fn readable(&self, vpn: VirtPageNum) -> Option<bool>;
    fn writable(&self, vpn: VirtPageNum) -> Option<bool>;
    fn executable(&self, vpn: VirtPageNum) -> Option<bool>;
    /// 只检查 PTE 用户位和请求的读写权限。
    ///
    /// # Semantics
    ///
    /// 这是非 faulting 探测，不会分配页表页、触发 CoW 或填充 lazy 映射。
    fn user_access_ok(&self, vpn: VirtPageNum, access: UserAccess) -> Option<bool>;
}

#[allow(unused)]
pub fn gen_start_end(start: VirtAddr, end: VirtAddr) -> (VirtPageNum, VirtPageNum) {
    (start.floor(), end.ceil())
}

// 老工具函数先留着
pub fn ptf_ok(ptf: usize) -> bool {
    ptf & 1 == 1
}
