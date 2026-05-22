use crate::hal::{
    get_bad_addr, get_bad_instruction, get_exception_cause, MachineContext, TrapContext,
    UserContext, UserSignalMask,
};
use crate::signal_type;
use core::fmt::{self, Debug, Formatter};
use core::mem::size_of;
use log::{debug, error, trace, warn};

use crate::config::*;
use crate::mm::{UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    exit_current_and_run_next, exit_group_and_run_next, WaitQueue,
};

use super::current_task;
use super::task::TaskControlBlock;
use crate::utils::error::SyscallErr;

mod action;
mod delivery;
mod frame;
mod pending;
mod wait;

pub use action::Sighand;
pub use delivery::{send_process_signal, send_process_signal_info, send_thread_signal};
use frame::signal_frame_layout;
pub use pending::{PendingSignal, SignalQueue};
pub use wait::{sigsuspend, sigtimedwait};

bitflags! {
    /// Signals 枚举
    pub struct Signals: signal_type!(){
        /// Hangup.
        const	SIGHUP		= 1 << ( 0);
        /// Interactive attention signal.
        const	SIGINT		= 1 << ( 1);
        /// Quit.
        const	SIGQUIT		= 1 << ( 2);
        /// Illegal instruction.
        const	SIGILL		= 1 << ( 3);
        /// Trace/breakpoint trap.
        const	SIGTRAP		= 1 << ( 4);
        /// IOT instruction, abort() on a PDP-11.
        const	SIGABRT		= 1 << ( 5);
        /// Bus error.
        const	SIGBUS		= 1 << ( 6);
        /// Erroneous arithmetic operation.
        const	SIGFPE		= 1 << ( 7);
        /// Killed.
        const	SIGKILL		= 1 << ( 8);
        /// User-defined signal 1.
        const	SIGUSR1		= 1 << ( 9);
        /// Invalid access to storage.
        const	SIGSEGV		= 1 << (10);
        /// User-defined signal 2.
        const	SIGUSR2		= 1 << (11);
        /// Broken pipe.
        const	SIGPIPE		= 1 << (12);
        /// Alarm clock.
        const	SIGALRM		= 1 << (13);
        /// Termination request.
        const	SIGTERM		= 1 << (14);
        const	SIGSTKFLT	= 1 << (15);
        /// Child terminated or stopped.
        const	SIGCHLD		= 1 << (16);
        /// Continue.
        const	SIGCONT		= 1 << (17);
        /// Stop, unblockable.
        const	SIGSTOP		= 1 << (18);
        /// Keyboard stop.
        const	SIGTSTP		= 1 << (19);
        /// Background read from control terminal.
        const	SIGTTIN		= 1 << (20);
        /// Background write to control terminal.
        const	SIGTTOU		= 1 << (21);
        /// Urgent data is available at a socket.
        const	SIGURG		= 1 << (22);
        /// CPU time limit exceeded.
        const	SIGXCPU		= 1 << (23);
        /// File size limit exceeded.
        const	SIGXFSZ		= 1 << (24);
        /// Virtual timer expired.
        const	SIGVTALRM	= 1 << (25);
        /// Profiling timer expired.
        const	SIGPROF		= 1 << (26);
        /// Window size change (4.3 BSD, Sun).
        const	SIGWINCH	= 1 << (27);
        /// I/O now possible (4.2 BSD).
        const	SIGIO		= 1 << (28);
        const   SIGPWR      = 1 << (29);
        /// Bad system call.
        const   SIGSYS      = 1 << (30);
        /* --- realtime signals for pthread --- */
        const   SIGTIMER    = 1 << (31);
        const   SIGCANCEL   = 1 << (32);
        const   SIGSYNCCALL = 1 << (33);
        /* --- other realtime signals --- */
        const   SIGRT_3     = 1 << (34);
        const   SIGRT_4     = 1 << (35);
        const   SIGRT_5     = 1 << (36);
        const   SIGRT_6     = 1 << (37);
        const   SIGRT_7     = 1 << (38);
        const   SIGRT_8     = 1 << (39);
        const   SIGRT_9     = 1 << (40);
        const   SIGRT_10    = 1 << (41);
        const   SIGRT_11    = 1 << (42);
        const   SIGRT_12    = 1 << (43);
        const   SIGRT_13    = 1 << (44);
        const   SIGRT_14    = 1 << (45);
        const   SIGRT_15    = 1 << (46);
        const   SIGRT_16    = 1 << (47);
        const   SIGRT_17    = 1 << (48);
        const   SIGRT_18    = 1 << (49);
        const   SIGRT_19    = 1 << (50);
        const   SIGRT_20    = 1 << (51);
        const   SIGRT_21    = 1 << (52);
        const   SIGRT_22    = 1 << (53);
        const   SIGRT_23    = 1 << (54);
        const   SIGRT_24    = 1 << (55);
        const   SIGRT_25    = 1 << (56);
        const   SIGRT_26    = 1 << (57);
        const   SIGRT_27    = 1 << (58);
        const   SIGRT_28    = 1 << (59);
        const   SIGRT_29    = 1 << (60);
        const   SIGRT_30    = 1 << (61);
        const   SIGRT_31    = 1 << (62);
        const   SIGRTMAX    = 1 << (63);
    }
}

