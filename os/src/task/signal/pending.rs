use alloc::collections::VecDeque;

use crate::syscall::errno::{EAGAIN, EINVAL};

use super::{SigInfo, Signals};

const MAX_QUEUED_SIGNALS: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct PendingSignal {
    pub signal: Signals,
    pub siginfo: SigInfo,
}

impl PendingSignal {
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
pub struct SignalQueue {
    bitmap: Signals,
    queue: VecDeque<PendingSignal>,
}

impl SignalQueue {
    pub fn empty() -> Self {
        Self {
            bitmap: Signals::empty(),
            queue: VecDeque::new(),
        }
    }

    pub fn pending(&self) -> Signals {
        self.bitmap
    }

    pub fn contains(&self, signal: Signals) -> bool {
        self.bitmap.contains(signal)
    }

    pub fn queued_count(&self) -> usize {
        self.queue.len()
    }

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

    pub fn dequeue_matching(&mut self, set: Signals) -> Option<PendingSignal> {
        let index = self
            .queue
            .iter()
            .position(|pending| set.contains(pending.signal))?;
        let pending = self.queue.remove(index)?;
        self.refresh_bitmap_for(pending.signal);
        Some(pending)
    }

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

pub fn is_realtime_signal(signal: Signals) -> bool {
    signal.to_signum().map(|signum| signum >= 32).unwrap_or(false)
}
