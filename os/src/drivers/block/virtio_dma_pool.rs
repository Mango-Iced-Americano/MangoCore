//! 物理连续的 DMA 缓冲区池。
//!
//! 内核启动早期从 fresh 物理页面池一次性分配若干固定大小的连续
//! 缓冲区，供 VirtIO 驱动共享/回收。每槽 16 页（64 KiB），总共
//! 4 槽，避免碎片化回收栈破坏物理连续性。
//!
//! # 状态机
//!
//! ```text
//! Free ──reserve──▶ Reserved ──consume──▶ InUse ──finish_unshare──▶ Free
//!                     │                            ▲
//!                     └────cancel──────────────────┘
//! ```
//!
//! # Safety
//!
//! 池内页面在整个内核生命周期内 **永不归还** 帧分配器。释放槽位仅
//! 将槽位标记为可复用，`frames_alloc_fresh_contiguous` 产出的
//! `Arc<FrameTracker>` 始终由池持有。

use crate::config::PAGE_SIZE;
use crate::mm::{frames_alloc_fresh_contiguous, FrameTracker};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

// ── 常量 ────────────────────────────────────────────────────────────────

/// 每槽位页数。
pub const DMA_POOL_SLOT_PAGES: usize = 16;
/// 槽位总数。
pub const DMA_POOL_SLOTS: usize = 4;
/// 每槽位字节数。
pub const DMA_POOL_BUF_BYTES: usize = DMA_POOL_SLOT_PAGES * PAGE_SIZE;

// ── 公开类型 ────────────────────────────────────────────────────────────

/// 槽位预约令牌。
///
/// 获得后可选择 `consume`（转入 InUse）或 `cancel`（重回 Free）。
#[derive(Debug, Clone, Copy)]
pub struct DmaReservation {
    pub slot: usize,
    pub gen: usize,
}

// ── 内部类型 ────────────────────────────────────────────────────────────

/// 槽位状态。
#[derive(Debug)]
enum SlotState {
    Free,
    Reserved { gen: usize, pages: usize },
    InUse { gen: usize },
}

/// 单个 DMA 槽位。
struct DmaSlot {
    /// 槽位起始物理地址。
    pa: usize,
    /// 池持帧，永不释放。
    #[allow(unused)]
    frames: Vec<Arc<FrameTracker>>,
    /// 当前状态。
    state: SlotState,
    /// 代际计数器，防 ABA。
    gen: usize,
}

/// DMA 池全局状态。
struct DmaPool {
    enabled: bool,
    init_attempted: bool,
    slots: Vec<DmaSlot>,
    /// Round-robin 搜索起点。
    next: usize,
}

lazy_static::lazy_static! {
    static ref DMA_POOL: Mutex<DmaPool> = Mutex::new(DmaPool {
        enabled: false,
        init_attempted: false,
        slots: Vec::new(),
        next: 0,
    });
}

/// Serialize the short bridge window between a block request's reservation
/// and the `virtio_drivers::Hal::share()` callbacks that consume it.
///
/// The reservation is carried through a legacy global because the virtio
/// driver API does not pass request context into `share()`.  Block and net
/// devices use the same HAL implementation, so protecting only each device's
/// own queue mutex is insufficient on SMP: another device can overwrite the
/// pending token between the request setup and the driver's share callbacks.
/// Callers hold this guard across one complete virtio request, including the
/// `read_blocks`/`write_blocks`/`send`/`receive` call.
static DMA_BRIDGE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct DmaBridgeGuard {
    guard: Option<spin::MutexGuard<'static, ()>>,
    irq_was_enabled: bool,
    lock_start_ticks: usize,
    lock_acquired_ticks: usize,
}

pub(crate) fn dma_bridge_lock() -> DmaBridgeGuard {
    // VirtIO completion paths may be interrupted by the scheduler's network
    // poll on the same hart.  Disable local interrupts before taking the
    // bridge lock so an interrupt cannot re-enter `share()` and spin forever
    // on a lock held by the interrupted block request.
    let irq_was_enabled = crate::hal::local_irq_save();
    let lock_start_ticks = crate::task::perf::perf_time_now_for(
        crate::task::perf::STATS_PROFILE_MEMORY_IO,
    );
    let guard = DMA_BRIDGE_LOCK.lock();
    let lock_acquired_ticks = crate::task::perf::perf_time_now_for(
        crate::task::perf::STATS_PROFILE_MEMORY_IO,
    );
    DmaBridgeGuard {
        guard: Some(guard),
        irq_was_enabled,
        lock_start_ticks,
        lock_acquired_ticks,
    }
}

impl Drop for DmaBridgeGuard {
    fn drop(&mut self) {
        let released_ticks = crate::task::perf::perf_time_now_for(
            crate::task::perf::STATS_PROFILE_MEMORY_IO,
        );
        crate::task::perf::record_virtio_dma_bridge_lock(
            self.lock_acquired_ticks.wrapping_sub(self.lock_start_ticks),
            released_ticks.wrapping_sub(self.lock_acquired_ticks),
        );
        // Release the lock before restoring interrupts; otherwise an
        // immediately pending interrupt could re-enter the bridge while the
        // previous guard is still live.
        drop(self.guard.take());
        crate::hal::local_irq_restore(self.irq_was_enabled);
    }
}

// ── 公开 API ────────────────────────────────────────────────────────────

