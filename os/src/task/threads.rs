//! Futex 等待、唤醒和重排。
//!
//! 私有 futex 等待队列存放在进程内 `FutexTable` 中；process-shared futex
//! 使用全局表。每次等待都有可跟随 requeue 迁移的独立 waiter，因此超时、
//! 信号和 `waitv` 能区分真实 wake 与普通调度唤醒。
//!
//! # Locking
//!
//! futex table 锁只保护等待队列映射。阻塞前通过
//! `block_current_and_run_next_with_lock_checked` 释放该锁，唤醒路径不跨等待点持锁。

use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{
    hal::PageTableImpl,
    mm::{AddressSpace, AddressSpaceInner, FaultAccess, FrameTracker, PhysAddr, UserPtr, VirtAddr},
    syscall::errno::*,
    task::{
        block_current_and_run_next_with_lock_checked, current_task,
        discard_non_actionable_unblocked_signals, has_actionable_signal, task_manager_counts,
        wait_with_timeout, wake_interruptible, TaskControlBlock, TaskStatus,
    },
    timer::{get_time, timespec_to_ticks_ceil, TimeSpec},
};
use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
    vec::Vec,
};
use lazy_static::lazy_static;
use num_enum::FromPrimitive;

use super::manager::WaitResult;

#[cfg(target_arch = "loongarch64")]
const PRECISE_FUTEX_SPIN_NS: usize = 12_000_000;
#[cfg(not(target_arch = "loongarch64"))]
const PRECISE_FUTEX_SPIN_NS: usize = 1_250_000;
// LA64 QEMU has a stable futex timeout return-to-user tail. The 2K1000LA
// board does not have that tail, so applying this bias there causes an early
// timeout observable by futex_wait05.
#[cfg(all(target_arch = "loongarch64", feature = "board_laqemu"))]
const FUTEX_REL_TIMEOUT_EXIT_BIAS_NS: usize = 180_000;
#[cfg(all(target_arch = "loongarch64", not(feature = "board_laqemu")))]
const FUTEX_REL_TIMEOUT_EXIT_BIAS_NS: usize = 0;

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
    /// 进程间共享 futex 的全局等待表。
    pub static ref PROCESS_SHARED_FUTEX: spin::Mutex<FutexTable> =
        spin::Mutex::new(FutexTable::new());
}
static PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY: AtomicBool = AtomicBool::new(false);

/// 一个 process-shared futex word 的稳定内核身份。
///
/// 仅保存 PPN 会在原页回收、同一 PPN 分配给新页后发生 ABA，
/// 使无关的新页错误命中旧等待队列。该类型用 `Arc` 保持原
/// backing 存活；同一页内再用字节偏移区分不同 futex word。
#[derive(Clone)]
pub struct SharedFutexKey {
    backing: Arc<FrameTracker>,
    page_offset: usize,
}

impl SharedFutexKey {
    pub fn new(backing: Arc<FrameTracker>, page_offset: usize) -> Self {
        debug_assert!(page_offset < crate::config::PAGE_SIZE);
        Self {
            backing,
            page_offset,
        }
    }

    fn queue_key(&self) -> QueueKey {
        QueueKey {
            backing_identity: Arc::as_ptr(&self.backing) as usize,
            word_offset: self.page_offset,
        }
    }
}

impl PartialEq for SharedFutexKey {
    fn eq(&self, other: &Self) -> bool {
        self.page_offset == other.page_offset && Arc::ptr_eq(&self.backing, &other.backing)
    }
}

impl Eq for SharedFutexKey {}

/// `FutexTable` 内部的有序查找键。
///
/// 私有 futex 已由每进程 `FutexTable` 隔离，因此用
/// `(0, user_va)`；共享 futex 用 `(Arc 对象身份, 页内偏移)`。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct QueueKey {
    backing_identity: usize,
    word_offset: usize,
}

impl QueueKey {
    fn private(user_va: usize) -> Self {
        Self {
            backing_identity: 0,
            word_offset: user_va,
        }
    }
}

