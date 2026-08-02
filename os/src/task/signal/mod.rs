//! POSIX/Linux 信号核心实现。
//!
//! 本模块定义信号集合、`rt_sigaction` ABI、备用信号栈、`siginfo_t` 布局，
//! 并在 `do_signal()` 中把 pending signal 转换为用户 handler 调用或默认动作。
//! 进程级投递、pending 队列和等待 syscall 分别拆到子模块。
//!
//! # Locking
//!
//! `do_signal()` 只在选择 pending signal、读取 action 和提交 trap context 时
//! 短暂持锁。向用户栈写 signal frame 前必须释放 `task.inner` 和 `sighand`，
//! 在停止进程、退出进程或重新进入调度前也必须显式释放这些锁。

use crate::hal::{get_bad_addr, get_bad_instruction, get_exception_cause, UserContext};
use crate::signal_type;
use core::fmt::{self, Debug, Formatter};
use log::{error, warn};

use crate::config::*;
use crate::mm::{UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    block_current_and_run_next_with_lock_checked, exit_current_and_run_next,
    exit_group_and_run_next, WaitQueue,
};
use alloc::sync::Arc;

use super::current_task;
use super::task::TaskControlBlock;
use crate::utils::error::SyscallErr;

mod action;
mod delivery;
mod frame;
mod pending;
mod wait;

