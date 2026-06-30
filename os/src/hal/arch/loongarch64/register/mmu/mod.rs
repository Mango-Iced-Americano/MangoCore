//! LoongArch64 MMU 相关 CSR。
//!
//! 本模块覆盖页表根、直接映射窗口、TLB 表项和 TLB refill 专用寄存器。
//! 这些 wrapper 被页表初始化、TLB 操作和地址异常处理路径共同使用。

#![allow(unused)]

use core::fmt::write;
pub mod asid;
pub mod dmw;
pub mod pgd;
pub mod pwch;
pub mod pwcl;
pub mod stlbps;
pub mod tlbehi;
pub mod tlbelo;
pub mod tlbidx;
pub mod tlbrbadv;
pub mod tlbrehi;
pub mod tlbrelo;
pub mod tlbrentry;
pub mod tlbrera;
pub mod tlbrprmd;
pub mod tlbrsave;

/// LoongArch64 页表、TLB 和直接映射窗口使用的内存访问类型（MAT）。
#[derive(Debug, Eq, PartialEq, num_enum::TryFromPrimitive, num_enum::IntoPrimitive)]
#[repr(usize)]
pub enum MemoryAccessType {
    /// 强序非缓存访问，通常用于设备或需要严格顺序的内存区域。
    StronglyOrderedUnCached = 0,
    /// 一致性缓存访问，内核普通内存映射通常使用该类型。
    CoherentCached = 1,
    /// 弱序非缓存访问。
    WeaklyOrderedUnCached = 2,
}
impl core::fmt::Display for MemoryAccessType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self)
    }
}