/// 一次 futex 等待的稳定注册项。
///
/// Linux 的 `futex_q` 会跟随 requeue 更新 key 和 bucket 归属。MangoCore
/// 用一个安全的 `Arc` 对象表达同一语义，避免把 syscall 栈上指针
/// 发布给其它 CPU。所有 key 变更和队列增删都由所属 `FutexTable`
/// 锁串行化；原子只用于在等待任务和唤醒 CPU 之间发布结果。
///
/// 同一任务在 `futex_waitv` 中可以同时有多个注册项，因此每个 waiter
/// 都有独立身份，不能只依赖 TCB 指针判断究竟是哪一项被唤醒。
struct FutexWaiter {
    task: Weak<TaskControlBlock>,
    current_backing_identity: AtomicUsize,
    current_word_offset: AtomicUsize,
    woken: AtomicBool,
}

impl FutexWaiter {
    fn new(task: Weak<TaskControlBlock>, key: QueueKey) -> Self {
        Self {
            task,
            current_backing_identity: AtomicUsize::new(key.backing_identity),
            current_word_offset: AtomicUsize::new(key.word_offset),
            woken: AtomicBool::new(false),
        }
    }

    /// 读取 waiter 当前所属队列。
    ///
    /// 调用者必须持有所属 `FutexTable` 的外层锁。两个原子字段
    /// 只是为了使 waiter 可安全共享，不表示它们可以无锁原子更新。
    fn queue_key_locked(&self) -> QueueKey {
        QueueKey {
            backing_identity: self.current_backing_identity.load(Ordering::Relaxed),
            word_offset: self.current_word_offset.load(Ordering::Relaxed),
        }
    }

    /// 在同一把 `FutexTable` 锁下更新 requeue 后的归属。
    fn move_to_locked(&self, key: QueueKey) {
        self.current_backing_identity
            .store(key.backing_identity, Ordering::Relaxed);
        self.current_word_offset
            .store(key.word_offset, Ordering::Relaxed);
    }

    fn mark_woken(&self) {
        self.woken.store(true, Ordering::Release);
    }

    fn was_woken(&self) -> bool {
        self.woken.load(Ordering::Acquire)
    }
}

struct FutexQueue {
    waiters: VecDeque<Arc<FutexWaiter>>,
    /// 只有 shared 队列需要 pin backing；私有队列为 `None`。
    /// 每个非空 key 仅保留一份 `Arc`，而不是每个 waiter 一份。
    backing_pin: Option<Arc<FrameTracker>>,
}

impl FutexQueue {
    fn new(backing_pin: Option<Arc<FrameTracker>>) -> Self {
        Self {
            waiters: VecDeque::new(),
            backing_pin,
        }
    }

    fn assert_same_backing(&self, backing: Option<&Arc<FrameTracker>>) {
        debug_assert!(match (&self.backing_pin, backing) {
            (None, None) => true,
            (Some(queued), Some(requested)) => Arc::ptr_eq(queued, requested),
            _ => false,
        });
    }

    fn is_empty(&self) -> bool {
        self.waiters.is_empty()
    }

    fn compact_stale(&mut self) {
        self.waiters.retain(|waiter| waiter.task.strong_count() > 0);
    }

    fn enqueue(&mut self, waiter: Arc<FutexWaiter>) {
        self.waiters.push_back(waiter);
    }

    /// 只删除这一次等待，不会误删同一 TCB 的其它 waitv 注册项。
    fn remove(&mut self, waiter: &Arc<FutexWaiter>) -> bool {
        let old_len = self.waiters.len();
        self.waiters.retain(|queued| !Arc::ptr_eq(queued, waiter));
        self.waiters.len() != old_len
    }