pub use action::Sighand;
pub(crate) use delivery::{
    queue_kernel_process_signal, queue_process_signal_info, wake_process_signal_waiter,
};
pub use delivery::{
    send_process_signal, send_process_signal_info, send_process_signal_to_current_task,
    send_thread_signal, send_thread_signal_info_deferred,
};
use frame::signal_frame_layout;
pub(crate) use pending::PosixTimerEventId;
pub use pending::{is_realtime_signal, PendingSignal, SignalQueue};
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
    // SIGKILL | SIGSTOP
    /// Signals that cannot be blocked by user sigprocmask.
    pub const CAN_NOT_BE_MASKED: Signals = Signals::from_bits_truncate(1 << 8 | 1 << 18);
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

    pub fn wakes_interruptible(
        self,
        sigmask: Signals,
        signal_wait_mask: Signals,
        wake_unblocked: bool,
    ) -> bool {
        self.contains(Signals::SIGCONT)
            || !(self & signal_wait_mask).is_empty()
            || (wake_unblocked && !self.difference(sigmask).is_empty())
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

/// 查询或替换进程共享的 signal disposition。
///
/// `act == NULL` 只查询，`oldact == NULL` 不返回旧值。`SIGKILL` 和 `SIGSTOP`
/// 的 disposition 不可修改。
pub fn sigaction(signum: usize, act: *const UserSigAction, oldact: *mut UserSigAction) -> isize {
    let task = current_task().unwrap();
    if matches!(signum, 0 | 9 | 19 | 65..) {
        warn!("[sigaction] bad signum: {}", signum);
        return EINVAL;
    }

    let token = task.process.user_token();
    // Linux 先读取新 action，再在 sighand 锁内同时快照旧值并提交新值。
    // 这样用户缺页不会阻塞同进程其他线程的 signal disposition 操作。
    let new_action = match UserPtr::new(act).read_optional(token) {
        Ok(action) => action.map(UserSigAction::into_kernel),
        Err(_) => {
            log::error!("[sigaction] Failed to copy sigact {:?} from user.", act);
            return EFAULT;
        }
    };
    let old_action = {
        let sighand_ref = task.process.sighand();
        let mut sighand = sighand_ref.lock();
        let old_action =
            UserSigAction::from_kernel(sighand.get(signum).copied().unwrap_or_else(SigAction::new));
        if let Some(mut action) = new_action {
            action.mask.remove(Signals::CAN_NOT_BE_MASKED);
            if action.handler == SigHandler::SIG_IGN {
                // 显式保存 SIG_IGN，不能把它和默认动作混为一谈。
                sighand.set(signum, Some(action));
            } else if action.handler == SigHandler::SIG_DFL && action.flags.is_empty() {
                sighand.set(signum, None);
            } else {
                sighand.set(signum, Some(action));
            }
        }
        old_action
    };
    // 新 action 已经提交；旧值写回失败只返回 EFAULT，不回滚共享 sighand。
    if !oldact.is_null() && UserPtrMut::new(oldact).write(token, &old_action).is_err() {
        log::error!(
            "[sigaction] Failed to copy old action {:?} to user.",
            oldact
        );
        return EFAULT;
    }
    SUCCESS
}

pub fn sigchld_requests_auto_reap(sighand: &Sighand) -> bool {
    const SIGCHLD_SIGNUM: usize = 17;
    match sighand.get(SIGCHLD_SIGNUM) {
        Some(act) => {
            act.handler == SigHandler::SIG_IGN || act.flags.contains(SigActionFlags::SA_NOCLDWAIT)
        }
        None => false,
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
    if signal_default_dumps_core(signum) && current_core_dump_enabled() {
        signum | WAIT_COREDUMP
    } else {
        signum
    }
}

fn signal_default_dumps_core(signum: u32) -> bool {
    matches!(signum, 3 | 4 | 5 | 6 | 7 | 8 | 11 | 24 | 25 | 31)
}

fn current_core_dump_enabled() -> bool {
    current_task()
        .map(|task| {
            // 两个 owner 分别快照，rlimit 锁不跨入线程私有 dumpable 状态。
            let core_limit = task.process.core_limit();
            let inner = task.acquire_inner_lock();
            core_limit > 0 && inner.dumpable != 0
        })
        .unwrap_or(false)
}

fn exit_current_with_sigsegv() -> ! {
    exit_current_and_run_next(default_signal_wait_status(Signals::SIGSEGV));
}

/// 把完整的 rt signal frame 写入用户栈。
///
/// 调用方必须先释放任务锁；只有两个对象都写成功后，才能提交 handler 入口。
fn write_user_signal_frame(
    token: usize,
    ucontext_addr: usize,
    siginfo_addr: usize,
    user_context: &UserContext,
    siginfo: &SigInfo,
) -> Result<(), isize> {
    UserPtrMut::from_addr(siginfo_addr).write(token, siginfo)?;
    UserPtrMut::from_addr(ucontext_addr).write(token, user_context)
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
    let pending = inner.sigpending.pending() | task.process.shared_pending_hint();
    pending.difference(sigmask)
}

fn pending_signals(task: &TaskControlBlock) -> Signals {
    let inner = task.acquire_inner_lock();
    inner.sigpending.pending() | task.process.shared_pending_hint()
}

/// 判断 `sigtimedwait` 正在等待的集合中是否已有 pending signal。
///
/// 线程队列与进程共享队列分别在各自 owner 锁下读取，避免嵌套锁。该谓词只用于
/// 关闭“条件复查完成、调度器登记睡眠意图之前”的丢唤醒窗口，不领取信号。
pub(crate) fn has_waited_signal(task: &TaskControlBlock) -> bool {
    let (thread_pending, wait_mask) = {
        let inner = task.acquire_inner_lock();
        (inner.sigpending.pending(), inner.signal_wait_mask)
    };
    if wait_mask.is_empty() {
        return false;
    }
    !(thread_pending & wait_mask).is_empty()
        || !(task.process.shared_pending() & wait_mask).is_empty()
}

fn has_pending_stop_release_signal(task: &TaskControlBlock) -> bool {
    // SIGCONT resumes a stopped task even if the signal is currently masked;
    // SIGKILL must also break a stopped wait so the task can terminate.
    let pending = pending_signals(task);
    pending.contains(Signals::SIGCONT) || pending.contains(Signals::SIGKILL)
}

fn signal_is_actionable(sighand: &Sighand, signum: usize, signal: Signals) -> bool {
    match sighand.get(signum) {
        Some(act) if act.handler == SigHandler::SIG_IGN => false,
        Some(act) if act.handler == SigHandler::SIG_DFL => !SIG_DFL_IGNORE.contains(signal),
        Some(_) => true,
        None => !SIG_DFL_IGNORE.contains(signal),
    }
}

fn take_next_pending_signal(
    task: &TaskControlBlock,
    inner: &mut super::task::TaskControlBlockInner,
) -> Option<PendingSignal> {
    let thread_pending = inner.sigpending.pending().difference(inner.sigmask);
    if let Some(pending) = inner.sigpending.dequeue_matching(thread_pending) {
        return Some(pending);
    }

    let shared_pending = task.process.shared_pending_hint().difference(inner.sigmask);
    if shared_pending.is_empty() {
        return None;
    }
    task.process.take_shared_matching(shared_pending)
}

pub fn discard_non_actionable_unblocked_signals(task: &TaskControlBlock) {
    let (thread_pending, sigmask, wait_mask) = {
        let inner = task.acquire_inner_lock();
        (
            inner
                .sigpending
                .pending()
                .difference(inner.sigmask)
                .difference(inner.signal_wait_mask),
            inner.sigmask,
            inner.signal_wait_mask,
        )
    };
    // disposition 为 ignore 的信号通常可直接清理，但正在被 sigtimedwait 领取的
    // 集合必须保留；否则未屏蔽的 waited signal 会在登记窗口中被通用清理器吞掉。
    let shared_pending = task
        .process
        .shared_pending_hint()
        .difference(sigmask)
        .difference(wait_mask);
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
    loop {
        let task = current_task().unwrap();
        if has_pending_stop_release_signal(&task) {
            break;
        }

        let mut guard = wait_queue.lock();
        guard.prepare_to_wait(Arc::downgrade(&task));
        if has_pending_stop_release_signal(&task) {
            guard.finish_wait(task.as_ref());
            break;
        }
        drop(task);

        block_current_and_run_next_with_lock_checked(guard, |task| {
            !has_pending_stop_release_signal(task)
        });

        let task = current_task().unwrap();
        wait_queue.lock().finish_wait(&task);
    }
}

fn stop_current_process_for_signal(signum: usize) {
    let task = current_task().unwrap();
    let process = task.process.clone();
    process.mark_stopped(signum);
    drop(task);
    wait_for_default_stop_signal();
}

fn signal_should_ptrace_stop(inner: &super::task::TaskControlBlockInner, signal: Signals) -> bool {
    inner.ptrace_traceme && signal != Signals::SIGKILL && signal != Signals::SIGCONT
}

/// 执行信号处理，返回仍由本 CPU current 槽拥有的任务。
///
/// 调用者会立即进入不返回的用户态恢复汇编，因此必须在跳转前
/// 显式 `drop` 返回的 `Arc`，不能让它留在永不析构的 trap 栈帧上。
pub fn do_signal() -> Arc<TaskControlBlock> {
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    if inner.pending_oom_kill {
        inner.pending_oom_kill = false;
        inner.add_signal(Signals::SIGKILL);
        warn!(
            "[OOM killer] tid {} pid {} marked for OOM kill, sending SIGKILL",
            task.gettid(),
            task.pid()
        );
    }
    while let Some(pending) = take_next_pending_signal(&task, &mut inner) {
        let signum = pending.signum();
        let signal = pending.signal;
        let sighand_ref = task.process.sighand();
        let mut sighand = sighand_ref.lock();
        if signal_should_ptrace_stop(&inner, signal) {
            drop(sighand);
            drop(sighand_ref);
            drop(inner);
            drop(task);
            stop_current_process_for_signal(signum);
            return do_signal();
        }
        // user-defined handler
        if let Some(act) = sighand.get(signum).copied() {
            // SIG_IGN → discard this signal (POSIX: ignored signals are not delivered)
            if act.handler == SigHandler::SIG_IGN {
                continue;
            }
            if act.handler != SigHandler::SIG_DFL {
                // Linux 在取得 action 后、释放 sighand 锁前完成一次性 handler
                // 的复位，避免另一个线程再次观察到旧 handler。
                if act.flags.contains(SigActionFlags::SA_RESETHAND) {
                    let mut reset_action = act;
                    reset_action.handler = SigHandler::SIG_DFL;
                    sighand.set(signum, Some(reset_action));
                }
                drop(sighand);
                drop(sighand_ref);

                // 所有可能失败的用户态写入都基于本地快照完成。写成功前不改
                // live trap context，失败时可直接按 SIGSEGV 退出当前进程。
                let mut return_context = *inner.trap_context_mut();
                let a0_isize = return_context.gp.a0 as isize;
                // if this syscall wants to restart
                if a0_isize == -(SyscallErr::ERESTART as isize) {
                    // and if `SA_RESTART` is set
                    if act.flags.contains(SigActionFlags::SA_RESTART)
                        && return_context.gp.a7 != SYSCALL_SIGTIMEDWAIT
                        && return_context.gp.a7 != SYSCALL_RT_SIGSUSPEND
                    {
                        // back to `ecall`
                        return_context.gp.pc -= 4;
                        // restore syscall parameter `a0`
                        return_context.gp.a0 = return_context.origin_a0;
                    } else {
                        // will return EINTR after sigreturn
                        return_context.gp.a0 = EINTR as usize;
                    }
                }
                let current_sp = return_context.gp.sp;
                let alt_stack = inner.signal_stack;
                let use_alt_stack = act.flags.contains(SigActionFlags::SA_ONSTACK)
                    && !alt_stack.is_disabled()
                    && !alt_stack.contains_sp(current_sp);
                let default_stack_top = task.ustack_bottom_va();
                let default_stack_bottom = default_stack_top - USER_STACK_SIZE;
                let normal_stack_bottom =
                    if current_sp > default_stack_bottom && current_sp <= default_stack_top {
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
                let Some((ucontext_addr, siginfo_addr, sig_sp, sig_size)) =
                    signal_frame_layout(frame_base_sp, stack_bottom)
                else {
                    error!("[do_signal] User stack has no room for signal frame. Send SIGSEGV.");
                    drop(inner);
                    drop(task);
                    exit_current_with_sigsegv();
                };

                let current_sigmask = inner.sigmask;
                // sigsuspend 的旧 mask 只在 frame 成功提交后清除；用户写失败时
                // 保持 live task 状态不变，由 SIGSEGV 退出路径统一收尾。
                let saved_sigmask = inner.sigmask_to_restore.unwrap_or(current_sigmask);
                let handler_sigmask = current_sigmask
                    | if act.flags.contains(SigActionFlags::SA_NODEFER) {
                        act.mask - Signals::CAN_NOT_BE_MASKED
                    } else {
                        (signal | act.mask) - Signals::CAN_NOT_BE_MASKED
                    };
                let mcontext = return_context.machine_context();
                #[cfg(feature = "loongarch64")]
                let lsx = return_context.lsx;
                let mut frame_stack = if use_alt_stack {
                    alt_stack.with_runtime_flags(sig_sp)
                } else {
                    SignalStack::new(sig_sp, sig_size)
                };
                if use_alt_stack {
                    frame_stack.flags = SignalStackFlags::ONSTACK.bits;
                }

                #[cfg(feature = "loongarch64")]
                let user_context =
                    UserContext::new(0, 0, frame_stack, saved_sigmask, mcontext, lsx);
                #[cfg(feature = "riscv")]
                let user_context = UserContext::new(0, 0, frame_stack, saved_sigmask, mcontext);
                let handler_pc = act.handler.addr().unwrap();
                let handler_ra =
                    if act.flags.contains(SigActionFlags::SA_RESTORER) && act.restorer != 0 {
                        act.restorer
                    } else {
                        SIGNAL_TRAMPOLINE
                    };
                let token = task.process.user_token();
                drop(inner);

                if let Err(errno) = write_user_signal_frame(
                    token,
                    ucontext_addr,
                    siginfo_addr,
                    &user_context,
                    &pending.siginfo,
                ) {
                    error!(
                        "[do_signal] Failed to write user signal frame: {}. Send SIGSEGV.",
                        errno
                    );
                    drop(task);
                    exit_current_with_sigsegv();
                }

                // 用户 frame 已完整可见，此时才原子化地发布 handler 入口。
                let mut inner = task.acquire_inner_lock();
                let trap_cx = inner.trap_context_mut();
                trap_cx.set_machine_context(mcontext);
                #[cfg(feature = "loongarch64")]
                {
                    trap_cx.lsx = lsx;
                }
                trap_cx.gp.a0 = signum;
                // Linux rt frame 始终提供 siginfo/ucontext 地址；未声明
                // SA_SIGINFO 的单参数 handler 会自然忽略额外参数寄存器。
                trap_cx.gp.a1 = siginfo_addr;
                trap_cx.gp.a2 = ucontext_addr;
                trap_cx.set_sp(sig_sp);
                trap_cx.gp.ra = handler_ra;
                trap_cx.gp.pc = handler_pc;
                inner.sigmask_to_restore = None;
                inner.sigmask = handler_sigmask;
                drop(inner);
                return task;
            }
        }
        // user program doesn't register a handler for this signal, use our default handler
        match signal {
            // caused by a specific instruction in user program, print log here before exit
            Signals::SIGILL | Signals::SIGSEGV => {
                warn!("[do_signal] process terminated due to {:?}", signal);
                if pending.siginfo.is_sync_fault_for(signal) {
                    let scause = get_exception_cause();
                    if signal == Signals::SIGILL {
                        let stval = get_bad_instruction();
                        println!(
                                "[kernel] {:?} in application, instruction addr = {:#x}, bad instruction = {:#x}, core dumped.",
                                scause,
                                inner.trap_context_mut().gp.pc,
                                stval,
                            );
                    } else {
                        let stval = get_bad_addr();
                        println!(
                                "[kernel] {:?} in application, bad addr = {:#x}, bad instruction = {:#x}, core dumped.",
                                scause,
                                stval,
                                inner.trap_context_mut().gp.pc,
                            );
                    }
                }
                drop(inner);
                drop(sighand);
                drop(sighand_ref);
                drop(task);
                exit_group_and_run_next(default_signal_wait_status(signal));
            }
            // the current process we are handing is sure to be in RUNNING status, so just ignore SIGCONT
            // where we really wake up this process is where we sent SIGCONT, such as `sys_kill()`
            Signals::SIGCHLD | Signals::SIGCONT | Signals::SIGURG | Signals::SIGWINCH => {
                continue;
            }
            // stop (or we should say block) current process
            Signals::SIGSTOP | Signals::SIGTSTP | Signals::SIGTTIN | Signals::SIGTTOU => {
                drop(inner);
                drop(sighand);
                drop(sighand_ref);
                drop(task);
                stop_current_process_for_signal(signum);
                return do_signal();
            }
            // for all other signals, we should terminate current process
            _ => {
                warn!("[do_signal] process terminated due to {:?}", signal);
                drop(inner);
                drop(sighand);
                drop(sighand_ref);
                drop(task);
                exit_group_and_run_next(default_signal_wait_status(signal));
            }
        }
    }
    drop(inner);
    task
}

pub fn sigaltstack(ss: *const SignalStack, old_ss: *mut SignalStack) -> isize {
    let task = current_task().unwrap();
    let token = task.process.user_token();
    let new_stack = match UserPtr::new(ss).read_optional(token) {
        Ok(stack) => stack,
        Err(errno) => return errno,
    };
    let old_stack = {
        let mut inner = task.acquire_inner_lock();
        let current_sp = inner.trap_context_mut().gp.sp;
        let old_stack = inner.signal_stack.with_runtime_flags(current_sp);

        if let Some(mut stack) = new_stack {
            let flags = SignalStackFlags::from_bits_truncate(stack.flags);
            let allowed = SignalStackFlags::DISABLE | SignalStackFlags::AUTODISARM;
            if stack.flags & !allowed.bits != 0 || flags.contains(SignalStackFlags::ONSTACK) {
                return EINVAL;
            }
            if inner.signal_stack.contains_sp(current_sp) {
                return EPERM;
            }
            if flags.contains(SignalStackFlags::DISABLE) {
                inner.signal_stack = SignalStack::disabled();
            } else {
                if stack.size < SignalStack::MIN_SIZE || stack.sp.checked_add(stack.size).is_none()
                {
                    return ENOMEM;
                }
                stack.flags &= allowed.bits;
                stack.flags &= !SignalStackFlags::DISABLE.bits;
                stack.flags &= !SignalStackFlags::ONSTACK.bits;
                inner.signal_stack = stack;
            }
        }
        old_stack
    };

    // signal stack 已在短临界区内提交；copyout 可能缺页，必须位于 task 锁外。
    if !old_ss.is_null() && UserPtrMut::new(old_ss).write(token, &old_stack).is_err() {
        return EFAULT;
    }
    SUCCESS
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
/// 用户 ABI 只读写当前内核支持的低 64 位 signal mask；内核中的 `Signals`
/// 可以更宽，不能直接按其 Rust 布局执行 uaccess。
pub fn sigprocmask(how: u32, set: *const u64, oldset: *mut u64) -> isize {
    let task = current_task().unwrap();
    let token = task.process.user_token();
    let new_set = match UserPtr::new(set).read_optional(token) {
        Ok(bits) => bits.map(|bits| Signals::from_bits_truncate(bits as signal_type!())),
        Err(errno) => return errno,
    };
    let old_bits = {
        let mut inner = task.acquire_inner_lock();
        let old_bits = inner.sigmask.bits() as u64;
        if let Some(signal_set) = new_set {
            match SigMaskHow::from_bits(how) {
                Some(SigMaskHow::SIG_BLOCK) => inner.sigmask.insert(signal_set),
                Some(SigMaskHow::SIG_UNBLOCK) => inner.sigmask.remove(signal_set),
                Some(SigMaskHow::SIG_SETMASK) => inner.sigmask = signal_set,
                _ => return EINVAL,
            }
            inner.sigmask.remove(Signals::CAN_NOT_BE_MASKED);
        }
        old_bits
    };

    // 与 Linux 一致：新 mask 已提交后，oldset copyout 失败不回滚线程 mask。
    if !oldset.is_null() {
        if let Err(errno) = UserPtrMut::new(oldset).write(token, &old_bits) {
            return errno;
        }
    }
    SUCCESS
}

#[allow(unused)]
#[derive(Clone, Copy, Debug)]
#[repr(C)]
/// Linux 兼容的 `siginfo_t` 子集。
///
/// # Semantics
///
/// 布局固定为 128 字节，当前只填充 signal number、errno、code、sender pid/uid
/// 和 `sigval`。其它联合字段以 padding 保留，避免 copy_to_user 覆盖用户栈。
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

    /// 构造 Linux `SI_TIMER` 联合布局。
    ///
    /// `siginfo_t` 的 timer 分支与通用 sender 分支复用同一段内存：
    /// `si_pid/si_uid/si_value` 对应 `si_tid/si_overrun/si_value`。
    pub(crate) fn new_timer(
        si_signo: usize,
        timer_id: usize,
        overrun: usize,
        si_value: usize,
    ) -> Self {
        let mut info =
            Self::new_with_sender_value(si_signo, 0, Self::SI_TIMER as usize, timer_id, si_value);
        info.si_uid = overrun.min(i32::MAX as usize) as u32;
        info
    }

    pub(crate) fn set_timer_overrun(&mut self, overrun: usize) {
        self.si_uid = overrun.min(i32::MAX as usize) as u32;
    }

    pub fn timer_id(&self) -> u32 {
        self.si_pid
    }

    pub fn timer_overrun(&self) -> u32 {
        self.si_uid
    }

    pub fn with_signal_sender(mut self, si_signo: usize, si_pid: usize) -> Self {
        self.si_signo = si_signo as u32;
        self.si_pid = si_pid as u32;
        self.si_uid = 0;
        self
    }

    pub fn signo(&self) -> usize {
        self.si_signo as usize
    }

    pub fn errno(&self) -> i32 {
        self.si_errno as i32
    }

    pub fn code(&self) -> i32 {
        self.si_code as i32
    }

    pub fn sender_pid(&self) -> u32 {
        self.si_pid
    }

    pub fn sender_uid(&self) -> u32 {
        self.si_uid
    }

    pub fn value(&self) -> usize {
        self.si_value
    }

    pub fn is_kernel_generated(&self) -> bool {
        (self.si_code as i32) >= 0
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
    pub const ILL_ILLOPC: u32 = 1;
    const ILL_ILLOPN: u32 = 2;
    const ILL_ILLADR: u32 = 3;
    const ILL_ILLTRP: u32 = 4;
    const ILL_PRVOPC: u32 = 5;
    const ILL_PRVREG: u32 = 6;
    const ILL_COPROC: u32 = 7;
    const ILL_BADSTK: u32 = 8;
    pub const SEGV_MAPERR: u32 = 1;
    pub const SEGV_ACCERR: u32 = 2;
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

    pub fn is_sync_fault_for(&self, signal: Signals) -> bool {
        match signal {
            Signals::SIGILL => (Self::ILL_ILLOPC..=Self::ILL_BADSTK).contains(&self.si_code),
            Signals::SIGSEGV => (Self::SEGV_MAPERR..=Self::SEGV_PKUERR).contains(&self.si_code),
            _ => false,
        }
    }
}
