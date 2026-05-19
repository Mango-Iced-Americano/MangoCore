use core::mem::size_of;

use crate::hal::UserContext;

use super::SigInfo;

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