const SYSCALL_SIGTIMEDWAIT: usize = 137;
const SYSCALL_RT_SIGSUSPEND: usize = 133;

impl Signals {
    // SIGILL | SIGKILL | SIGSEGV | SIGSTOP
    /// 不能被处理的信号
    pub const CAN_NOT_BE_MASKED: Signals =
        Signals::from_bits_truncate(1 << 3 | 1 << 8 | 1 << 10 | 1 << 18);
    const EMPTY: Signals = Signals::empty();
    /// if 0 <= signum < 64, return `Ok(Signals)`, else return `Err()` (illeagal)
    pub fn from_signum(signum: usize) -> Result<Signals, ()> {
        match signum {
            0 => Ok(Signals::EMPTY),
            1..=64 => Ok(Signals::from_bits_truncate(1 << (signum - 1))),
            _ => Err(()),
        }
    }
    pub fn to_signum(&self) -> Result<usize, ()> {
        if self.bits().count_ones() == 1 {
            Ok(self.bits().trailing_zeros() as usize + 1)
        } else {
            Err(())
        }
    }
    /// Returns rightmost signal's signum if self is not empty.
    pub fn peek_front(&self) -> Option<usize> {
        if self.is_empty() {
            None
        } else {
            Some(self.bits().trailing_zeros() as usize + 1)
        }
    }
}

bitflags! {
    /// Bits in `sa_flags' used to denote the default signal action.
    /// 信号处理标志
    pub struct SigActionFlags: usize{
    /// Don't send SIGCHLD when children stop.
        const SA_NOCLDSTOP = 1		   ;
    /// Don't create zombie on child death.
        const SA_NOCLDWAIT = 2		   ;
    /// Invoke signal-catching function with three arguments instead of one.
        const SA_SIGINFO   = 4		   ;
    /// Use signal stack by using `sa_restorer'.
        const SA_ONSTACK   = 0x08000000;
    /// Restart syscall on signal return.
        const SA_RESTART   = 0x10000000;
    /// Don't automatically block the signal when its handler is being executed.
        const SA_NODEFER   = 0x40000000;
    /// Reset to SIG_DFL on entry to handler.
        const SA_RESETHAND = 0x80000000;
    /// Historical no-op.
        const SA_INTERRUPT = 0x20000000;
    /// Use signal trampoline provided by C library's wrapper function.
        const SA_RESTORER  = 0x04000000;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
/// 信号处理函数类型
pub struct SigHandler(usize);

impl SigHandler {
    /// 默认处理
    pub(super) const SIG_DFL: Self = Self(0);
    /// 忽略信号
    pub(super) const SIG_IGN: Self = Self(1);
    fn addr(&self) -> Option<usize> {
        match *self {
            Self::SIG_DFL | Self::SIG_IGN => None,
            sig_handler => Some(sig_handler.0),
        }
    }
}

impl Debug for SigHandler {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            SigHandler::SIG_DFL => f.write_fmt(format_args!("SIG_DFL")),
            SigHandler::SIG_IGN => f.write_fmt(format_args!("SIG_IGN")),
            sig_handler => f.write_fmt(format_args!("0x{:X}", sig_handler.0)),
        }
    }
}

#[cfg(feature = "loongarch64")]
#[derive(Clone, Copy)]
#[repr(C)]
/// 信号处理动作
pub struct SigAction {
    /// 信号处理函数
    pub handler: SigHandler,
    /// 信号处理标志位
    pub flags: SigActionFlags,
    /// 恢复函数地址。la64 rt_sigaction 使用 kernel k_sigaction 布局：
    /// handler, flags, restorer, mask。
    pub restorer: usize,
    /// 要屏蔽的信号
    pub mask: Signals,
}

#[cfg(feature = "riscv")]
#[derive(Clone, Copy)]
#[repr(C)]
pub struct SigAction {
    pub handler: SigHandler,
    pub flags: SigActionFlags,
    pub restorer: usize,
    pub mask: Signals,
}

#[derive(Clone, Copy)]
#[repr(C)]
/// rt_sigaction 用户 ABI 结构。Linux k_sigaction 只传递低 64 位 signal mask；
/// la64 内部 Signals 是 128 位，不能直接 copy_to_user，否则会覆盖用户栈。
pub struct UserSigAction {
    pub handler: SigHandler,
    pub flags: SigActionFlags,
    pub restorer: usize,
    pub mask: u64,
}

