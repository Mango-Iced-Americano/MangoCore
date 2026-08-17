use std::convert::TryFrom;

/// 固定参数 LCG；所有 scale 选择均由 seed 可重放。
pub struct Lcg {
    state: u64,
}

impl Lcg {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    pub fn choose_index(&mut self, len: usize) -> Option<usize> {
        let Ok(len) = u64::try_from(len) else {
            return None;
        };
        if len == 0 {
            return None;
        }
        usize::try_from(self.next_u64() % len).ok()
    }
}
