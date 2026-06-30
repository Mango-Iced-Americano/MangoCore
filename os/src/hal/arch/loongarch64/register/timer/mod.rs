//! LoongArch64 定时器与稳定计数器 CSR。
//!
//! 这些寄存器配置 timer 中断、读取倒计时值，并管理 stable counter 的补偿和标识信息。

#![allow(unused)]
pub mod cntc;
pub mod tcfg;
pub mod ticlr;
pub mod tid;
pub mod tval;
