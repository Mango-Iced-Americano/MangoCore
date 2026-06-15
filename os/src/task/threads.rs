/*
    此文件内容用于
    内容与RISCV版本相同，无需修改
*/
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{
    mm::UserPtr,
    syscall::errno::*,
    task::{
        block_current_and_run_next_with_lock_checked, current_task, current_task_ref,
        discard_non_actionable_unblocked_signals, has_actionable_signal, task_manager_counts,
        wait_with_timeout, TaskControlBlock,
    },
    timer::{get_clock_freq, get_time, TimeSpec, NSEC_PER_SEC},
};
use alloc::{collections::BTreeMap, sync::Arc};
use lazy_static::lazy_static;
use num_enum::FromPrimitive;

use super::manager::{WaitQueue, WaitResult};

#[cfg(target_arch = "loongarch64")]
const PRECISE_FUTEX_SPIN_NS: usize = 12_000_000;
#[cfg(not(target_arch = "loongarch64"))]
const PRECISE_FUTEX_SPIN_NS: usize = 1_250_000;
// la64 QEMU has a stable futex timeout return-to-user tail.  Keep a small
// bias, but leave enough room so LTP does not observe an early timeout.
#[cfg(target_arch = "loongarch64")]
const FUTEX_REL_TIMEOUT_EXIT_BIAS_NS: usize = 180_000;

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
static PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY: AtomicBool = AtomicBool::new(false);

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

#[derive(Clone, Copy)]
pub struct FutexWaitEntry {
    pub futex_word: UserPtr<u32>,
    pub futex_key: usize,
    pub val: u32,
}

fn wait_queue_for_key(map: &mut BTreeMap<usize, WaitQueue>, key: usize) -> &mut WaitQueue {
    map.entry(key).or_insert_with(WaitQueue::new)
}

fn shared_wait_queue_for_key(map: &mut BTreeMap<usize, WaitQueue>, key: usize) -> &mut WaitQueue {
    PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY.store(true, Ordering::Relaxed);
    wait_queue_for_key(map, key)
}

fn refresh_shared_futex_nonempty(map: &BTreeMap<usize, WaitQueue>) {
    PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY.store(!map.is_empty(), Ordering::Relaxed);
}

/// 清理 PROCESS_SHARED_FUTEX 中所有空 WaitQueue 条目。
/// 降频至每 64 个 tick 执行一次，避免频繁扫描 BTreeMap。
pub fn compact_shared_futex() {
    if !PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY.load(Ordering::Relaxed) {
        return;
    }
    static TICK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let t = TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if t % 64 != 0 {
        return;
    }
    let mut map = PROCESS_SHARED_FUTEX.lock();
    map.retain(|_, wq| {
        wq.compact_stale();
        !wq.is_empty()
    });
    refresh_shared_futex_nonempty(&map);
}

