/*
    此文件内容用于
    内容与RISCV版本相同，无需修改
*/
use crate::{syscall::errno::*, task::current_task, timer::TimeSpec};
use alloc::{collections::BTreeMap, sync::Arc};
use lazy_static::lazy_static;
use log::*;
use num_enum::FromPrimitive;

use super::{
    block_current_and_run_next, has_actionable_signal,
    manager::{wait_with_timeout, WaitQueue},
};

#[allow(unused)]
#[derive(Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u32)]
/// 定义了Futex支持的操作类型
pub enum FutexCmd {
    /// This  operation  tests  that  the value at the futex
    /// word pointed to by the address uaddr still  contains
    /// the expected value val, and if so, then sleeps wait‐
    /// ing for a FUTEX_WAKE operation on  the  futex  word.
    /// The load of the value of the futex word is an atomic
    /// memory access (i.e., using atomic  machine  instruc‐
    /// tions  of  the respective architecture).  This load,
    /// the comparison with the expected value, and starting
    /// to  sleep  are  performed atomically and totally or‐
    /// dered with respect to other futex operations on  the
    /// same  futex word.  If the thread starts to sleep, it
    /// is considered a waiter on this futex word.   If  the
    /// futex  value does not match val, then the call fails
    /// immediately with the error EAGAIN.
    Wait = 0,
    /// This operation wakes at most val of the waiters that
    /// are waiting (e.g., inside FUTEX_WAIT) on  the  futex
    /// word  at  the  address uaddr.  Most commonly, val is
    /// specified as either 1 (wake up a single  waiter)  or
    /// INT_MAX (wake up all waiters).  No guarantee is pro‐
    /// vided about which waiters are awoken (e.g., a waiter
    /// with  a higher scheduling priority is not guaranteed
    /// to be awoken in preference to a waiter with a  lower
    /// priority).
    Wake = 1,
    Fd = 2,
    Requeue = 3,
    CmpRequeue = 4,
    WakeOp = 5,
    LockPi = 6,
    UnlockPi = 7,
    TrylockPi = 8,
    WaitBitset = 9,
    #[num_enum(default)]
    // 不在范围内，默认值为Invalid
    Invalid,
}

lazy_static! {
    /// 进程间共享 futex 的全局等待表，key = 物理地址
    pub static ref PROCESS_SHARED_FUTEX: spin::Mutex<BTreeMap<usize, WaitQueue>> =
        spin::Mutex::new(BTreeMap::new());
}

/// Fast Userspace Mutex
/// 快速用户空间互斥锁
/// # 作用
/// + 用于存储等待队列
/// # 参数
/// + key：usize
/// + value：WaitQueue
pub struct Futex {
    inner: BTreeMap<usize, WaitQueue>,
}

// Futex wait 只读用户 word
pub fn do_futex_wait(futex_word: &u32, val: u32, timeout: Option<TimeSpec>) -> isize {
    // 超时时间换成绝对时间
    let timeout = timeout.map(|t| t + TimeSpec::now());

    // 地址当作等待队列 key
    let futex_word_addr = futex_word as *const u32 as usize;

    // 拿当前任务
    let task = current_task().unwrap();

    // 拿 futex 锁准备改等待队列
    let mut futex = task.futex.lock();

    // 持锁后再读一次，避免丢 wake
    if *futex_word != val {
        drop(futex);
        trace!(
            "[futex] --wait-- **not match** futex: {:X}, val: {:X}",
            *futex_word,
            val
        );
        return EAGAIN;
    }

    // 取出这个地址对应的等待队列
    let mut wait_queue = if let Some(wait_queue) = futex.inner.remove(&futex_word_addr) {
        wait_queue
    } else {
        WaitQueue::new()
    };

    // 当前任务挂到等待队列里
    // 用弱引用避免循环引用
    wait_queue.add_task(Arc::downgrade(&task));

    // 等待队列放回去
    futex.inner.insert(futex_word_addr, wait_queue);

    // 有超时就挂到定时队列
    if let Some(timeout) = timeout {
        trace!("[do_futex_wait] sleep with timeout: {:?}", timeout);
        wait_with_timeout(Arc::downgrade(&task), timeout);
    }

    // 睡前先放锁
    drop(futex);
    drop(task);

    // 阻塞当前任务并切走
    block_current_and_run_next();

    // 醒来后重新拿当前任务
    let task = current_task().unwrap();

    // 检查有没有信号打断
    {
        let inner = task.acquire_inner_lock();
        let pending = inner.sigpending.difference(inner.sigmask);
        if !pending.is_empty() {
            drop(inner);
            // 只有真要处理的信号才返回 EINTR
            if has_actionable_signal(&task) {
                return EINTR;
            }
        }
    }

    // 没有信号打断就成功
    SUCCESS
}

