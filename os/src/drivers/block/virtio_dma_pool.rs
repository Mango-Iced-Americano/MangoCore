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
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

// ── 常量 ────────────────────────────────────────────────────────────────

/// 每槽位页数。
pub const DMA_POOL_SLOT_PAGES: usize = 16;
/// 槽位总数。
pub const DMA_POOL_SLOTS: usize = 4;
/// 每槽位字节数。
pub const DMA_POOL_BUF_BYTES: usize = DMA_POOL_SLOT_PAGES * PAGE_SIZE;
/// 小型 VirtIO 描述符槽位总数。
pub const DMA_SMALL_POOL_SLOTS: usize = 32;

// ── 公开类型 ────────────────────────────────────────────────────────────

/// 槽位预约令牌。
///
/// 获得后可选择 `consume`（转入 InUse）或 `cancel`（重回 Free）。
#[derive(Debug, Clone, Copy)]
pub struct DmaReservation {
    pub slot: usize,
    pub gen: usize,
}

/// DMA 池中一次 share 返回的槽位类型。
#[derive(Debug, Clone, Copy)]
pub enum DmaPoolSlot {
    Data(usize),
    Small(usize),
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

/// 一个固定大小的单页描述符槽位。
struct SmallDmaSlot {
    pa: usize,
    #[allow(unused)]
    frames: Vec<Arc<FrameTracker>>,
    in_use: bool,
}

/// DMA 池全局状态。
struct DmaPool {
    enabled: bool,
    init_attempted: bool,
    slots: Vec<DmaSlot>,
    small_slots: Vec<SmallDmaSlot>,
    /// Round-robin 搜索起点。
    next: usize,
    small_next: usize,
}

lazy_static::lazy_static! {
    static ref DMA_POOL: Mutex<DmaPool> = Mutex::new(DmaPool {
        enabled: false,
        init_attempted: false,
        slots: Vec::new(),
        small_slots: Vec::new(),
        next: 0,
        small_next: 0,
    });
}

/// Per-hart request context bridging a block reservation into the HAL
/// `share()` callback. The driver API does not carry an opaque request token,
/// but the callback is synchronous on the submitting hart. Local IRQ masking
/// therefore makes one fixed context per logical CPU sufficient without
/// serializing unrelated block or network devices across harts.
struct DmaBridgeContext {
    claimed: AtomicBool,
    /// Zero means no pending reservation; otherwise stores `slot + 1`.
    pending_slot: AtomicUsize,
    pending_gen: AtomicUsize,
}

impl DmaBridgeContext {
    const fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            pending_slot: AtomicUsize::new(0),
            pending_gen: AtomicUsize::new(0),
        }
    }

    fn take_pending(&self) -> Option<DmaReservation> {
        let encoded_slot = self.pending_slot.swap(0, Ordering::AcqRel);
        (encoded_slot != 0).then(|| DmaReservation {
            slot: encoded_slot - 1,
            gen: self.pending_gen.load(Ordering::Acquire),
        })
    }
}

static DMA_BRIDGE_CONTEXTS: [DmaBridgeContext; crate::smp::MAX_CPUS] =
    [const { DmaBridgeContext::new() }; crate::smp::MAX_CPUS];

pub(crate) struct DmaBridgeGuard {
    cpu_id: usize,
    irq_was_enabled: bool,
    lock_acquired_ticks: usize,
}

pub(crate) fn dma_bridge_lock() -> DmaBridgeGuard {
    // VirtIO completion paths may be interrupted by the scheduler's network
    // poll on the same hart. Disable local interrupts before claiming this
    // hart's context so an interrupt cannot nest a second synchronous
    // `share()` callback over the interrupted block request.
    let irq_was_enabled = crate::hal::local_irq_save();
    let cpu_id = crate::smp::cpu_id();
    let context = &DMA_BRIDGE_CONTEXTS[cpu_id];
    assert!(
        context
            .claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok(),
        "nested VirtIO DMA bridge on CPU {}",
        cpu_id
    );
    assert_eq!(
        context.pending_slot.load(Ordering::Acquire),
        0,
        "stale VirtIO DMA reservation on CPU {}",
        cpu_id
    );
    let lock_acquired_ticks =
        crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    DmaBridgeGuard {
        cpu_id,
        irq_was_enabled,
        lock_acquired_ticks,
    }
}

/// Publish one request reservation to the current hart's synchronous HAL
/// callback. The caller must hold [`DmaBridgeGuard`].
pub(crate) fn dma_bridge_set_reservation(reservation: Option<DmaReservation>) {
    let cpu_id = crate::smp::cpu_id();
    let context = &DMA_BRIDGE_CONTEXTS[cpu_id];
    assert!(
        context.claimed.load(Ordering::Acquire),
        "VirtIO DMA reservation published outside bridge guard"
    );
    assert_eq!(
        context.pending_slot.load(Ordering::Acquire),
        0,
        "VirtIO DMA reservation overwritten on CPU {}",
        cpu_id
    );
    if let Some(reservation) = reservation {
        context
            .pending_gen
            .store(reservation.gen, Ordering::Relaxed);
        context
            .pending_slot
            .store(reservation.slot + 1, Ordering::Release);
    }
}

