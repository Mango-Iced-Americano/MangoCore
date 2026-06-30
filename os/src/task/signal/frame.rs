//! 用户态信号帧布局计算。
//!
//! 信号投递前在目标用户栈上预留 `UserContext` 和 `SigInfo`，本模块只做地址
//! 和边界计算，实际 copy_to_user 由 `signal::do_signal` 完成。

use core::mem::size_of;

use crate::hal::UserContext;

use super::SigInfo;

/// 计算信号帧在用户栈上的布局。
///
/// # Semantics
///
/// 返回 `(ucontext_addr, siginfo_addr, sig_sp, sig_size)`。地址按 8 字节对齐，
/// 且 `siginfo_addr` 不得低于调用方给出的 `stack_bottom`。
///
/// # Errors
///
/// 返回 `None` 表示减法溢出或选中的栈空间不足，调用方应按信号栈溢出处理。
pub(super) fn signal_frame_layout(
    base_sp: usize,
    stack_bottom: usize,
) -> Option<(usize, usize, usize, usize)> {
    let ucontext_addr = base_sp.checked_sub(size_of::<UserContext>())? & !0x7;
    let siginfo_addr = ucontext_addr.checked_sub(size_of::<SigInfo>())? & !0x7;
    if siginfo_addr < stack_bottom {
        return None;
    }
    let sig_sp = siginfo_addr;
    let sig_size = sig_sp - stack_bottom;
    Some((ucontext_addr, siginfo_addr, sig_sp, sig_size))
}