/// 唤醒等待在全局 process-shared futex（物理地址 key）上的最多 val 个任务
pub fn futex_wake_shared(phys_key: usize, val: u32) -> isize {
    let mut shared = PROCESS_SHARED_FUTEX.lock();
    if let Some(mut wait_queue) = shared.remove(&phys_key) {
        let ret = wait_queue.wake_at_most(val as usize);
        if !wait_queue.is_empty() {
            shared.insert(phys_key, wait_queue);
        }
        ret as isize
    } else {
        0
    }
}

/// Process-shared futex wait — 使用全局物理地址表
pub fn do_futex_wait_shared(
    futex_word: &u32,
    val: u32,
    timeout: Option<TimeSpec>,
    phys_key: usize,
) -> isize {
    let timeout = timeout.map(|t| t + TimeSpec::now());
    let task = current_task().unwrap();
    let mut shared = PROCESS_SHARED_FUTEX.lock();

    // 【修复 TOCTOU】：必须在持有全局锁之后重新读取 *futex_word
    if *futex_word != val {
        drop(shared);
        trace!(
            "[futex-shared] --wait-- **not match** futex: {:X}, val: {:X}",
            *futex_word,
            val
        );
        return EAGAIN;
    }

    let mut wait_queue = shared.remove(&phys_key).unwrap_or_else(WaitQueue::new);
    wait_queue.add_task(Arc::downgrade(&task));
    shared.insert(phys_key, wait_queue);
    if let Some(timeout) = timeout {
        trace!("[do_futex_wait_shared] sleep with timeout: {:?}", timeout);
        wait_with_timeout(Arc::downgrade(&task), timeout);
    }
    drop(shared);
    drop(task);
    block_current_and_run_next();
    // 唤醒后检查信号
    let task = current_task().unwrap();
    {
        let inner = task.acquire_inner_lock();
        let pending = inner.sigpending.difference(inner.sigmask);
        if !pending.is_empty() {
            drop(inner);
            if has_actionable_signal(&task) {
                return EINTR;
            }
        }
    }
    SUCCESS
}

// Futex的方法实现
impl Futex {
    /// 创建一个新的Futex
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }

    /// 唤醒等待在指定 Futex 地址上的最多 val 个任务
    pub fn wake(&mut self, futex_word_addr: usize, val: u32) -> isize {
        if let Some(mut wait_queue) = self.inner.remove(&futex_word_addr) {
            let ret = wait_queue.wake_at_most(val as usize);
            if !wait_queue.is_empty() {
                self.inner.insert(futex_word_addr, wait_queue);
            }
            ret as isize
        } else {
            0
        }
    }

    /// 重新排列
    pub fn requeue(&mut self, futex_word: &u32, futex_word_2: &u32, val: u32, val2: u32) -> isize {
        let futex_word_addr = futex_word as *const u32 as usize;
        let futex_word_addr_2 = futex_word_2 as *const u32 as usize;
        let wake_cnt = if val != 0 {
            self.wake(futex_word_addr, val)
        } else {
            0
        };
        if let Some(mut wait_queue) = self.inner.remove(&futex_word_addr) {
            let mut wait_queue_2 = if let Some(wait_queue) = self.inner.remove(&futex_word_addr_2) {
                wait_queue
            } else {
                WaitQueue::new()
            };
            let mut requeue_cnt = 0;
            if val2 != 0 {
                while let Some(task) = wait_queue.pop_task() {
                    wait_queue_2.add_task(task);
                    requeue_cnt += 1;
                    if requeue_cnt == val2 as isize {
                        break;
                    }
                }
            }
            if !wait_queue.is_empty() {
                self.inner.insert(futex_word_addr, wait_queue);
            }
            if !wait_queue_2.is_empty() {
                self.inner.insert(futex_word_addr_2, wait_queue_2);
            }
            wake_cnt + requeue_cnt
        } else {
            wake_cnt
        }
    }

    /// 清空队列
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}
