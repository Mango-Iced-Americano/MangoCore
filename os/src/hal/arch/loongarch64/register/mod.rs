//! LoongArch64 控制状态寄存器（CSR）的类型化访问入口。
//!
//! 本模块按基础控制、MMU、RAS 和定时器寄存器分组，并向 HAL 的陷入、
//! 地址转换和时钟路径导出当前内核实际使用的 CSR wrapper。

#[macro_use]
mod csr_macros;
mod base;
pub mod mmu;
mod ras;
mod timer;
pub use base::{
    badi::*, badv::*, 
    cpuid::*, 
    crmd::*, ecfg::*, eentry::*, era::*, estat::*, euen::*, 
    // llbctl::*,
    misc::*, prcfg::*, prmd::*, rvacfg::*,
};
pub use mmu::{
    asid::*, dmw::*, pgd::*, pwch::*, pwcl::*, stlbps::*, tlbehi::*, tlbelo::*, tlbidx::*,
    tlbrbadv::*, tlbrehi::*, tlbrelo::*, tlbrentry::*, tlbrera::*, tlbrprmd::*, 
    // tlbrsave::*,
    MemoryAccessType,
};
pub use ras::{
    merrctl::*, merrentry::*, merrera::*, 
    // merrinfo::*, merrsave::*
};
pub use timer::{
    // cntc::*, 
    tcfg::*, ticlr::*, 
    // tid::*, tval::*
};