fn remove_empty_wait_queue(map: &mut BTreeMap<usize, WaitQueue>, key: usize) {
    if let Some(wq) = map.get_mut(&key) {
        wq.compact_stale();
        if wq.is_empty() {
            map.remove(&key);
        }
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

fn futex_wait_result_to_errno(wait_result: WaitResult) -> isize {
    match wait_result {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => ETIMEDOUT,
    }
}

fn check_waitv_values(entries: &[FutexWaitEntry], token: usize) -> Option<isize> {
    for entry in entries {
        match entry.futex_word.read(token) {
            Ok(value) if value == entry.val => {}
            Ok(_) => return Some(EAGAIN),
            Err(errno) => return Some(errno),
        }
    }
    None
}

fn deadline_expired(deadline: Option<TimeSpec>) -> bool {
    deadline
        .map(|deadline| TimeSpec::now() >= deadline)
        .unwrap_or(false)
}

#[inline(always)]
fn timespec_to_ticks(time: TimeSpec) -> usize {
    time.tv_sec
        .saturating_mul(get_clock_freq())
        .saturating_add(time.tv_nsec.saturating_mul(get_clock_freq()) / NSEC_PER_SEC)
}

fn finish_waitv_private(
    futex: &mut Futex,
    entries: &[FutexWaitEntry],
    task: &TaskControlBlock,
) -> Option<usize> {
    let mut woken_key = None;

    for (index, entry) in entries.iter().enumerate() {
        if entries[..index]
            .iter()
            .any(|finished| finished.futex_key == entry.futex_key)
        {
            continue;
        }
        let removed = {
            let wait_queue = futex.wait_queue_mut(entry.futex_key);
            wait_queue.finish_wait(task)
        };
        if !removed && woken_key.is_none() {
            woken_key = Some(entry.futex_key);
        }
        futex.remove_empty(entry.futex_key);
    }

    woken_key.and_then(|key| entries.iter().position(|entry| entry.futex_key == key))
}

fn finish_waitv_shared(
    futex: &mut BTreeMap<usize, WaitQueue>,
    entries: &[FutexWaitEntry],
    task: &TaskControlBlock,
) -> Option<usize> {
    let mut woken_key = None;

    for (index, entry) in entries.iter().enumerate() {
        if entries[..index]
            .iter()
            .any(|finished| finished.futex_key == entry.futex_key)
        {
            continue;
        }
        let removed = {
            let wait_queue = wait_queue_for_key(futex, entry.futex_key);
            wait_queue.finish_wait(task)
        };
        if !removed && woken_key.is_none() {
            woken_key = Some(entry.futex_key);
        }
        remove_empty_wait_queue(futex, entry.futex_key);
    }

    refresh_shared_futex_nonempty(futex);
    woken_key.and_then(|key| entries.iter().position(|entry| entry.futex_key == key))
}

fn try_single_thread_short_timeout(
    futex_word: UserPtr<u32>,
    token: usize,
    val: u32,
    deadline: Option<TimeSpec>,
) -> Option<WaitResult> {
    let deadline = deadline?;
    let now = TimeSpec::now();
    if deadline - now > TimeSpec::from_ms(150) {
        return None;
    }

    let task = current_task_ref().unwrap();
    if task.process.live_thread_count() != 1 {
        return None;
    }
    if task_manager_counts()
        .map(|(ready, _)| ready != 0)
        .unwrap_or(true)
    {
        return None;
    }

    let deadline_ticks = timespec_to_ticks(deadline);
    let mut spins = 0usize;
    loop {
        match futex_word.read(token) {
            Ok(value) if value == val => {}
            Ok(_) => return Some(WaitResult::Ready(EAGAIN)),
            Err(errno) => return Some(WaitResult::Ready(errno)),
        }
        if get_time() >= deadline_ticks {
            return Some(WaitResult::TimedOut);
        }
        if has_actionable_signal(&task) {
            return Some(WaitResult::Interrupted);
        }

        spins = spins.wrapping_add(1);
        if spins & 0x3ff == 0 {
            if task_manager_counts()
                .map(|(ready, _)| ready != 0)
                .unwrap_or(true)
            {
                return None;
            }
        }
        spin_loop();
    }
}

fn futex_wait_block_deadline(deadline: Option<TimeSpec>) -> Option<TimeSpec> {
    let deadline = deadline?;
    let now = TimeSpec::now();
    if now >= deadline {
        return Some(deadline);
    }

    let spin_guard = TimeSpec::from_ns(PRECISE_FUTEX_SPIN_NS);
    if deadline - now > spin_guard {
        Some(deadline - spin_guard)
    } else {
        Some(deadline)
    }
}

fn futex_relative_deadline(timeout: TimeSpec) -> TimeSpec {
    let mut deadline = timeout + TimeSpec::now();
    #[cfg(target_arch = "loongarch64")]
    {
        if timeout >= TimeSpec::from_ms(10) {
            deadline = deadline - TimeSpec::from_ns(FUTEX_REL_TIMEOUT_EXIT_BIAS_NS);
        }
    }
    deadline
}

fn futex_wait_tail_spin<T, Q, F>(
    lock: &spin::Mutex<T>,
    mut queue_of: Q,
    cond: &mut F,
    task: &Arc<TaskControlBlock>,
    deadline: TimeSpec,
    normal_wake_result: isize,
) -> WaitResult
where
    Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
    F: FnMut(&mut T) -> Option<isize>,
{
    let task_weak = Arc::downgrade(task);
    let deadline_ticks = timespec_to_ticks(deadline);

    loop {
        if get_time() >= deadline_ticks {
            let mut guard = lock.lock();
            queue_of(&mut guard).finish_wait(task.as_ref());
            return WaitResult::TimedOut;
        }

        {
            let mut guard = lock.lock();
            if let Some(res) = cond(&mut guard) {
                queue_of(&mut guard).finish_wait(task.as_ref());
                return WaitResult::Ready(res);
            }
            if !queue_of(&mut guard).contains(&task_weak) {
                return WaitResult::Ready(normal_wake_result);
            }
        }

        if has_actionable_signal(task) {
            let mut guard = lock.lock();
            queue_of(&mut guard).finish_wait(task.as_ref());
            return WaitResult::Interrupted;
        }
        discard_non_actionable_unblocked_signals(task);
        spin_loop();
    }
}

fn futex_wait_event_interruptible_timeout_locked<T, Q, F>(
    lock: &spin::Mutex<T>,
    mut queue_of: Q,
    mut cond: F,
    deadline: Option<TimeSpec>,
    normal_wake_result: isize,
) -> WaitResult
where
    Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
    F: FnMut(&mut T) -> Option<isize>,
{
    {
        let mut guard = lock.lock();
        if let Some(res) = cond(&mut guard) {
            return WaitResult::Ready(res);
        }
    }

    loop {
        let mut guard = lock.lock();
        if deadline_expired(deadline) {
            return WaitResult::TimedOut;
        }

        let task = current_task().unwrap();
        queue_of(&mut guard).prepare_to_wait(Arc::downgrade(&task));

        if let Some(res) = cond(&mut guard) {
            queue_of(&mut guard).finish_wait(task.as_ref());
            return WaitResult::Ready(res);
        }
        if deadline_expired(deadline) {
            queue_of(&mut guard).finish_wait(task.as_ref());
            return WaitResult::TimedOut;
        }
        if has_actionable_signal(&task) {
            queue_of(&mut guard).finish_wait(task.as_ref());
            return WaitResult::Interrupted;
        }
        discard_non_actionable_unblocked_signals(&task);

        let block_deadline = futex_wait_block_deadline(deadline);
        if let Some(real_deadline) = deadline {
            if block_deadline == Some(real_deadline) {
                drop(guard);
                return futex_wait_tail_spin(
                    lock,
                    queue_of,
                    &mut cond,
                    &task,
                    real_deadline,
                    normal_wake_result,
                );
            }
        }
        if let Some(block_deadline) = block_deadline {
            wait_with_timeout(Arc::downgrade(&task), block_deadline);
        }
        drop(task);

        block_current_and_run_next_with_lock_checked(guard, |task| {
            let no_signal = !has_actionable_signal(task);
            let not_timed_out = block_deadline
                .map(|deadline| TimeSpec::now() < deadline)
                .unwrap_or(true);
            no_signal && not_timed_out
        });

        let task = current_task_ref().unwrap();
        let mut guard = lock.lock();
        let removed = queue_of(&mut guard).finish_wait(task);
        drop(guard);
        task.acquire_inner_lock().refresh_real_timer();

        if !removed {
            return WaitResult::Ready(normal_wake_result);
        }
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
    super::perf::record_futex_wait(false, deadline.is_some());
    let futex_table = current_task_ref().unwrap().process.futex().clone();

    if let Some(wait_result) = try_single_thread_short_timeout(futex_word, token, val, deadline) {
        super::perf::record_futex_wait_result(wait_result);
        return futex_wait_result_to_errno(wait_result);
    }

    let wait_result = futex_wait_event_interruptible_timeout_locked(
        &futex_table,
        |futex| futex.wait_queue_mut(futex_key),
        |_: &mut Futex| match futex_word.read(token) {
            Ok(value) if value == val => None,
            Ok(value) => {
                Some(EAGAIN)
            }
            Err(errno) => Some(errno),
        },
        deadline,
        SUCCESS,
    );
    futex_table.lock().remove_empty(futex_key);
    super::perf::record_futex_wait_result(wait_result);

    futex_wait_result_to_errno(wait_result)
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
        timeout.map(futex_relative_deadline),
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

pub fn do_futex_waitv(
    entries: &[FutexWaitEntry],
    token: usize,
    deadline: Option<TimeSpec>,
) -> isize {
    if let Some(result) = check_waitv_values(entries, token) {
        return result;
    }

    let futex_table = current_task_ref().unwrap().process.futex().clone();

    loop {
        if deadline_expired(deadline) {
            return ETIMEDOUT;
        }

        let task = current_task().unwrap();
        let mut guard = futex_table.lock();
        for entry in entries {
            guard
                .wait_queue_mut(entry.futex_key)
                .prepare_to_wait(Arc::downgrade(&task));
        }

        if let Some(result) = check_waitv_values(entries, token) {
            finish_waitv_private(&mut guard, entries, task.as_ref());
            return result;
        }
        if deadline_expired(deadline) {
            finish_waitv_private(&mut guard, entries, task.as_ref());
            return ETIMEDOUT;
        }
        if has_actionable_signal(&task) {
            finish_waitv_private(&mut guard, entries, task.as_ref());
            return EINTR;
        }
        discard_non_actionable_unblocked_signals(&task);

        if let Some(deadline) = deadline {
            wait_with_timeout(Arc::downgrade(&task), deadline);
        }
        drop(task);

        block_current_and_run_next_with_lock_checked(guard, |task| {
            !has_actionable_signal(task)
                && !deadline_expired(deadline)
                && check_waitv_values(entries, token).is_none()
        });

        let task = current_task_ref().unwrap();
        let mut guard = futex_table.lock();
        if let Some(index) = finish_waitv_private(&mut guard, entries, task) {
            task.acquire_inner_lock().refresh_real_timer();
            return index as isize;
        }
        drop(guard);
        task.acquire_inner_lock().refresh_real_timer();
    }
}

/// 唤醒等待在全局 process-shared futex（物理地址 key）上的最多 val 个任务
pub fn futex_wake_shared(phys_key: usize, val: u32) -> isize {
    let mut shared = PROCESS_SHARED_FUTEX.lock();
    let woke = wake_waiters(&mut shared, phys_key, val);
    refresh_shared_futex_nonempty(&shared);
    super::perf::record_futex_wake(true, woke);
    woke
}

pub fn futex_requeue_shared(phys_key: usize, phys_key_2: usize, val: u32, val2: usize) -> isize {
    let mut shared = PROCESS_SHARED_FUTEX.lock();
    let ret = requeue_waiters(&mut shared, phys_key, phys_key_2, val, val2);
    refresh_shared_futex_nonempty(&shared);
    ret
}

pub fn do_futex_waitv_shared(
    entries: &[FutexWaitEntry],
    token: usize,
    deadline: Option<TimeSpec>,
) -> isize {
    if let Some(result) = check_waitv_values(entries, token) {
        return result;
    }

    loop {
        if deadline_expired(deadline) {
            return ETIMEDOUT;
        }

        let task = current_task().unwrap();
        let mut guard = PROCESS_SHARED_FUTEX.lock();
        for entry in entries {
            shared_wait_queue_for_key(&mut guard, entry.futex_key)
                .prepare_to_wait(Arc::downgrade(&task));
        }

        if let Some(result) = check_waitv_values(entries, token) {
            finish_waitv_shared(&mut guard, entries, task.as_ref());
            return result;
        }
        if deadline_expired(deadline) {
            finish_waitv_shared(&mut guard, entries, task.as_ref());
            return ETIMEDOUT;
        }
        if has_actionable_signal(&task) {
            finish_waitv_shared(&mut guard, entries, task.as_ref());
            return EINTR;
        }
        discard_non_actionable_unblocked_signals(&task);

        if let Some(deadline) = deadline {
            wait_with_timeout(Arc::downgrade(&task), deadline);
        }
        drop(task);

        block_current_and_run_next_with_lock_checked(guard, |task| {
            !has_actionable_signal(task)
                && !deadline_expired(deadline)
                && check_waitv_values(entries, token).is_none()
        });

        let task = current_task_ref().unwrap();
        let mut guard = PROCESS_SHARED_FUTEX.lock();
        if let Some(index) = finish_waitv_shared(&mut guard, entries, task) {
            task.acquire_inner_lock().refresh_real_timer();
            return index as isize;
        }
        drop(guard);
        task.acquire_inner_lock().refresh_real_timer();
    }
}

/// Process-shared futex wait — 使用全局物理地址表
fn do_futex_wait_shared_until(
    futex_word: UserPtr<u32>,
    token: usize,
    val: u32,
    deadline: Option<TimeSpec>,
    phys_key: usize,
) -> isize {
    super::perf::record_futex_wait(true, deadline.is_some());
    if let Some(wait_result) = try_single_thread_short_timeout(futex_word, token, val, deadline) {
        super::perf::record_futex_wait_result(wait_result);
        return futex_wait_result_to_errno(wait_result);
    }

    let wait_result = futex_wait_event_interruptible_timeout_locked(
        &PROCESS_SHARED_FUTEX,
        |shared| shared_wait_queue_for_key(shared, phys_key),
        |_: &mut BTreeMap<usize, WaitQueue>| match futex_word.read(token) {
            Ok(value) if value == val => None,
            Ok(value) => {
                Some(EAGAIN)
            }
            Err(errno) => Some(errno),
        },
        deadline,
        SUCCESS,
    );
    {
        let mut shared = PROCESS_SHARED_FUTEX.lock();
        remove_empty_wait_queue(&mut shared, phys_key);
        refresh_shared_futex_nonempty(&shared);
    }
    super::perf::record_futex_wait_result(wait_result);

    futex_wait_result_to_errno(wait_result)
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
        timeout.map(futex_relative_deadline),
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
        let woke = wake_waiters(&mut self.inner, futex_key, val);
        super::perf::record_futex_wake(false, woke);
        woke
    }

    fn wait_queue_mut(&mut self, futex_key: usize) -> &mut WaitQueue {
        wait_queue_for_key(&mut self.inner, futex_key)
    }

    fn remove_empty(&mut self, futex_key: usize) {
        remove_empty_wait_queue(&mut self.inner, futex_key);
    }

    /// 重新排列
    pub fn requeue(
        &mut self,
        futex_key: usize,
        futex_key_2: usize,
        val: u32,
        val2: usize,
    ) -> isize {
        requeue_waiters(&mut self.inner, futex_key, futex_key_2, val, val2)
    }

    /// 清空队列
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}
