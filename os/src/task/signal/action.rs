//! 进程级信号动作表。
//!
//! `Sighand` 存储 `rt_sigaction` 设置的处理函数、mask 和 flag。表项按信号
//! 编号减一索引，未显式设置的项等价于 Linux 的 `SIG_DFL`。
//!
//! # Locking
//!
//! 调用方通常通过 `ProcessControlBlock::sighand()` 取得外层 `Mutex`。
//! 本类型自身不包含同步原语。

use alloc::vec::Vec;

use super::{SigAction, SigHandler};

#[derive(Clone)]
/// 进程共享的信号动作表。
pub struct Sighand {
    actions: Vec<Option<SigAction>>,
}

impl Sighand {
    /// 创建空动作表。
    ///
    /// # Semantics
    ///
    /// 空表不预分配 64 个槽位；`None` 表示 `SIG_DFL`。
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
        }
    }

    /// 克隆现有动作表，供未共享 `CLONE_SIGHAND` 的 fork/clone 子进程使用。
    pub fn from_existing(other: &Self) -> Self {
        Self {
            actions: other.actions.clone(),
        }
    }

    /// 查询指定信号的显式动作。
    ///
    /// 返回 `None` 表示该信号使用默认动作，或 `signum` 不是有效的 1-based 信号编号。
    pub fn get(&self, signum: usize) -> Option<&SigAction> {
        self.actions
            .get(signum.checked_sub(1)?)
            .and_then(|action| action.as_ref())
    }

    /// 设置指定信号的动作。
    ///
    /// # Semantics
    ///
    /// `action = None` 恢复为 `SIG_DFL`。表按需扩容，避免每个进程为未使用信号
    /// 常驻完整动作数组。
    pub fn set(&mut self, signum: usize, action: Option<SigAction>) {
        let idx = signum.saturating_sub(1);
        if idx >= self.actions.len() {
            self.actions.resize_with(idx + 1, || None);
        }
        self.actions[idx] = action;
    }

    /// 按 `execve` 语义重置信号动作。
    ///
    /// 用户 handler 不跨 exec 保留，但显式 `SIG_IGN` 必须继续生效。
    pub fn reset_for_exec(&mut self) {
        for action in self.actions.iter_mut() {
            if action
                .as_ref()
                .is_some_and(|action| action.handler != SigHandler::SIG_IGN)
            {
                *action = None;
            }
        }
    }
}

impl Default for Sighand {
    fn default() -> Self {
        Self::new()
    }
}
