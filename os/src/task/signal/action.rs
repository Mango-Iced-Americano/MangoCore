use alloc::boxed::Box;
use alloc::vec::Vec;

use super::SigAction;

#[derive(Clone)]
pub struct Sighand {
    actions: Vec<Option<Box<SigAction>>>,
}

impl Sighand {
    pub fn new() -> Self {
        // 从空 Vec 开始，按需在 set() 中扩展，避免每个进程浪费 1024 字节。
        Self {
            actions: Vec::new(),
        }
    }

    pub fn from_existing(other: &Self) -> Self {
        Self {
            actions: other.actions.clone(),
        }
    }

    pub fn get(&self, signum: usize) -> Option<&SigAction> {
        self.actions
            .get(signum.checked_sub(1)?)
            .and_then(|action| action.as_deref())
    }

    pub fn set(&mut self, signum: usize, action: Option<SigAction>) {
        let idx = signum.saturating_sub(1);
        if idx >= self.actions.len() {
            // Resize on demand: 扩展到足够容纳该信号编号
            self.actions.resize_with(idx + 1, || None);
        }
        self.actions[idx] = action.map(Box::new);
    }

    pub fn reset(&mut self) {
        for action in self.actions.iter_mut() {
            *action = None;
        }
    }
}

impl Default for Sighand {
    fn default() -> Self {
        Self::new()
    }
}
