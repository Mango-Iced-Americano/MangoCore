//! 可在内核与 host 测试之间共享的等待队列核心。

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

/// 一轮等待的原子通知 token。
pub struct WaitToken {
    state: AtomicUsize,
}

impl WaitToken {
    /// 尚未收到通知或清理的初始状态。
    pub const WAITING: usize = 0;
    /// 一个唤醒源已唯一领取通知权。
    pub const NOTIFIED: usize = 1;
    /// 等待者已结束本轮等待。
    pub const CLOSED: usize = 2;

    /// 创建处于 Waiting 的 token。
    pub const fn new_waiting() -> Self {
        Self {
            state: AtomicUsize::new(Self::WAITING),
        }
    }

    /// 唯一领取通知权。
    ///
    /// AcqRel 发布通知；失败路径的 Acquire 与成功领取方同步，避免多队列
    /// 等待者看到旧 token 状态。
    pub fn notify(&self) -> bool {
        self.state
            .compare_exchange(
                Self::WAITING,
                Self::NOTIFIED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// 关闭仍未通知的等待。
    pub fn close(&self) {
        let _ = self.state.compare_exchange(
            Self::WAITING,
            Self::CLOSED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// 返回本轮等待是否仍可提交 Blocking。
    pub fn is_waiting(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::WAITING
    }

    /// 返回原子状态，供模型检查器记录单调迁移。
    pub fn token_state(&self) -> usize {
        self.state.load(Ordering::Acquire)
    }

    /// 领取通知；guard 未提交时会把已领取 token 关闭。
    pub fn try_claim_if_waiting(&self) -> Option<WaitClaim<'_>> {
        if self.notify() {
            Some(WaitClaim {
                token: self,
                committed: false,
            })
        } else {
            None
        }
    }

    fn close_claimed(&self) {
        let _ = self.state.compare_exchange(
            Self::NOTIFIED,
            Self::CLOSED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// `try_claim_if_waiting` 的事务 guard。
pub struct WaitClaim<'a> {
    token: &'a WaitToken,
    committed: bool,
}

impl WaitClaim<'_> {
    /// 保留已领取的通知。
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for WaitClaim<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.token.close_claimed();
        }
    }
}

/// 永不复用的等待条目身份。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryId(usize);

impl EntryId {
    /// 返回仅用于诊断和 deterministic replay 的数值身份。
    pub const fn get(self) -> usize {
        self.0
    }
}

/// 所有队列共用单调编号，跨队列共享条目也不会误删同号条目。
static NEXT_ENTRY_ID: AtomicUsize = AtomicUsize::new(1);

fn allocate_entry_id() -> EntryId {
    loop {
        let current = NEXT_ENTRY_ID.load(Ordering::Relaxed);
        assert!(current != usize::MAX, "WaitQueue EntryId exhausted");
        match NEXT_ENTRY_ID.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return EntryId(current),
            Err(_) => continue,
        }
    }
}

/// 队列条目：payload 与本轮唯一通知 token 具有相同生命周期。
pub struct WaitEntryCore<P> {
    id: EntryId,
    payload: P,
    token: WaitToken,
}

impl<P> WaitEntryCore<P> {
    fn new(payload: P) -> Self {
        Self {
            id: allocate_entry_id(),
            payload,
            token: WaitToken::new_waiting(),
        }
    }

    /// 创建可被多个队列共享的条目。
    pub fn new_waiting(payload: P) -> Arc<Self> {
        Arc::new(Self::new(payload))
    }

    /// 返回本条目的不复用身份。
    pub const fn id(&self) -> EntryId {
        self.id
    }

    /// 借用 payload。
    pub fn payload(&self) -> &P {
        &self.payload
    }

    /// 在条目未共享时修改 payload。
    pub fn payload_mut(&mut self) -> &mut P {
        &mut self.payload
    }

    /// 借用本轮通知 token。
    pub fn token(&self) -> &WaitToken {
        &self.token
    }

    /// 返回本轮等待是否尚未收到通知。
    pub fn is_waiting(&self) -> bool {
        self.token.is_waiting()
    }

    /// 结束本轮仍未通知的等待。
    pub fn close(&self) {
        self.token.close();
    }
}

/// 一次 wake 回调可使用的唯一 token 操作入口。
#[derive(Clone, Copy)]
pub struct WakeAttempt<'a> {
    token: &'a WaitToken,
}

impl WakeAttempt<'_> {
    /// 在调用者完成 New/Zombie 等前置过滤后领取通知权。
    pub fn try_claim(self) -> bool {
        self.token.notify()
    }

    /// 将不可唤醒的条目关闭，防止其它共享队列再次领取。
    pub fn close(self) {
        self.token.close();
    }
}

mod queue;

pub use queue::WaitQueueCore;