impl SigAction {
    pub fn new() -> Self {
        Self {
            handler: SigHandler::SIG_DFL,
            flags: SigActionFlags::empty(),
            restorer: 0,
            mask: Signals::empty(),
        }
    }
}

impl UserSigAction {
    fn from_kernel(action: SigAction) -> Self {
        Self {
            handler: action.handler,
            flags: action.flags,
            restorer: action.restorer,
            mask: action.mask.bits() as u64,
        }
    }

    fn into_kernel(self) -> SigAction {
        SigAction {
            handler: self.handler,
            flags: self.flags,
            restorer: self.restorer,
            mask: Signals::from_bits_truncate(self.mask as signal_type!()),
        }
    }
}

impl Debug for SigAction {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "[ sa_handler: {:?}, sa_mask: ({:?}), sa_flags: ({:?}) ]",
            self.handler, self.mask, self.flags
        ))
    }
}

/// Change the action taken by a process on receipt of a specific signal.
/// (See signal(7) for  an  overview of signals.)
/// # Fields in Structure of `act` & `oldact`
///
/// # Arguments
/// * `signum`: specifies the signal and can be any valid signal except `SIGKILL` and `SIGSTOP`.
/// * `act`: new action
/// * `oldact`: old action
/// 此函数与RV版本略有不同，但是可以不处理，因为此版本的这个函数鲁棒性更强
pub fn sigaction(signum: usize, act: *const UserSigAction, oldact: *mut UserSigAction) -> isize {
    let task = current_task().unwrap();
    match signum {
        0 /* None */ | 9 /* SIGKILL */ | 19 /* SIGSTOP */ | 65.. /* Unsupported */ => {
            warn!("[sigaction] bad signum: {}", signum);
            EINVAL
        }
        signum => {
            trace!("[sigaction] signal: {:?}", Signals::from_signum(signum));
            let token = task.get_user_token();
            if !oldact.is_null() {
                let sighand_ref = task.process.sighand();
                let sighand = sighand_ref.lock();
                let suc = if let Some(sigact) = sighand.get(signum) {
                    trace!("[sigaction] *oldact: {:?}", sigact);
                    UserPtrMut::new(oldact).write(token, &UserSigAction::from_kernel(*sigact))
                } else {
                    trace!("[sigaction] *oldact: not found");
                    UserPtrMut::new(oldact)
                        .write(token, &UserSigAction::from_kernel(SigAction::new()))
                };
                if suc.is_err() {
                    log::error!("[sigaction] Error on copy_to_user(_,{:?},_)", oldact);
                    return EFAULT;
                }
            }
            if let Some(mut sigact) = match UserPtr::new(act).read_optional(token) {
                Ok(sigact) => sigact.map(UserSigAction::into_kernel),
                Err(_) => {
                    log::error!("[sigaction] Failed to copy sigact {:?} from user.", act);
                    return EFAULT;
                }
            } {
                sigact.mask.remove(Signals::CAN_NOT_BE_MASKED);
                let sighand_ref = task.process.sighand();
                let mut sighand = sighand_ref.lock();
                if sigact.handler == SigHandler::SIG_IGN {
                    // Store SIG_IGN explicitly so we can distinguish from SIG_DFL
                    sighand.set(signum, Some(sigact));
                } else if sigact.handler == SigHandler::SIG_DFL {
                    sighand.set(signum, None);
                } else {
                    sighand.set(signum, Some(sigact));
                }
                trace!("[sigaction] *act: {:?}", sigact);
            }
            SUCCESS
        }
    }
}