    /// 在任务可被调度前先发布真实 futex wake，使 timeout/wake 竞争
    /// 由 futex table 锁决定唯一胜者。
    fn wake_at_most(&mut self, limit: usize) -> usize {
        let mut wake_count = 0usize;
        let scan_count = self.waiters.len();

        // 只扫描调用时已经存在的条目；未被消费的条目推回同一个
        // VecDeque，避免在 futex table 自旋锁内额外分配临时 Vec。
        for _ in 0..scan_count {
            let waiter = self.waiters.pop_front().unwrap();
            let Some(task) = waiter.task.upgrade() else {
                continue;
            };
            let status = task.task_status();
            let wakeable = matches!(
                status,
                TaskStatus::Blocking(_)
                    | TaskStatus::Blocked
                    | TaskStatus::Queued(_)
                    | TaskStatus::Migrating
                    | TaskStatus::Running(_)
            );
            // New/Zombie 不可能是合法 futex waiter，直接丢弃这个坏条目。
            if !wakeable {
                continue;
            }
            if wake_count >= limit {
                self.waiters.push_back(waiter);
                continue;
            }

            waiter.mark_woken();
            task.wait_timer_generation.fetch_add(1, Ordering::Relaxed);
            if matches!(status, TaskStatus::Blocking(_) | TaskStatus::Blocked) {
                // FutexTable 的外层锁仍由调用者持有；先移除 waiter 并发布
                // woken，再进入既有 table -> TASK_MANAGER 单向唤醒顺序。
                let _ = wake_interruptible(task);
            }
            wake_count += 1;
        }
        wake_count
    }

    /// 搬运完整注册项，并在目标队列可见前更新 waiter 的归属 key。
    fn requeue_to(&mut self, target: &mut Self, target_key: QueueKey, limit: usize) -> usize {
        let mut moved = 0usize;
        while moved < limit {
            let Some(waiter) = self.waiters.pop_front() else {
                break;
            };
            if waiter.task.strong_count() == 0 {
                continue;
            }
            waiter.move_to_locked(target_key);
            target.waiters.push_back(waiter);
            moved += 1;
        }
        moved
    }
}

/// 一个进程或全局 shared futex 共用的等待表。
///
/// 表锁同时是 wait 注册、wake 移除、requeue 搬运和 timeout/signal
/// 撤销的唯一线性化点。
pub struct FutexTable {
    queues: BTreeMap<QueueKey, FutexQueue>,
}

/// 从一个用户 `futex_waitv` 条目解析出的内核等待条件。
pub struct FutexWaitSpec {
    futex_word: UserPtr<u32>,
    queue_key: QueueKey,
    shared_backing: Option<Arc<FrameTracker>>,
    val: u32,
}

/// 一次可重试 futex table 操作的结果。
///
/// `Retry` 只在等待项尚未发布、队列尚未迁移时返回。syscall 层必须先在锁外
/// 重新 fault-in 并重算 shared key，不能把它直接暴露成用户可见 errno。
pub enum FutexOpOutcome {
    Complete(isize),
    Retry,
}

impl FutexWaitSpec {
    pub fn private(futex_word: UserPtr<u32>, user_va: usize, val: u32) -> Self {
        Self {
            futex_word,
            queue_key: QueueKey::private(user_va),
            shared_backing: None,
            val,
        }
    }

    pub fn shared(futex_word: UserPtr<u32>, key: SharedFutexKey, val: u32) -> Self {
        let queue_key = key.queue_key();
        Self {
            futex_word,
            queue_key,
            shared_backing: Some(key.backing),
            val,
        }
    }
}

fn refresh_shared_futex_nonempty(table: &FutexTable) {
    PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY.store(!table.is_empty(), Ordering::Relaxed);
}

/// 清理 PROCESS_SHARED_FUTEX 中所有空队列条目。
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
    let mut table = PROCESS_SHARED_FUTEX.lock();
    table.compact_stale();
    refresh_shared_futex_nonempty(&table);
}

fn futex_wait_result_to_errno(wait_result: WaitResult) -> isize {
    match wait_result {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => ETIMEDOUT,
    }
}

enum FutexWordCheck {
    Matches,
    Mismatch,
    Retry,
}

/// 在已经持有 VM read guard 时解析一个 futex word，不触发缺页或 PTE 修改。
///
/// shared futex 还要复核调用方锁外取得的 backing。失败统一交给外层重试，
/// 不能在持有 futex table 锁时等待 VM 锁或进入 fault-in。
fn resolve_futex_word_nofault(
    address_space: &AddressSpaceInner<PageTableImpl>,
    futex_word: UserPtr<u32>,
    expected_backing: Option<&Arc<FrameTracker>>,
) -> Option<PhysAddr> {
    if let Some(expected) = expected_backing {
        let current = address_space
            .futex_shared_backing(VirtAddr::from(futex_word.addr()))
            .ok()
            .flatten()?;
        if !Arc::ptr_eq(expected, &current) {
            return None;
        }
    }

    address_space
        .resolve_user_va(VirtAddr::from(futex_word.addr()), FaultAccess::Load)
        .ok()
}