/// Consume the current hart's reservation for the data-buffer `share()` call.
pub(crate) fn dma_bridge_take_data_reservation() -> Option<DmaReservation> {
    DMA_BRIDGE_CONTEXTS[crate::smp::cpu_id()].take_pending()
}

/// Cancel a reservation that the driver did not consume for this request.
pub(crate) fn dma_bridge_cancel_pending() {
    if let Some(reservation) = DMA_BRIDGE_CONTEXTS[crate::smp::cpu_id()].take_pending() {
        dma_pool_cancel_reservation(reservation.slot, reservation.gen);
    }
}

impl Drop for DmaBridgeGuard {
    fn drop(&mut self) {
        let released_ticks =
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
        crate::task::perf::record_virtio_dma_bridge_lock(
            0,
            released_ticks.wrapping_sub(self.lock_acquired_ticks),
        );
        // Fail-safe cleanup covers checked-arithmetic and driver error exits
        // between reservation publication and the normal per-request cancel.
        if let Some(reservation) = DMA_BRIDGE_CONTEXTS[self.cpu_id].take_pending() {
            dma_pool_cancel_reservation(reservation.slot, reservation.gen);
        }
        DMA_BRIDGE_CONTEXTS[self.cpu_id]
            .claimed
            .store(false, Ordering::Release);
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

    // The descriptor pool is independent from the large data pool. If it
    // cannot be allocated, keep the data pool enabled and use the existing
    // correctness-preserving fallback for small share() buffers.
    let mut small_slots = Vec::new();
    if small_slots.try_reserve(DMA_SMALL_POOL_SLOTS).is_ok() {
        for _ in 0..DMA_SMALL_POOL_SLOTS {
            let Some(frames) = frames_alloc_fresh_contiguous(1) else {
                small_slots.clear();
                break;
            };
            let pa = frames[0].ppn.0 * PAGE_SIZE;
            small_slots.push(SmallDmaSlot {
                pa,
                frames,
                in_use: false,
            });
        }
    }

    let mut pool = DMA_POOL.lock();
    pool.slots = slots;
    pool.small_slots = small_slots;
    pool.enabled = true;
    pool.next = 0;
    pool.small_next = 0;
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

/// Return whether the optional single-page descriptor pool is available.
pub fn dma_small_pool_is_enabled() -> bool {
    let pool = DMA_POOL.lock();
    pool.enabled && !pool.small_slots.is_empty()
}

/// Allocate one fixed single-page slot for a known block descriptor buffer.
pub fn dma_pool_try_alloc_small() -> Option<(DmaPoolSlot, usize)> {
    let mut pool = DMA_POOL.lock();
    if !pool.enabled || pool.small_slots.is_empty() {
        return None;
    }
    let n = pool.small_slots.len();
    let start = pool.small_next;
    for offset in 0..n {
        let idx = (start + offset) % n;
        let pa = {
            let slot = &mut pool.small_slots[idx];
            if slot.in_use {
                continue;
            }
            slot.in_use = true;
            slot.pa
        };
        {
            pool.small_next = (idx + 1) % n;
        }
        return Some((DmaPoolSlot::Small(idx), pa));
    }
    None
}

/// 查询物理地址是否属于某槽位。
pub fn dma_pool_lookup(pa: usize) -> Option<DmaPoolSlot> {
    let pool = DMA_POOL.lock();
    if !pool.enabled {
        return None;
    }
    for (i, slot) in pool.slots.iter().enumerate() {
        let end = slot.pa + DMA_POOL_BUF_BYTES;
        if pa >= slot.pa && pa < end {
            return Some(DmaPoolSlot::Data(i));
        }
    }
    for (i, slot) in pool.small_slots.iter().enumerate() {
        if pa == slot.pa {
            return Some(DmaPoolSlot::Small(i));
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
pub fn dma_pool_finish_unshare(slot: DmaPoolSlot) {
    let mut pool = DMA_POOL.lock();
    match slot {
        DmaPoolSlot::Data(index) => {
            if let Some(entry) = pool.slots.get_mut(index) {
                match entry.state {
                    SlotState::InUse { .. } => {
                        entry.state = SlotState::Free;
                        drop(pool);
                        crate::task::perf::record_virtio_dma_pool_finish();
                    }
                    _ => {
                        log::warn!(
                            "dma_pool_finish_unshare: data slot {} not InUse ({:?})",
                            index,
                            entry.state
                        );
                    }
                }
            }
        }
        DmaPoolSlot::Small(index) => {
            if let Some(entry) = pool.small_slots.get_mut(index) {
                if entry.in_use {
                    entry.in_use = false;
                    drop(pool);
                    crate::task::perf::record_virtio_dma_pool_finish();
                } else {
                    log::warn!("dma_pool_finish_unshare: small slot {} not in use", index);
                }
            }
        }
    }
}
