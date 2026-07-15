//! 用户态信号帧布局计算。
//!
//! 信号投递前在目标用户栈上预留 `UserContext` 和 `SigInfo`，本模块只做地址
//! 和边界计算，实际 copy_to_user 由 `signal::do_signal` 完成。

use core::mem::{align_of, size_of};

use crate::hal::UserContext;
use crate::mm::USER_STACK_ABI_ALIGN;

use super::SigInfo;

/// 计算信号帧在用户栈上的布局。
///
/// # Semantics
///
/// 返回 `(ucontext_addr, siginfo_addr, sig_sp, sig_size)`。`ucontext_addr` 按
/// 架构上下文的自然对齐要求对齐，且传给用户处理函数的栈指针满足 ABI 对齐，
/// 且 `siginfo_addr` 不得低于调用方给出的 `stack_bottom`。
///
/// # Errors
///
/// 返回 `None` 表示减法溢出或选中的栈空间不足，调用方应按信号栈溢出处理。
pub(super) fn signal_frame_layout(
    base_sp: usize,
    stack_bottom: usize,
) -> Option<(usize, usize, usize, usize)> {
    let context_align = align_of::<UserContext>().max(USER_STACK_ABI_ALIGN);
    debug_assert!(context_align.is_power_of_two());
    let ucontext_addr = base_sp.checked_sub(size_of::<UserContext>())? & !(context_align - 1);
    let siginfo_addr =
        ucontext_addr.checked_sub(size_of::<SigInfo>())? & !(USER_STACK_ABI_ALIGN - 1);
    if siginfo_addr < stack_bottom {
        return None;
    }
    let sig_sp = siginfo_addr;
    let sig_size = sig_sp - stack_bottom;
    Some((ucontext_addr, siginfo_addr, sig_sp, sig_size))
}
