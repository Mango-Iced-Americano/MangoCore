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
    WakeBitset = 10,
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

fn wake_waiters(map: &mut BTreeMap<usize, WaitQueue>, key: usize, val: u32) -> isize {
    if let Some(mut wait_queue) = map.remove(&key) {
        let ret = wait_queue.wake_at_most(val as usize);
        if !wait_queue.is_empty() {
            map.insert(key, wait_queue);
        }
        ret as isize
    } else {
        0
    }
}

fn requeue_waiters(
    map: &mut BTreeMap<usize, WaitQueue>,
    key: usize,
    key_2: usize,
    val: u32,
    val2: usize,
) -> isize {
    let wake_cnt = if val != 0 {
        wake_waiters(map, key, val)
    } else {
        0
    };

    if key == key_2 {
        return wake_cnt;
    }

    if let Some(mut wait_queue) = map.remove(&key) {
        let mut wait_queue_2 = map.remove(&key_2).unwrap_or_else(WaitQueue::new);
        let mut requeue_cnt = 0;
        while requeue_cnt < val2 {
            if let Some(task) = wait_queue.pop_task() {
                wait_queue_2.add_task(task);
                requeue_cnt += 1;
            } else {
                break;
            }
        }
        if !wait_queue.is_empty() {
            map.insert(key, wait_queue);
        }
        if !wait_queue_2.is_empty() {
            map.insert(key_2, wait_queue_2);
        }
        wake_cnt + requeue_cnt as isize
    } else {
        wake_cnt
    }
}

// Futex wait 只读用户 word
fn do_futex_wait_until(
    futex_word: UserPtr<u32>,
    token: usize,
    futex_key: usize,
    val: u32,
    deadline: Option<TimeSpec>,
) -> isize {
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
        deadline,
        SUCCESS,
    );
    futex_table.lock().remove_empty(futex_key);

    match wait_result {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => ETIMEDOUT,
    }
}

// Futex wait 只读用户 word，timeout 参数为相对时间。
pub fn do_futex_wait(
    futex_word: UserPtr<u32>,
    token: usize,
    futex_key: usize,
    val: u32,
    timeout: Option<TimeSpec>,
) -> isize {
    do_futex_wait_until(
        futex_word,
        token,
        futex_key,
        val,
        timeout.map(|t| t + TimeSpec::now()),
    )
}

// FUTEX_WAIT_BITSET 的 timeout 参数为绝对时间。
pub fn do_futex_wait_bitset(
    futex_word: UserPtr<u32>,
    token: usize,
    futex_key: usize,
    val: u32,
    deadline: Option<TimeSpec>,
) -> isize {
    do_futex_wait_until(futex_word, token, futex_key, val, deadline)
}

/// 唤醒等待在全局 process-shared futex（物理地址 key）上的最多 val 个任务
pub fn futex_wake_shared(phys_key: usize, val: u32) -> isize {
    let mut shared = PROCESS_SHARED_FUTEX.lock();
    wake_waiters(&mut shared, phys_key, val)
}

pub fn futex_requeue_shared(phys_key: usize, phys_key_2: usize, val: u32, val2: usize) -> isize {
    let mut shared = PROCESS_SHARED_FUTEX.lock();
    requeue_waiters(&mut shared, phys_key, phys_key_2, val, val2)
}

/// Process-shared futex wait — 使用全局物理地址表
fn do_futex_wait_shared_until(
    futex_word: UserPtr<u32>,
    token: usize,
    val: u32,
    deadline: Option<TimeSpec>,
    phys_key: usize,
) -> isize {
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
        deadline,
        SUCCESS,
    );
    remove_empty_wait_queue(&mut PROCESS_SHARED_FUTEX.lock(), phys_key);

    match wait_result {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => ETIMEDOUT,
    }
}

pub fn do_futex_wait_shared(
    futex_word: UserPtr<u32>,
    token: usize,
    val: u32,
    timeout: Option<TimeSpec>,
    phys_key: usize,
) -> isize {
    do_futex_wait_shared_until(
        futex_word,
        token,
        val,
        timeout.map(|t| t + TimeSpec::now()),
        phys_key,
    )
}

pub fn do_futex_wait_bitset_shared(
    futex_word: UserPtr<u32>,
    token: usize,
    val: u32,
    deadline: Option<TimeSpec>,
    phys_key: usize,
) -> isize {
    do_futex_wait_shared_until(futex_word, token, val, deadline, phys_key)
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
        wake_waiters(&mut self.inner, futex_key, val)
    }

    fn wait_queue_mut(&mut self, futex_key: usize) -> &mut WaitQueue {
        wait_queue_for_key(&mut self.inner, futex_key)
    }

    fn remove_empty(&mut self, futex_key: usize) {
        remove_empty_wait_queue(&mut self.inner, futex_key);
    }

    /// 重新排列
    pub fn requeue(&mut self, futex_key: usize, futex_key_2: usize, val: u32, val2: usize) -> isize {
        requeue_waiters(&mut self.inner, futex_key, futex_key_2, val, val2)
    }

    /// 清空队列
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}
