//! LoongArch64 基础控制 CSR。
//!
//! 这些寄存器描述当前特权级、例外入口/返回地址、中断配置、CPU 标识和
//! 实现相关能力，是 trap 初始化与异常分发路径的基础状态来源。

#![allow(unused)]

pub mod badi;
pub mod badv;
pub mod cpuid;
pub mod crmd;
pub mod ecfg;
pub mod eentry;
pub mod era;
pub mod estat;
pub mod euen;
pub mod llbctl;
pub mod misc;
pub mod prcfg;
pub mod prmd;
pub mod rvacfg;