/// 在 futex table 锁内对所有 word 做无缺页复查。
///
/// VM 锁只能 try-lock：锁忙、PTE 变化或 shared backing 已改变都返回
/// `Retry`，由上层先释放 table 锁，再走 faultable 读取和 key 重算。
fn check_words_nofault(
    vm: &AddressSpace<PageTableImpl>,
    entries: &[FutexWaitSpec],
) -> FutexWordCheck {
    vm.try_read(|address_space| {
        for entry in entries {
            let Some(pa) = resolve_futex_word_nofault(
                address_space,
                entry.futex_word,
                entry.shared_backing.as_ref(),
            ) else {
                return FutexWordCheck::Retry;
            };
            // Safety: syscall 层已验证 u32 对齐；VM try-read guard 保证
            // PTE 与 frame 在这次硬件宽度读取期间不会被并发撤销。
            let value = unsafe { pa.direct_map_ptr().cast::<u32>().read_volatile() };
            if value != entry.val {
                return FutexWordCheck::Mismatch;
            }
        }
        FutexWordCheck::Matches
    })
    .unwrap_or(FutexWordCheck::Retry)
}

/// 在 futex table 锁内复核 requeue 的两端映射，并可选比较 source word。
///
/// `expected == None` 对应 FUTEX_REQUEUE：shared source 复核 backing/PTE，private
/// source 保留 VA-key 语义，target 均复核当前映射；`Some` 对应 FUTEX_CMP_REQUEUE，
/// source load、比较和后续 requeue 共享同一个 table 临界区。
fn check_requeue_nofault(
    vm: &AddressSpace<PageTableImpl>,
    source_word: UserPtr<u32>,
    source_backing: Option<&Arc<FrameTracker>>,
    target_word: UserPtr<u32>,
    target_backing: Option<&Arc<FrameTracker>>,
    expected: Option<u32>,
) -> FutexWordCheck {
    vm.try_read(|address_space| {
        let source_pa = if expected.is_some() || source_backing.is_some() {
            let Some(source_pa) =
                resolve_futex_word_nofault(address_space, source_word, source_backing)
            else {
                return FutexWordCheck::Retry;
            };
            Some(source_pa)
        } else {
            // 普通 private REQUEUE 不读取 source word；它的 key 是进程内 VA，
            // 与 PTE/backing 无关。额外解析 PTE 会错误收紧既有 ABI。
            None
        };
        if resolve_futex_word_nofault(address_space, target_word, target_backing).is_none() {
            return FutexWordCheck::Retry;
        }

        if let Some(expected) = expected {
            let Some(source_pa) = source_pa else {
                return FutexWordCheck::Retry;
            };
            // Safety: 两个地址已在 syscall 层验证 u32 对齐，VM guard 在读取期间
            // 冻结 PTE；对齐的硬件 u32 load 是本次 compare 的线性化点。
            let value = unsafe { source_pa.direct_map_ptr().cast::<u32>().read_volatile() };
            if value != expected {
                return FutexWordCheck::Mismatch;
            }
        }
        FutexWordCheck::Matches
    })
    .unwrap_or(FutexWordCheck::Retry)
}