bitflags! {
    pub struct SignalStackFlags : u32 {
        const ONSTACK = 1;
        const DISABLE = 2;
        const AUTODISARM = 0x80000000;
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct SignalStack {
    pub sp: usize,
    pub flags: u32,
    pub size: usize,
}

impl SignalStack {
    #[cfg(feature = "riscv")]
    pub const MIN_SIZE: usize = 2048;
    #[cfg(feature = "loongarch64")]
    pub const MIN_SIZE: usize = 4096;

    pub const fn disabled() -> Self {
        SignalStack {
            sp: 0,
            flags: SignalStackFlags::DISABLE.bits,
            size: 0,
        }
    }

    fn new(sp: usize, size: usize) -> Self {
        SignalStack {
            sp,
            flags: SignalStackFlags::DISABLE.bits,
            size,
        }
    }

    pub fn is_disabled(&self) -> bool {
        SignalStackFlags::from_bits_truncate(self.flags).contains(SignalStackFlags::DISABLE)
    }

    pub fn contains_sp(&self, sp: usize) -> bool {
        if self.is_disabled() {
            return false;
        }
        match self.sp.checked_add(self.size) {
            Some(end) => self.sp <= sp && sp < end,
            None => false,
        }
    }

    pub fn top(&self) -> Option<usize> {
        if self.is_disabled() {
            None
        } else {
            self.sp.checked_add(self.size)
        }
    }

    pub fn with_runtime_flags(mut self, current_sp: usize) -> Self {
        if self.is_disabled() {
            self.flags = SignalStackFlags::DISABLE.bits;
        } else if self.contains_sp(current_sp) {
            self.flags = SignalStackFlags::ONSTACK.bits;
        } else {
            self.flags &= !SignalStackFlags::ONSTACK.bits;
            self.flags &= !SignalStackFlags::DISABLE.bits;
        }
        self
    }
}

const WAIT_COREDUMP: u32 = 0x80;

fn default_signal_wait_status(signal: Signals) -> u32 {
    let signum = signal.to_signum().unwrap() as u32;
    // Linux wait status uses bit 7 to report WCOREDUMP(status).
    if matches!(signum, 3 | 4 | 5 | 6 | 7 | 8 | 11 | 24 | 25 | 31) {
        signum | WAIT_COREDUMP
    } else {
        signum
    }
}

fn exit_current_with_sigsegv() -> ! {
    exit_current_and_run_next(default_signal_wait_status(Signals::SIGSEGV));
}

/// Signals whose SIG_DFL action is to ignore the signal.
/// These signals should NOT cause EINTR in pselect/ppoll/wait/etc.
pub(super) const SIG_DFL_IGNORE: Signals = Signals::from_bits_truncate(
    Signals::SIGCHLD.bits()
        | Signals::SIGCONT.bits()
        | Signals::SIGURG.bits()
        | Signals::SIGWINCH.bits(),
);

fn pending_unblocked_signals(task: &TaskControlBlock) -> Signals {
    let inner = task.acquire_inner_lock();
    let sigmask = inner.sigmask;
    let pending = inner.sigpending.pending() | task.process.shared_pending();
    pending.difference(sigmask)
}

fn signal_is_actionable(sighand: &Sighand, signum: usize, signal: Signals) -> bool {
    match sighand.get(signum) {
        Some(act) => act.handler != SigHandler::SIG_IGN,
        None => !SIG_DFL_IGNORE.contains(signal),
    }
}

fn take_next_pending_signal(
    task: &TaskControlBlock,
    inner: &mut super::task::TaskControlBlockInner,
) -> Option<(PendingSignal, bool)> {
    let thread_pending = inner.sigpending.pending().difference(inner.sigmask);
    if let Some(pending) = inner.sigpending.dequeue_matching(thread_pending) {
        return Some((pending, false));
    }

    let shared_pending = task.process.shared_pending().difference(inner.sigmask);
    task.process
        .take_shared_matching(shared_pending)
        .map(|pending| (pending, true))
}

pub fn discard_non_actionable_unblocked_signals(task: &TaskControlBlock) {
    let (thread_pending, sigmask) = {
        let inner = task.acquire_inner_lock();
        (
            inner.sigpending.pending().difference(inner.sigmask),
            inner.sigmask,
        )
    };
    let shared_pending = task.process.shared_pending().difference(sigmask);
    let mut discard_thread = Signals::empty();
    let mut discard_shared = Signals::empty();
    let sighand_ref = task.process.sighand();
    let sighand = sighand_ref.lock();
    for signum in 1..=64usize {
        let signal = match Signals::from_signum(signum) {
            Ok(signal) => signal,
            Err(_) => continue,
        };
        if !signal_is_actionable(&sighand, signum, signal) {
            if thread_pending.contains(signal) {
                discard_thread.insert(signal);
            }
            if shared_pending.contains(signal) {
                discard_shared.insert(signal);
            }
        }
    }
    drop(sighand);

    if !discard_thread.is_empty() {
        task.acquire_inner_lock()
            .sigpending
            .remove_signals(discard_thread);
    }
    for signum in 1..=64usize {
        if let Ok(signal) = Signals::from_signum(signum) {
            if discard_shared.contains(signal) {
                task.process.take_shared_signal(signal);
            }
        }
    }
}

/// Check whether any pending-unblocked signal has an actionable disposition.
/// A signal is actionable only if:
///   - it has a user-registered custom handler, OR
///   - its SIG_DFL action is NOT "ignore" (i.e. not SIGCHLD/SIGCONT/SIGURG/SIGWINCH)
/// Signals with SIG_IGN disposition are NOT actionable.
///
/// This function is used by pselect/ppoll/wait_io_core/has_unblocked_signal etc.
/// to decide whether a pending signal should trigger EINTR.
pub fn has_actionable_signal(task: &TaskControlBlock) -> bool {
    let pending = pending_unblocked_signals(task);
    if pending.is_empty() {
        return false;
    }
    log::debug!(
        "Task tid {} pid {} has pending: {:x}, mask: {:x}",
        task.tid.0,
        task.pid(),
        pending.bits(),
        task.acquire_inner_lock().sigmask.bits()
    );
    let sighand_ref = task.process.sighand();
    let sighand = sighand_ref.lock();
    for signum in 1..=64usize {
        let signal_bit = match Signals::from_signum(signum) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !pending.contains(signal_bit) {
            continue;
        }
        if signal_is_actionable(&sighand, signum, signal_bit) {
            return true;
        }
    }
    false
}

fn wait_for_default_stop_signal() {
    let wait_queue = spin::Mutex::new(WaitQueue::new());
    let _ = WaitQueue::wait_event_interruptible(&wait_queue, || {
        let task = current_task()?;
        if pending_unblocked_signals(&task).contains(Signals::SIGCONT) {
            Some(0)
        } else {
            None
        }
    });
}

/// 执行信号处理
/// 在从内核返回到用户空间前调用
pub fn do_signal() {
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    while let Some((pending, from_process)) = take_next_pending_signal(&task, &mut inner) {
        let signum = pending.signum();
        let signal = pending.signal;
        trace!(
            "[do_signal] signal: {:?}, from_process: {}, thread_pending: {:?}, process_pending: {:?}, sigmask: {:?}",
            signal,
            from_process,
            inner.sigpending.pending(),
            task.process.shared_pending(),
            inner.sigmask
        );
        let sighand_ref = task.process.sighand();
        let mut sighand = sighand_ref.lock();
        // user-defined handler
        if let Some(act) = sighand.get(signum).copied() {
            // SIG_IGN → discard this signal (POSIX: ignored signals are not delivered)
            if act.handler == SigHandler::SIG_IGN {
                trace!("[do_signal] Ignore {:?} (SIG_IGN)", signal);
                continue;
            }
            {
                let trap_cx = inner.get_trap_cx();
                let a0_isize = trap_cx.gp.a0 as isize;
                // if this syscall wants to restart
                if a0_isize == -(SyscallErr::ERESTART as isize) {
                    // and if `SA_RESTART` is set
                    if act.flags.contains(SigActionFlags::SA_RESTART)
                        && trap_cx.gp.a7 != SYSCALL_SIGTIMEDWAIT
                        && trap_cx.gp.a7 != SYSCALL_RT_SIGSUSPEND
                    {
                        debug!("[do_signal] syscall will restart after sigreturn");
                        // back to `ecall`
                        trap_cx.gp.pc -= 4;
                        // restore syscall parameter `a0`
                        trap_cx.gp.a0 = trap_cx.origin_a0;
                    } else {
                        debug!("[do_signal] syscall was interrupted");
                        // will return EINTR after sigreturn
                        trap_cx.gp.a0 = EINTR as usize;
                    }
                }
            }
            let current_sp = inner.get_trap_cx().gp.sp;
            let alt_stack = inner.signal_stack;
            let use_alt_stack = act.flags.contains(SigActionFlags::SA_ONSTACK)
                && !alt_stack.is_disabled()
                && !alt_stack.contains_sp(current_sp);
            let default_stack_top = task.ustack_bottom_va();
            let default_stack_bottom = default_stack_top - USER_STACK_SIZE;
            let normal_stack_bottom = if current_sp > default_stack_bottom
                && current_sp <= default_stack_top
            {
                // execve 后会重新分配默认用户栈；vfork/clone 旧的 ustack_base
                // 可能仍是自定义栈指针，因此按当前 sp 所在栈槽重新判定边界。
                default_stack_bottom
            } else {
                task.ustack_base.saturating_sub(USER_STACK_SIZE)
            };
            let (frame_base_sp, stack_bottom) = if use_alt_stack {
                match alt_stack.top() {
                    Some(top) => (top, alt_stack.sp),
                    None => (current_sp, normal_stack_bottom),
                }
            } else {
                (current_sp, normal_stack_bottom)
            };
            // check if we have enough space on selected user stack
            if let Some((ucontext_addr, siginfo_addr, sig_sp, sig_size)) =
                signal_frame_layout(frame_base_sp, stack_bottom)
            {
                let token = task.get_user_token();
                let saved_sigmask = inner.sigmask_to_restore.take().unwrap_or(inner.sigmask);
                let mcontext =
                    unsafe { *(inner.get_trap_cx() as *const TrapContext).cast::<MachineContext>() };
                let mut frame_stack = if use_alt_stack {
                    alt_stack.with_runtime_flags(sig_sp)
                } else {
                    SignalStack::new(sig_sp, sig_size)
                };
                if use_alt_stack {
                    frame_stack.flags = SignalStackFlags::ONSTACK.bits;
                }
                // In this case, signal hander have three parameters
                if act.flags.contains(SigActionFlags::SA_SIGINFO) {
                    let user_context =
                        UserContext::new(0, 0, frame_stack, saved_sigmask, mcontext);
                    if UserPtrMut::from_addr(ucontext_addr)
                        .write(token, &user_context) // push UserContext into user stack
                        .is_err()
                    {
                        error!("[do_signal] Failed to write UserContext to user stack. Send SIGSEGV.");
                        drop(inner);
                        drop(sighand);
                        drop(task);
                        exit_current_with_sigsegv();
                    }
                    if UserPtrMut::from_addr(siginfo_addr)
                        .write(token, &pending.siginfo) // push SigInfo into user stack
                        .is_err()
                    {
                        error!("[do_signal] Failed to write SigInfo to user stack. Send SIGSEGV.");
                        drop(inner);
                        drop(sighand);
                        drop(task);
                        exit_current_with_sigsegv();
                    }
                    let trap_cx = inner.get_trap_cx();
                    trap_cx.gp.a2 = ucontext_addr; // a2 <- *UserContext
                    trap_cx.gp.a1 = siginfo_addr; // a1 <- *SigInfo
                                                  // In this case, signal handler only have one parameter (a0 <- signum), so only copy something necessary
                                                  // To simplify the implementation of sigreturn, here we keep the same layout as above...
                } else {
                    // push sigmask into user stack
                    let user_sigmask = UserContext::encode_sigmask(saved_sigmask);
                    match UserPtrMut::from_addr(
                        ucontext_addr + 2 * size_of::<usize>() + size_of::<SignalStack>(),
                    )
                    .write(token, &user_sigmask)
                    {
                        Ok(()) => {}
                        Err(_) => {
                            error!(
                                "[do_signal] Failed to write sigmask to user stack! Send SIGSEGV."
                            );
                            drop(inner);
                            drop(sighand);
                            drop(task);
                            exit_current_with_sigsegv();
                        }
                    }

                    if UserPtrMut::from_addr(
                        ucontext_addr
                            + 2 * size_of::<usize>()
                            + size_of::<SignalStack>()
                            + size_of::<UserSignalMask>()
                            + UserContext::PADDING_SIZE,
                    )
                    .write(token, &mcontext) // push MachineContext into user stack
                    .is_err()
                    {
                        error!(
                            "[do_signal] Failed to write MachineContext to user stack. Send SIGSEGV."
                        );
                        drop(inner);
                        drop(sighand);
                        drop(task);
                        exit_current_with_sigsegv();
                    }
                }
                let trap_cx = inner.get_trap_cx();
                trap_cx.gp.a0 = signum; // a0 <- signum
                trap_cx.set_sp(sig_sp); // update sp, because we've pushed something into stack
                trap_cx.gp.ra = if act.flags.contains(SigActionFlags::SA_RESTORER)
                    && act.restorer != 0
                {
                    act.restorer // legacy, signal trampoline provided by C library's wrapper function
                } else {
                    SIGNAL_TRAMPOLINE // ra <- __call_sigreturn, when handler ret, we will go to __call_sigreturn
                };
                trap_cx.gp.pc = act.handler.addr().unwrap(); // restore pc with addr of handler
            } else {
                error!(
                    "[do_signal] User stack will overflow after push trap context! Send SIGSEGV."
                );
                drop(inner);
                drop(sighand);
                drop(task);
                exit_current_with_sigsegv();
            }
            let (trace_ra, trace_sp) = {
                let trap_cx = inner.get_trap_cx();
                (trap_cx.gp.ra, trap_cx.gp.sp)
            };
            trace!(
                "[do_signal] signal: {:?}, signum: {:?}, handler: {:?} (ra: 0x{:X}, sp: 0x{:X})",
                signal,
                signum,
                act.handler,
                trace_ra,
                trace_sp
            );
            // mask some signals
            inner.sigmask |= if act.flags.contains(SigActionFlags::SA_NODEFER) {
                act.mask - Signals::CAN_NOT_BE_MASKED
            } else {
                (signal | act.mask) - Signals::CAN_NOT_BE_MASKED
            };
            if act.flags.contains(SigActionFlags::SA_RESETHAND) {
                sighand.set(signum, None);
            }
            // go back to `trap_return`
            return;
        } else {
            // user program doesn't register a handler for this signal, use our default handler
            match signal {
                // caused by a specific instruction in user program, print log here before exit
                Signals::SIGILL | Signals::SIGSEGV => {
                    let scause = get_exception_cause();
                    if signal == Signals::SIGILL {
                        let stval = get_bad_instruction();
                        warn!("[do_signal] process terminated due to {:?}", signal);
                        println!(
                        "[kernel] {:?} in application, instruction addr = {:#x}, bad instruction = {:#x}, core dumped.",
                        scause,
                        inner.get_trap_cx().gp.pc,
                        stval,
                        );
                    } else {
                        let stval = get_bad_addr();
                        warn!("[do_signal] process terminated due to {:?}", signal);
                        println!(
                        "[kernel] {:?} in application, bad addr = {:#x}, bad instruction = {:#x}, core dumped.",
                        scause,
                        stval,
                        inner.get_trap_cx().gp.pc,
                        );
                    };
                    drop(inner);
                    drop(sighand);
                    drop(task);
                    exit_group_and_run_next(default_signal_wait_status(signal));
                }
                // the current process we are handing is sure to be in RUNNING status, so just ignore SIGCONT
                // where we really wake up this process is where we sent SIGCONT, such as `sys_kill()`
                Signals::SIGCHLD | Signals::SIGCONT | Signals::SIGURG | Signals::SIGWINCH => {
                    trace!("[do_signal] Ignore {:?}", signal);
                    continue;
                }
                // stop (or we should say block) current process
                Signals::SIGTSTP | Signals::SIGTTIN | Signals::SIGTTOU => {
                    drop(inner);
                    drop(sighand);
                    drop(task);
                    wait_for_default_stop_signal();
                    // because this loop require `inner`, and we have `drop(inner)` above, so `break` is compulsory
                    // this would cause some signals won't be handled immediately when this process resumes
                    // but it doesn't matter, maybe
                    break;
                }
                // for all other signals, we should terminate current process
                _ => {
                    warn!("[do_signal] process terminated due to {:?}", signal);
                    drop(inner);
                    drop(sighand);
                    drop(task);
                    exit_group_and_run_next(default_signal_wait_status(signal));
                }
            }
        }
    }
}

pub fn sigaltstack(ss: *const SignalStack, old_ss: *mut SignalStack) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let new_stack = match UserPtr::new(ss).read_optional(token) {
        Ok(stack) => stack,
        Err(errno) => return errno,
    };
    let mut inner = task.acquire_inner_lock();
    let current_sp = inner.get_trap_cx().gp.sp;
    let old_stack = inner.signal_stack.with_runtime_flags(current_sp);

    if !old_ss.is_null() {
        if UserPtrMut::new(old_ss).write(token, &old_stack).is_err() {
            return crate::syscall::errno::EFAULT;
        }
    }

    if let Some(mut stack) = new_stack {
        let flags = SignalStackFlags::from_bits_truncate(stack.flags);
        let allowed = SignalStackFlags::DISABLE | SignalStackFlags::AUTODISARM;
        if stack.flags & !allowed.bits != 0 || flags.contains(SignalStackFlags::ONSTACK) {
            return crate::syscall::errno::EINVAL;
        }
        if inner.signal_stack.contains_sp(current_sp) {
            return crate::syscall::errno::EPERM;
        }
        if flags.contains(SignalStackFlags::DISABLE) {
            inner.signal_stack = SignalStack::disabled();
        } else {
            if stack.size < SignalStack::MIN_SIZE {
                return crate::syscall::errno::ENOMEM;
            }
            if stack.sp.checked_add(stack.size).is_none() {
                return crate::syscall::errno::ENOMEM;
            }
            stack.flags &= allowed.bits;
            stack.flags &= !SignalStackFlags::DISABLE.bits;
            stack.flags &= !SignalStackFlags::ONSTACK.bits;
            inner.signal_stack = stack;
        }
    }
    crate::syscall::errno::SUCCESS
}

bitflags! {
    pub struct SigMaskHow: u32 {
        const SIG_BLOCK     = 0;
        const SIG_UNBLOCK   = 1;
        const SIG_SETMASK   = 2;
    }
}

/// fetch and/or change the signal mask of the calling thread.
/// # Warning
/// In fact, `set` & `oldset` should be 1024 bits `sigset_t`, but we only support 64 signals now.
/// For the sake of performance, we use `Signals` instead.
pub fn sigprocmask(how: u32, set: *const Signals, oldset: *mut Signals) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let token = task.get_user_token();
    // If oldset is non-NULL, the previous value of the signal mask is stored in oldset
    if oldset as usize != 0 {
        let old_bits = inner.sigmask.bits() as u64;
        match UserPtrMut::new(oldset as *mut u64).write(token, &old_bits) {
            Ok(()) => {}
            Err(errno) => return errno,
        }
        trace!("[sigprocmask] *oldset: ({:?})", inner.sigmask);
    }
    // If set is NULL, then the signal mask is unchanged
    if set as usize != 0 {
        let how = SigMaskHow::from_bits(how);
        let signal_set = match UserPtr::new(set as *const u64).read(token) {
            Ok(bits) => Signals::from_bits_truncate(bits as signal_type!()),
            Err(errno) => return errno,
        };
        trace!("[sigprocmask] how: {:?}, *set: ({:?})", how, signal_set);
        match how {
            // add the signals not yet blocked in the given set to the mask
            Some(SigMaskHow::SIG_BLOCK) => {
                inner.sigmask.insert(signal_set);
            }
            // remove the blocked signals in the set from the sigmask
            // NOTE: unblocking a signal not blocked is allowed
            Some(SigMaskHow::SIG_UNBLOCK) => {
                inner.sigmask.remove(signal_set);
            }
            // set the signal mask to what we see
            Some(SigMaskHow::SIG_SETMASK) => {
                inner.sigmask = signal_set;
            }
            // `how` was invalid
            _ => return EINVAL,
        };
        // unblock SIGILL & SIGSEGV, otherwise infinite loop may occurred
        // unblock SIGKILL & SIGSTOP, they can't be masked according to standard
        inner.sigmask.remove(Signals::CAN_NOT_BE_MASKED);
    }
    SUCCESS
}

