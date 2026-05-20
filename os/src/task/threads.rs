/*
    此文件内容用于
    内容与RISCV版本相同，无需修改
*/
use crate::{mm::UserPtr, syscall::errno::*, task::current_task, timer::TimeSpec};
use alloc::collections::BTreeMap;
use lazy_static::lazy_static;
use log::*;
use num_enum::FromPrimitive;

use super::manager::{WaitQueue, WaitResult};

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

fn wait_queue_for_key(map: &mut BTreeMap<usize, WaitQueue>, key: usize) -> &mut WaitQueue {
    if !map.contains_key(&key) {
        map.insert(key, WaitQueue::new());
    }
    map.get_mut(&key).unwrap()
}

fn remove_empty_wait_queue(map: &mut BTreeMap<usize, WaitQueue>, key: usize) {
    if map.get(&key).map(|wait_queue| wait_queue.is_empty()).unwrap_or(false) {
        map.remove(&key);
    }
}

// Futex wait 只读用户 word
pub fn do_futex_wait(
    futex_word: UserPtr<u32>,
    token: usize,
    futex_key: usize,
    val: u32,
    timeout: Option<TimeSpec>,
) -> isize {
    // 超时时间换成绝对时间
    let timeout = timeout.map(|t| t + TimeSpec::now());

    let task = current_task().unwrap();
    let futex_table = task.process.futex().clone();
    drop(task);

    let wait_result = WaitQueue::wait_event_interruptible_timeout_locked_with_wake_result(
        &futex_table,
        |futex| futex.wait_queue_mut(futex_key),
        |_: &mut Futex| match futex_word.read(token) {
            Ok(value) if value == val => None,
            Ok(value) => {
                trace!(
                    "[futex] --wait-- **not match** futex: {:X}, val: {:X}",
                    value,
                    val
                );
                Some(EAGAIN)
            }
            Err(errno) => Some(errno),
        },
        timeout,
        SUCCESS,
    );
    futex_table.lock().remove_empty(futex_key);

    match wait_result {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => ETIMEDOUT,
    }
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
    futex_word: UserPtr<u32>,
    token: usize,
    val: u32,
    timeout: Option<TimeSpec>,
    phys_key: usize,
) -> isize {
    let timeout = timeout.map(|t| t + TimeSpec::now());
    let wait_result = WaitQueue::wait_event_interruptible_timeout_locked_with_wake_result(
        &PROCESS_SHARED_FUTEX,
        |shared| wait_queue_for_key(shared, phys_key),
        |_: &mut BTreeMap<usize, WaitQueue>| match futex_word.read(token) {
            Ok(value) if value == val => None,
            Ok(value) => {
                trace!(
                    "[futex-shared] --wait-- **not match** futex: {:X}, val: {:X}",
                    value,
                    val
                );
                Some(EAGAIN)
            }
            Err(errno) => Some(errno),
        },
        timeout,
        SUCCESS,
    );
    remove_empty_wait_queue(&mut PROCESS_SHARED_FUTEX.lock(), phys_key);

    match wait_result {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => ETIMEDOUT,
    }
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
    pub fn wake(&mut self, futex_key: usize, val: u32) -> isize {
        if let Some(mut wait_queue) = self.inner.remove(&futex_key) {
            let ret = wait_queue.wake_at_most(val as usize);
            if !wait_queue.is_empty() {
                self.inner.insert(futex_key, wait_queue);
            }
            ret as isize
        } else {
            0
        }
    }

    fn wait_queue_mut(&mut self, futex_key: usize) -> &mut WaitQueue {
        wait_queue_for_key(&mut self.inner, futex_key)
    }

    fn remove_empty(&mut self, futex_key: usize) {
        remove_empty_wait_queue(&mut self.inner, futex_key);
    }

    /// 重新排列
    pub fn requeue(&mut self, futex_key: usize, futex_key_2: usize, val: u32, val2: u32) -> isize {
        let wake_cnt = if val != 0 {
            self.wake(futex_key, val)
        } else {
            0
        };
        if let Some(mut wait_queue) = self.inner.remove(&futex_key) {
            let mut wait_queue_2 = if let Some(wait_queue) = self.inner.remove(&futex_key_2) {
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
                self.inner.insert(futex_key, wait_queue);
            }
            if !wait_queue_2.is_empty() {
                self.inner.insert(futex_key_2, wait_queue_2);
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