fn deadline_expired(deadline: Option<TimeSpec>) -> bool {
    deadline
        .map(|deadline| TimeSpec::now() >= deadline)
        .unwrap_or(false)
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

    let task = current_task().unwrap();
    if task.process.live_thread_count() != 1 {
        return None;
    }
    if task_manager_counts()
        .map(|(ready, _)| ready != 0)
        .unwrap_or(true)
    {
        return None;
    }

    let deadline_ticks = timespec_to_ticks_ceil(deadline);
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

fn futex_wait_tail_spin(
    lock: &spin::Mutex<FutexTable>,
    waiter: &Arc<FutexWaiter>,
    task: &Arc<TaskControlBlock>,
    deadline: TimeSpec,
) -> WaitResult {
    let deadline_ticks = timespec_to_ticks_ceil(deadline);

    loop {
        let mut table = lock.lock();
        // wake 在同一把锁下先移除 waiter 再发布 woken，因此
        // 与 deadline 同时到达时，谁先取得 table 锁就是唯一胜者。
        if waiter.was_woken() {
            return WaitResult::Ready(SUCCESS);
        }
        if get_time() >= deadline_ticks {
            table.remove_wait(waiter);
            return WaitResult::TimedOut;
        }
        if has_actionable_signal(task) {
            table.remove_wait(waiter);
            return WaitResult::Interrupted;
        }
        discard_non_actionable_unblocked_signals(task);
        drop(table);
        spin_loop();
    }
}

fn futex_wait_event_interruptible_timeout_locked(
    lock: &spin::Mutex<FutexTable>,
    vm: &AddressSpace<PageTableImpl>,
    entry: &FutexWaitSpec,
    deadline: Option<TimeSpec>,
) -> Result<WaitResult, ()> {
    let task = current_task().unwrap();
    let waiter = Arc::new(FutexWaiter::new(Arc::downgrade(&task), entry.queue_key));
    let mut table = lock.lock();

    if deadline_expired(deadline) {
        return Ok(WaitResult::TimedOut);
    }
    match check_words_nofault(vm, core::slice::from_ref(entry)) {
        FutexWordCheck::Matches => {}
        FutexWordCheck::Mismatch => return Ok(WaitResult::Ready(EAGAIN)),
        FutexWordCheck::Retry => return Err(()),
    }
    if has_actionable_signal(&task) {
        return Ok(WaitResult::Interrupted);
    }

    // 最后一次值比较与 waiter 发布由同一把 table 锁串行化。
    // wake 要么先完成并让上面的比较看到新值，要么在解锁后看到 waiter。
    table.enqueue(
        entry.queue_key,
        waiter.clone(),
        entry.shared_backing.as_ref(),
    );
    discard_non_actionable_unblocked_signals(&task);
    drop(table);
    drop(task);

    loop {
        let task = current_task().unwrap();
        let mut table = lock.lock();

        if waiter.was_woken() {
            return Ok(WaitResult::Ready(SUCCESS));
        }
        if deadline_expired(deadline) {
            table.remove_wait(&waiter);
            return Ok(WaitResult::TimedOut);
        }
        if has_actionable_signal(&task) {
            table.remove_wait(&waiter);
            return Ok(WaitResult::Interrupted);
        }
        discard_non_actionable_unblocked_signals(&task);

        let block_deadline = futex_wait_block_deadline(deadline);
        if let Some(real_deadline) = deadline {
            if block_deadline == Some(real_deadline) {
                drop(table);
                return Ok(futex_wait_tail_spin(lock, &waiter, &task, real_deadline));
            }
        }
        if let Some(block_deadline) = block_deadline {
            wait_with_timeout(Arc::downgrade(&task), block_deadline);
        }
        drop(task);

        block_current_and_run_next_with_lock_checked(table, |task| {
            let no_signal = !has_actionable_signal(task);
            let not_timed_out = block_deadline
                .map(|deadline| TimeSpec::now() < deadline)
                .unwrap_or(true);
            no_signal && not_timed_out
        });

        let task = current_task().unwrap();
        task.acquire_inner_lock().refresh_real_timer();
    }
}

/// 把 FUTEX_WAIT 的相对 timeout 固定为本次 syscall 的绝对 deadline。
/// retry 必须复用该值，不能重新起算并意外延长等待。
pub fn futex_wait_deadline(timeout: Option<TimeSpec>) -> Option<TimeSpec> {
    timeout.map(futex_relative_deadline)
}

/// 尝试在进程私有表中注册一次 futex wait。
///
/// `deadline` 已是绝对时间；调用者在 `Retry` 后须重新 fault-in 并解析 key。
pub fn do_futex_wait(
    futex_word: UserPtr<u32>,
    token: usize,
    futex_key: usize,
    val: u32,
    deadline: Option<TimeSpec>,
) -> FutexOpOutcome {
    super::perf::record_futex_wait(false, deadline.is_some());

    if let Some(wait_result) = try_single_thread_short_timeout(futex_word, token, val, deadline) {
        super::perf::record_futex_wait_result(wait_result);
        return FutexOpOutcome::Complete(futex_wait_result_to_errno(wait_result));
    }

    let task = current_task().unwrap();
    let futex_table = task.process.futex().clone();
    let vm = task.process.vm();
    drop(task);
    let entry = FutexWaitSpec::private(futex_word, futex_key, val);
    let wait_result =
        match futex_wait_event_interruptible_timeout_locked(&futex_table, &vm, &entry, deadline) {
            Ok(result) => result,
            Err(()) => return FutexOpOutcome::Retry,
        };
    super::perf::record_futex_wait_result(wait_result);

    FutexOpOutcome::Complete(futex_wait_result_to_errno(wait_result))
}

pub fn do_futex_waitv(entries: &[FutexWaitSpec], deadline: Option<TimeSpec>) -> FutexOpOutcome {
    let task = current_task().unwrap();
    let futex_table = task.process.futex().clone();
    let vm = task.process.vm();
    drop(task);
    futex_waitv_locked(&futex_table, &vm, entries, deadline)
}

fn last_woken_index(waiters: &[Arc<FutexWaiter>]) -> Option<usize> {
    // Linux futex_unqueue_multiple() 同样按数组顺序清理并保留最后一个
    // 已被 wake 的下标；多个 key 并发命中时不能擅自改成第一个。
    waiters.iter().rposition(|waiter| waiter.was_woken())
}

fn remove_waitv(table: &mut FutexTable, waiters: &[Arc<FutexWaiter>]) {
    for waiter in waiters {
        table.remove_wait(waiter);
    }
}

/// 在同一张 futex table 中原子注册多个等待项。
///
/// 每个用户 waitv 条目都有独立 waiter；即使两个条目使用同一 key，
/// wake 也只消费真正被移除的那个注册项，返回值仍是原始数组下标。
fn futex_waitv_locked(
    lock: &spin::Mutex<FutexTable>,
    vm: &AddressSpace<PageTableImpl>,
    entries: &[FutexWaitSpec],
    deadline: Option<TimeSpec>,
) -> FutexOpOutcome {
    let task = current_task().unwrap();
    let task_weak = Arc::downgrade(&task);
    let mut waiters = Vec::new();
    if waiters.try_reserve(entries.len()).is_err() {
        return FutexOpOutcome::Complete(ENOMEM);
    }
    for entry in entries {
        waiters.push(Arc::new(FutexWaiter::new(
            task_weak.clone(),
            entry.queue_key,
        )));
    }

    let mut table = lock.lock();
    if deadline_expired(deadline) {
        return FutexOpOutcome::Complete(ETIMEDOUT);
    }
    match check_words_nofault(vm, entries) {
        FutexWordCheck::Matches => {}
        FutexWordCheck::Mismatch => return FutexOpOutcome::Complete(EAGAIN),
        FutexWordCheck::Retry => return FutexOpOutcome::Retry,
    }
    if has_actionable_signal(&task) {
        return FutexOpOutcome::Complete(EINTR);
    }
    for (entry, waiter) in entries.iter().zip(waiters.iter()) {
        table.enqueue(
            entry.queue_key,
            waiter.clone(),
            entry.shared_backing.as_ref(),
        );
    }
    // 值比较与全部 waiter 发布处在同一 table 临界区；此后 requeue
    // 可能改变 key，恢复路径只读取各 registration 的权威状态。
    discard_non_actionable_unblocked_signals(&task);
    drop(table);
    drop(task);

    loop {
        let task = current_task().unwrap();
        let mut table = lock.lock();
        if let Some(index) = last_woken_index(&waiters) {
            remove_waitv(&mut table, &waiters);
            return FutexOpOutcome::Complete(index as isize);
        }
        if deadline_expired(deadline) {
            remove_waitv(&mut table, &waiters);
            return FutexOpOutcome::Complete(ETIMEDOUT);
        }
        if has_actionable_signal(&task) {
            remove_waitv(&mut table, &waiters);
            return FutexOpOutcome::Complete(EINTR);
        }
        discard_non_actionable_unblocked_signals(&task);

        if let Some(deadline) = deadline {
            wait_with_timeout(Arc::downgrade(&task), deadline);
        }
        drop(task);

        block_current_and_run_next_with_lock_checked(table, |task| {
            !has_actionable_signal(task) && !deadline_expired(deadline)
        });

        let task = current_task().unwrap();
        task.acquire_inner_lock().refresh_real_timer();
    }
}

/// 唤醒等待在全局 process-shared futex 上的最多 `val` 个任务。
pub fn futex_wake_shared(key: SharedFutexKey, val: u32) -> isize {
    let mut shared = PROCESS_SHARED_FUTEX.lock();
    let woke = shared.wake_waiters(key.queue_key(), val);
    refresh_shared_futex_nonempty(&shared);
    super::perf::record_futex_wake(true, woke);
    woke
}

/// 在调用方持有的 table 临界区内复核映射，并执行一次 requeue。
///
/// `expected` 为 `Some` 时，source 值比较也位于这个临界区；`Retry`
/// 只表示尚未修改任何队列，调用方必须在锁外重新 fault-in 和解析 key。
fn requeue_checked(
    table: &mut FutexTable,
    vm: &AddressSpace<PageTableImpl>,
    source_word: UserPtr<u32>,
    source_key: QueueKey,
    source_backing: Option<&Arc<FrameTracker>>,
    target_word: UserPtr<u32>,
    target_key: QueueKey,
    target_backing: Option<&Arc<FrameTracker>>,
    expected: Option<u32>,
    wake: u32,
    move_count: usize,
) -> FutexOpOutcome {
    match check_requeue_nofault(
        vm,
        source_word,
        source_backing,
        target_word,
        target_backing,
        expected,
    ) {
        FutexWordCheck::Matches => FutexOpOutcome::Complete(table.requeue_waiters(
            source_key,
            target_key,
            target_backing,
            wake,
            move_count,
        )),
        FutexWordCheck::Mismatch => FutexOpOutcome::Complete(EAGAIN),
        FutexWordCheck::Retry => FutexOpOutcome::Retry,
    }
}

/// 对进程私有 futex 执行带锁内 nofault 复查的 requeue。
pub fn futex_requeue_private(
    source_word: UserPtr<u32>,
    source_key: usize,
    target_word: UserPtr<u32>,
    target_key: usize,
    expected: Option<u32>,
    wake: u32,
    move_count: usize,
) -> FutexOpOutcome {
    let task = current_task().unwrap();
    let table = task.process.futex();
    let vm = task.process.vm();
    drop(task);
    let mut table = table.lock();
    requeue_checked(
        &mut table,
        &vm,
        source_word,
        QueueKey::private(source_key),
        None,
        target_word,
        QueueKey::private(target_key),
        None,
        expected,
        wake,
        move_count,
    )
}

/// 对 process-shared futex 执行带 backing 复查的 requeue。
pub fn futex_requeue_shared(
    source_word: UserPtr<u32>,
    source: SharedFutexKey,
    target_word: UserPtr<u32>,
    target: SharedFutexKey,
    expected: Option<u32>,
    wake: u32,
    move_count: usize,
) -> FutexOpOutcome {
    let task = current_task().unwrap();
    let vm = task.process.vm();
    drop(task);
    let mut table = PROCESS_SHARED_FUTEX.lock();
    let result = requeue_checked(
        &mut table,
        &vm,
        source_word,
        source.queue_key(),
        Some(&source.backing),
        target_word,
        target.queue_key(),
        Some(&target.backing),
        expected,
        wake,
        move_count,
    );
    refresh_shared_futex_nonempty(&table);
    result
}

pub fn do_futex_waitv_shared(
    entries: &[FutexWaitSpec],
    deadline: Option<TimeSpec>,
) -> FutexOpOutcome {
    let task = current_task().unwrap();
    let vm = task.process.vm();
    drop(task);
    PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY.store(true, Ordering::Relaxed);
    let result = futex_waitv_locked(&PROCESS_SHARED_FUTEX, &vm, entries, deadline);
    let shared = PROCESS_SHARED_FUTEX.lock();
    refresh_shared_futex_nonempty(&shared);
    result
}

/// Process-shared futex wait — 使用全局 backing 身份表。
pub fn do_futex_wait_shared(
    futex_word: UserPtr<u32>,
    token: usize,
    val: u32,
    deadline: Option<TimeSpec>,
    key: SharedFutexKey,
) -> FutexOpOutcome {
    super::perf::record_futex_wait(true, deadline.is_some());
    if let Some(wait_result) = try_single_thread_short_timeout(futex_word, token, val, deadline) {
        super::perf::record_futex_wait_result(wait_result);
        return FutexOpOutcome::Complete(futex_wait_result_to_errno(wait_result));
    }

    let task = current_task().unwrap();
    let vm = task.process.vm();
    drop(task);
    let entry = FutexWaitSpec::shared(futex_word, key, val);
    PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY.store(true, Ordering::Relaxed);
    let wait_result = match futex_wait_event_interruptible_timeout_locked(
        &PROCESS_SHARED_FUTEX,
        &vm,
        &entry,
        deadline,
    ) {
        Ok(result) => result,
        Err(()) => {
            let shared = PROCESS_SHARED_FUTEX.lock();
            refresh_shared_futex_nonempty(&shared);
            return FutexOpOutcome::Retry;
        }
    };
    let shared = PROCESS_SHARED_FUTEX.lock();
    refresh_shared_futex_nonempty(&shared);
    drop(shared);
    super::perf::record_futex_wait_result(wait_result);

    FutexOpOutcome::Complete(futex_wait_result_to_errno(wait_result))
}

impl FutexTable {
    /// 创建空 futex 等待表。
    pub fn new() -> Self {
        Self {
            queues: BTreeMap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    fn queue(&mut self, key: QueueKey, backing_pin: Option<&Arc<FrameTracker>>) -> &mut FutexQueue {
        let queue = self
            .queues
            .entry(key)
            .or_insert_with(|| FutexQueue::new(backing_pin.cloned()));
        queue.assert_same_backing(backing_pin);
        queue
    }

    fn enqueue(
        &mut self,
        key: QueueKey,
        waiter: Arc<FutexWaiter>,
        backing_pin: Option<&Arc<FrameTracker>>,
    ) {
        debug_assert_eq!(waiter.queue_key_locked(), key);
        self.queue(key, backing_pin).enqueue(waiter);
    }

    /// 按等待身份的当前 key 精确撤销，requeue 后也不会回到原队列。
    fn remove_wait(&mut self, waiter: &Arc<FutexWaiter>) -> bool {
        let key = waiter.queue_key_locked();
        let removed = self
            .queues
            .get_mut(&key)
            .map(|queue| queue.remove(waiter))
            .unwrap_or(false);
        self.remove_empty(key);
        removed
    }

    fn remove_empty(&mut self, key: QueueKey) {
        if self
            .queues
            .get(&key)
            .map(FutexQueue::is_empty)
            .unwrap_or(false)
        {
            self.queues.remove(&key);
        }
    }

    fn compact_stale(&mut self) {
        self.queues.retain(|_, queue| {
            queue.compact_stale();
            !queue.is_empty()
        });
    }

    fn wake_waiters(&mut self, key: QueueKey, val: u32) -> isize {
        let Some(mut queue) = self.queues.remove(&key) else {
            return 0;
        };
        let wake_count = queue.wake_at_most(val as usize);
        if !queue.is_empty() {
            self.queues.insert(key, queue);
        }
        wake_count as isize
    }

    fn requeue_waiters(
        &mut self,
        source: QueueKey,
        target: QueueKey,
        target_backing: Option<&Arc<FrameTracker>>,
        wake: u32,
        move_count: usize,
    ) -> isize {
        let wake_count = if wake == 0 {
            0
        } else {
            self.wake_waiters(source, wake)
        };
        if source == target {
            return wake_count;
        }

        let Some(mut source_queue) = self.queues.remove(&source) else {
            return wake_count;
        };
        let mut target_queue = self
            .queues
            .remove(&target)
            .unwrap_or_else(|| FutexQueue::new(target_backing.cloned()));
        target_queue.assert_same_backing(target_backing);
        let moved = source_queue.requeue_to(&mut target_queue, target, move_count);
        if !source_queue.is_empty() {
            self.queues.insert(source, source_queue);
        }
        if !target_queue.is_empty() {
            self.queues.insert(target, target_queue);
        }
        wake_count + moved as isize
    }

    /// 唤醒指定 key 上的最多 `val` 个等待项。
    pub fn wake(&mut self, futex_key: usize, val: u32) -> isize {
        let woke = self.wake_waiters(QueueKey::private(futex_key), val);
        super::perf::record_futex_wake(false, woke);
        woke
    }
}
