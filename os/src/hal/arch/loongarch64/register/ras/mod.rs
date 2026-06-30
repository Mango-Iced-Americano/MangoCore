//! LoongArch64 RAS machine-error CSR。
//!
//! 这些寄存器为 machine error 例外提供独立入口、返回地址、现场保存和错误信息，
//! 避免普通例外 CSR 在不可预测错误时被覆盖。

#![allow(unused)]
pub mod merrctl;
pub mod merrentry;
pub mod merrera;
pub mod merrinfo;
pub mod merrsave;