#[allow(unused)]
#[derive(Clone, Copy, Debug)]
#[repr(C)] //UNSAFE! IS THIS CORRECT?
pub struct SigInfo {
    si_signo: u32,
    si_errno: u32,
    si_code: u32,
    __pad0: u32,
    si_pid: u32,
    si_uid: u32,
    si_value: usize,
    // unsupported fields
    __pad: [u8; 128 - 6 * core::mem::size_of::<u32>() - core::mem::size_of::<usize>()],
}

impl SigInfo {
    pub fn new(si_signo: usize, si_errno: usize, si_code: usize) -> Self {
        Self::new_with_sender(si_signo, si_errno, si_code, 0)
    }

    pub fn new_with_sender(
        si_signo: usize,
        si_errno: usize,
        si_code: usize,
        si_pid: usize,
    ) -> Self {
        Self::new_with_sender_value(si_signo, si_errno, si_code, si_pid, 0)
    }

    pub fn new_with_sender_value(
        si_signo: usize,
        si_errno: usize,
        si_code: usize,
        si_pid: usize,
        si_value: usize,
    ) -> Self {
        Self {
            si_signo: si_signo as u32,
            si_errno: si_errno as u32,
            si_code: si_code as u32,
            __pad0: 0,
            si_pid: si_pid as u32,
            si_uid: 0,
            si_value,
            __pad: [0; 128 - 6 * core::mem::size_of::<u32>() - core::mem::size_of::<usize>()],
        }
    }

