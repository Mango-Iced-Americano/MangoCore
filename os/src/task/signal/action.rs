use alloc::boxed::Box;
use alloc::vec::Vec;

use super::SigAction;

#[derive(Clone)]
pub struct Sighand {
    actions: Vec<Option<Box<SigAction>>>,
}

impl Sighand {
    pub fn new() -> Self {
        let mut actions = Vec::with_capacity(64);
        actions.resize(64, None);
        Self { actions }
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
        if let Some(slot) = self.actions.get_mut(signum - 1) {
            *slot = action.map(Box::new);
        }
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
