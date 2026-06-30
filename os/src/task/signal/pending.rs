//! pending signal 队列。
//!
//! MangoCore 分别维护线程私有 pending 队列和进程共享 pending 队列。本模块提供
//! 队列元素、位图缓存和 Linux 非实时/实时信号排队语义。
//!
//! # Linux Compatibility
//!
//! 非实时信号在 pending 队列中合并为一个条目；实时信号允许重复排队，但队列
//! 有固定上限，耗尽时返回 `-EAGAIN`。

use alloc::collections::VecDeque;

use crate::syscall::errno::{EAGAIN, EINVAL};

use super::{SigInfo, Signals};

const MAX_QUEUED_SIGNALS: usize = 64;

#[derive(Clone, Copy, Debug)]
/// 一个带 `siginfo_t` 的 pending signal。
pub struct PendingSignal {
    /// 信号位图。队列项应只包含单个信号。
    pub signal: Signals,
    /// 投递给用户或等待接口的 `siginfo_t` 负载。
    pub siginfo: SigInfo,
}

impl PendingSignal {
    /// 根据信号编号创建无发送者信息的 pending signal。
    ///
    /// # Errors
    ///
    /// `signum` 不在支持范围内时返回 `-EINVAL`。
    pub fn new(signum: usize, si_errno: usize, si_code: usize) -> Result<Self, isize> {
        Self::new_with_sender(signum, si_errno, si_code, 0)
    }

    pub fn new_with_sender(
        signum: usize,
        si_errno: usize,
        si_code: usize,
        si_pid: usize,
    ) -> Result<Self, isize> {
        let signal = Signals::from_signum(signum).map_err(|_| EINVAL)?;
        Ok(Self {
            signal,
            siginfo: SigInfo::new_with_sender(signum, si_errno, si_code, si_pid),
        })
    }

    /// 根据信号位图创建 pending signal。
    ///
    /// # Errors
    ///
    /// `signal` 为空或包含多个信号时返回 `-EINVAL`。
    pub fn from_signal(signal: Signals, si_code: usize) -> Result<Self, isize> {
        Self::from_signal_with_sender(signal, si_code, 0)
    }

    pub fn from_signal_with_sender(
        signal: Signals,
        si_code: usize,
        si_pid: usize,
    ) -> Result<Self, isize> {
        let signum = signal.to_signum().map_err(|_| EINVAL)?;
        Self::new_with_sender(signum, 0, si_code, si_pid)
    }

    pub fn signum(&self) -> usize {
        self.signal.to_signum().unwrap_or(0)
    }
}

#[derive(Clone, Debug)]
/// Pending signal 队列及其快速位图缓存。
pub struct SignalQueue {
    bitmap: Signals,
    queue: VecDeque<PendingSignal>,
}

impl SignalQueue {
    /// 创建空队列。
    pub fn empty() -> Self {
        Self {
            bitmap: Signals::empty(),
            queue: VecDeque::new(),
        }
    }

    /// 返回当前 pending 信号位图。
    pub fn pending(&self) -> Signals {
        self.bitmap
    }

    /// 判断队列是否包含指定信号。
    pub fn contains(&self, signal: Signals) -> bool {
        self.bitmap.contains(signal)
    }

    /// 返回实际排队条目数。
    pub fn queued_count(&self) -> usize {
        self.queue.len()
    }

    /// 以无发送者信息的方式入队一个信号。
    pub fn enqueue_signal(&mut self, signal: Signals, si_code: usize) -> Result<(), isize> {
        self.enqueue_signal_with_sender(signal, si_code, 0)
    }

    pub fn enqueue_signal_with_sender(
        &mut self,
        signal: Signals,
        si_code: usize,
        si_pid: usize,
    ) -> Result<(), isize> {
        if signal.is_empty() {
            return Ok(());
        }
        self.enqueue(PendingSignal::from_signal_with_sender(
            signal, si_code, si_pid,
        )?)
    }

    /// 将完整 pending signal 入队。
    ///
    /// # Errors
    ///
    /// 队列达到上限时返回 `-EAGAIN`。非实时信号已经 pending 时视为成功，
    /// 并保持原有队列项。
    pub fn enqueue(&mut self, pending: PendingSignal) -> Result<(), isize> {
        if pending.signal.is_empty() {
            return Ok(());
        }
        if !is_realtime_signal(pending.signal) && self.bitmap.contains(pending.signal) {
            return Ok(());
        }
        if self.queue.len() >= MAX_QUEUED_SIGNALS {
            return Err(EAGAIN);
        }
        self.bitmap.insert(pending.signal);
        self.queue.push_back(pending);
        Ok(())
    }

    /// 取出第一个属于 `set` 的 pending signal。
    pub fn dequeue_matching(&mut self, set: Signals) -> Option<PendingSignal> {
        let index = self
            .queue
            .iter()
            .position(|pending| set.contains(pending.signal))?;
        let pending = self.queue.remove(index)?;
        self.refresh_bitmap_for(pending.signal);
        Some(pending)
    }

    /// 删除一个 pending signal，返回是否实际删除。
    pub fn remove_signal(&mut self, signal: Signals) -> bool {
        if let Some(index) = self
            .queue
            .iter()
            .position(|pending| pending.signal == signal)
        {
            self.queue.remove(index);
            self.refresh_bitmap_for(signal);
            true
        } else {
            false
        }
    }

    /// 删除 `signals` 集合中的所有 pending signal。
    pub fn remove_signals(&mut self, signals: Signals) {
        self.queue
            .retain(|pending| !signals.contains(pending.signal));
        self.rebuild_bitmap();
    }

    fn refresh_bitmap_for(&mut self, signal: Signals) {
        if !self.queue.iter().any(|pending| pending.signal == signal) {
            self.bitmap.remove(signal);
        }
    }

    fn rebuild_bitmap(&mut self) {
        self.bitmap = Signals::empty();
        for pending in self.queue.iter() {
            self.bitmap.insert(pending.signal);
        }
    }
}

impl Default for SignalQueue {
    fn default() -> Self {
        Self::empty()
    }
}

/// 判断信号是否属于实时信号范围。
pub fn is_realtime_signal(signal: Signals) -> bool {
    signal
        .to_signum()
        .map(|signum| signum >= 32)
        .unwrap_or(false)
}