/// 一次性初始化 DMA 池。
///
/// 仅在首次调用时尝试从 fresh 帧分配器分配各槽位。若任一槽位分配
/// 失败则禁用整个池。多次调用无副作用。
pub fn dma_pool_init_once() {
    let mut pool = DMA_POOL.lock();
    if pool.init_attempted {
        return;
    }
    pool.init_attempted = true;

    // 提前 drop 锁 — 页面分配内部会获取 FRAME_ALLOCATOR 锁，不应嵌套。
    drop(pool);

    let mut slots = Vec::new();
    if slots.try_reserve(DMA_POOL_SLOTS).is_err() {
        return;
    }

    for _ in 0..DMA_POOL_SLOTS {
        let frames = match frames_alloc_fresh_contiguous(DMA_POOL_SLOT_PAGES) {
            Some(f) => f,
            None => {
                // 已分配的槽位帧会随 Vec drop 时归还帧分配器。
                return;
            }
        };

        let pa = frames[0].ppn.0 * PAGE_SIZE;
        slots.push(DmaSlot {
            pa,
            frames,
            state: SlotState::Free,
            gen: 0,
        });
    }

    let mut pool = DMA_POOL.lock();
    pool.slots = slots;
    pool.enabled = true;
    pool.next = 0;
}

/// 预约一个能容纳 `pages` 页的槽位。
///
/// 池未启用或所有槽位已被占用时返回 `None`。`pages` 不能超过
/// `DMA_POOL_SLOT_PAGES`。
pub fn dma_pool_reserve(pages: usize) -> Option<DmaReservation> {
    assert!(pages <= DMA_POOL_SLOT_PAGES);
    let mut pool = DMA_POOL.lock();
    if !pool.enabled || pool.slots.is_empty() {
        drop(pool);
        crate::task::perf::record_virtio_dma_pool_reserve(false);
        return None;
    }

    let n = pool.slots.len();
    let start = pool.next;
    for offset in 0..n {
        let idx = (start + offset) % n;
        let slot = &mut pool.slots[idx];
        if let SlotState::Free = slot.state {
            let gen = slot.gen;
            slot.state = SlotState::Reserved { gen, pages };
            slot.gen = slot.gen.wrapping_add(1);
            pool.next = (idx + 1) % n;
            let reservation = Some(DmaReservation { slot: idx, gen });
            drop(pool);
            crate::task::perf::record_virtio_dma_pool_reserve(true);
            return reservation;
        }
    }
    drop(pool);
    crate::task::perf::record_virtio_dma_pool_reserve(false);
    None
}

/// 消费预约，返回槽位起始物理地址。
///
/// # Panics
///
/// 预约代际不匹配或槽位状态非 Reserved 时 panic。
pub fn dma_pool_consume_reserved(reservation: DmaReservation) -> usize {
    let mut pool = DMA_POOL.lock();
    let slot = &mut pool.slots[reservation.slot];
    match slot.state {
        SlotState::Reserved { gen, .. } if gen == reservation.gen => {
            let pa = slot.pa;
            slot.state = SlotState::InUse { gen };
            drop(pool);
            crate::task::perf::record_virtio_dma_pool_consume();
            pa
        }
        _ => panic!(
            "dma_pool_consume_reserved: stale reservation slot={} gen={}",
            reservation.slot, reservation.gen
        ),
    }
}

/// 取消预约，槽位重回 Free。
///
/// # Panics
///
/// 预约代际不匹配或槽位状态非 Reserved 时 panic。
pub fn dma_pool_cancel_reservation(slot: usize, gen: usize) {
    let mut pool = DMA_POOL.lock();
    let entry = &mut pool.slots[slot];
    match entry.state {
        SlotState::Reserved { gen: g, .. } if g == gen => {
            entry.state = SlotState::Free;
            drop(pool);
            crate::task::perf::record_virtio_dma_pool_cancel();
        }
        _ => panic!(
            "dma_pool_cancel_reservation: invalid slot={} gen={}",
            slot, gen
        ),
    }
}

/// 查询物理地址是否属于某槽位，返回槽位索引。
pub fn dma_pool_lookup(pa: usize) -> Option<usize> {
    let pool = DMA_POOL.lock();
    if !pool.enabled {
        return None;
    }
    for (i, slot) in pool.slots.iter().enumerate() {
        let end = slot.pa + DMA_POOL_BUF_BYTES;
        if pa >= slot.pa && pa < end {
            return Some(i);
        }
    }
    None
}

/// Return whether the fixed pool completed initialization successfully.
pub fn dma_pool_is_enabled() -> bool {
    DMA_POOL.lock().enabled
}

/// 完成 DMA 后释放槽位，标记为 Free。
///
/// 调用者必须已完成数据拷贝（device → driver 方向）。
/// 仅当槽位处于 InUse 状态时生效。
pub fn dma_pool_finish_unshare(slot: usize) {
    let mut pool = DMA_POOL.lock();
    if let Some(entry) = pool.slots.get_mut(slot) {
        match entry.state {
            SlotState::InUse { .. } => {
                entry.state = SlotState::Free;
                drop(pool);
                crate::task::perf::record_virtio_dma_pool_finish();
            }
            _ => {
                log::warn!(
                    "dma_pool_finish_unshare: slot {} not InUse ({:?})",
                    slot,
                    entry.state
                );
            }
        }
    }
}
