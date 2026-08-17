use alloc::collections::VecDeque;
use alloc::sync::Arc;

use super::{EntryId, WaitEntryCore, WaitToken, WakeAttempt};

fn never_stale<P>(_: &P) -> bool {
    false
}

/// 纯 FIFO 队列状态机。
///
/// 每个队列强持有 entry；entry 可由多个队列共享，故 token 位于 entry 内而非
/// 队列内。`stale` 只在 batch wake 已达到 limit 后决定保留规则。
pub struct WaitQueueCore<P> {
    inner: VecDeque<Arc<WaitEntryCore<P>>>,
    stale: fn(&P) -> bool,
}

impl<P> Clone for WaitQueueCore<P> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            stale: self.stale,
        }
    }
}

impl<P> WaitQueueCore<P> {
    /// 创建不主动回收 payload 的队列。
    pub fn new() -> Self {
        Self::new_with_stale(never_stale::<P>)
    }

    /// 创建带有零分配 stale 判断的队列。
    pub fn new_with_stale(stale: fn(&P) -> bool) -> Self {
        Self {
            inner: VecDeque::new(),
            stale,
        }
    }

    /// 登记新 entry 并返回其不复用身份。
    pub fn prepare_to_wait(&mut self, payload: P) -> EntryId {
        let entry = self.prepare_entry(payload);
        entry.id()
    }

    /// 登记并返回可供多队列共享的 entry handle。
    pub fn prepare_entry(&mut self, payload: P) -> Arc<WaitEntryCore<P>> {
        let entry = WaitEntryCore::new_waiting(payload);
        self.enqueue_existing(entry.clone());
        entry
    }

    /// 将已有共享 entry 追加到此队列。
    pub fn enqueue_existing(&mut self, entry: Arc<WaitEntryCore<P>>) {
        self.inner.push_back(entry);
    }

    /// 弹出队首 entry，不改变其 token。
    pub fn pop_entry(&mut self) -> Option<Arc<WaitEntryCore<P>>> {
        self.inner.pop_front()
    }

    /// 返回指定 entry 的 payload。
    pub fn get_payload(&self, id: EntryId) -> Option<&P> {
        self.inner
            .iter()
            .find(|entry| entry.id() == id)
            .map(|entry| entry.payload())
    }

    /// 在 entry 未被其它队列或等待者共享时返回可变 payload。
    pub fn get_payload_mut(&mut self, id: EntryId) -> Option<&mut P> {
        self.inner
            .iter_mut()
            .find(|entry| entry.id() == id)
            .and_then(Arc::get_mut)
            .map(WaitEntryCore::payload_mut)
    }

    /// 借用指定 entry 的 token；借用避免热路径额外分配或复制原子状态。
    pub fn token_of(&self, id: EntryId) -> Option<&WaitToken> {
        self.inner
            .iter()
            .find(|entry| entry.id() == id)
            .map(|entry| entry.token())
    }

    /// 判断 entry 是否仍在此队列中。
    pub fn contains_entry(&self, id: EntryId) -> bool {
        self.inner.iter().any(|entry| entry.id() == id)
    }

    /// 枚举 entry，供 façade 保留既有 `contains` 语义。
    pub fn entries(&self) -> impl Iterator<Item = &Arc<WaitEntryCore<P>>> {
        self.inner.iter()
    }

    /// FIFO 批量 wake：成功领取的条目移除，达到 limit 后仅保留非 stale 条目。
    pub fn wake_at_most<F>(&mut self, limit: usize, mut claim_closure: F) -> usize
    where
        F: FnMut(&P, EntryId, WakeAttempt<'_>) -> bool,
    {
        if limit == 0 {
            return 0;
        }
        let mut remaining = VecDeque::new();
        let mut wake_count = 0;
        while let Some(entry) = self.inner.pop_front() {
            if wake_count >= limit {
                if !(self.stale)(entry.payload()) {
                    remaining.push_back(entry);
                }
                continue;
            }
            let attempt = WakeAttempt { token: entry.token() };
            if claim_closure(entry.payload(), entry.id(), attempt) {
                wake_count += 1;
            }
        }
        self.inner = remaining;
        wake_count
    }

    /// hot-path 单次 wake：只移除队首到首个成功领取者之间的条目。
    pub fn wake_one<F>(&mut self, mut wake_closure: F) -> usize
    where
        F: FnMut(&P, EntryId, WakeAttempt<'_>) -> bool,
    {
        while let Some(entry) = self.inner.pop_front() {
            let attempt = WakeAttempt { token: entry.token() };
            if wake_closure(entry.payload(), entry.id(), attempt) {
                return 1;
            }
        }
        0
    }

    /// 关闭并精确删除本队列中的一个 entry。
    pub fn finish_entry(&mut self, id: EntryId) -> bool {
        let Some(index) = self.inner.iter().position(|entry| entry.id() == id) else {
            return false;
        };
        if let Some(entry) = self.inner.remove(index) {
            entry.close();
            true
        } else {
            false
        }
    }

    /// 仅按 identity 删除，用于 façade 已先关闭共享 token 的多队列清理。
    pub fn remove_entry(&mut self, id: EntryId) -> bool {
        let Some(index) = self.inner.iter().position(|entry| entry.id() == id) else {
            return false;
        };
        self.inner.remove(index).is_some()
    }

    /// 关闭并删除全部匹配 payload 的 entry。
    pub fn finish_wait<F>(&mut self, mut pred: F) -> bool
    where
        F: FnMut(&P) -> bool,
    {
        let old_len = self.inner.len();
        self.inner.retain(|entry| {
            if pred(entry.payload()) {
                entry.close();
                false
            } else {
                true
            }
        });
        self.inner.len() != old_len
    }

    /// 回收被 payload 生命周期判定为 stale 的 entry。
    pub fn remove_stale<F>(&mut self, mut pred: F) -> usize
    where
        F: FnMut(&P) -> bool,
    {
        let old_len = self.inner.len();
        self.inner.retain(|entry| !pred(entry.payload()));
        old_len - self.inner.len()
    }

    /// 返回 entry 数量。
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 返回队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}