    pub fn with_signal_sender(mut self, si_signo: usize, si_pid: usize) -> Self {
        self.si_signo = si_signo as u32;
        self.si_pid = si_pid as u32;
        self.si_uid = 0;
        self
    }
}

#[allow(unused)]
impl SigInfo {
    pub const SI_ASYNCNL: u32 = 60u32.wrapping_neg();
    pub const SI_TKILL: u32 = 6u32.wrapping_neg();
    pub const SI_SIGIO: u32 = 5u32.wrapping_neg();
    pub const SI_ASYNCIO: u32 = 4u32.wrapping_neg();
    pub const SI_MESGQ: u32 = 3u32.wrapping_neg();
    pub const SI_TIMER: u32 = 2u32.wrapping_neg();
    pub const SI_QUEUE: u32 = 1u32.wrapping_neg();
    pub const SI_USER: u32 = 0;
    pub const SI_KERNEL: u32 = 128;
    const FPE_INTDIV: u32 = 1;
    const FPE_INTOVF: u32 = 2;
    const FPE_FLTDIV: u32 = 3;
    const FPE_FLTOVF: u32 = 4;
    const FPE_FLTUND: u32 = 5;
    const FPE_FLTRES: u32 = 6;
    const FPE_FLTINV: u32 = 7;
    const FPE_FLTSUB: u32 = 8;
    const ILL_ILLOPC: u32 = 1;
    const ILL_ILLOPN: u32 = 2;
    const ILL_ILLADR: u32 = 3;
    const ILL_ILLTRP: u32 = 4;
    const ILL_PRVOPC: u32 = 5;
    const ILL_PRVREG: u32 = 6;
    const ILL_COPROC: u32 = 7;
    const ILL_BADSTK: u32 = 8;
    const SEGV_MAPERR: u32 = 1;
    const SEGV_ACCERR: u32 = 2;
    const SEGV_BNDERR: u32 = 3;
    const SEGV_PKUERR: u32 = 4;
    const BUS_ADRALN: u32 = 1;
    const BUS_ADRERR: u32 = 2;
    const BUS_OBJERR: u32 = 3;
    const BUS_MCEERR_AR: u32 = 4;
    const BUS_MCEERR_AO: u32 = 5;
    const CLD_EXITED: u32 = 1;
    const CLD_KILLED: u32 = 2;
    const CLD_DUMPED: u32 = 3;
    const CLD_TRAPPED: u32 = 4;
    const CLD_STOPPED: u32 = 5;
    const CLD_CONTINUED: u32 = 6;
}
