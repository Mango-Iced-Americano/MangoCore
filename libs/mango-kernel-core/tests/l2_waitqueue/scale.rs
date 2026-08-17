use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mango_kernel_core::wait_queue_core::{EntryId, WaitEntryCore, WaitQueueCore};

use crate::case_is_selected;
use crate::generator::Lcg;
use crate::invariants::{check_token_transition, check_wake_record, WakeRecord};

const TASK_POOL: [usize; 4] = [0, 1, 2, 3];
const MAX_LIVE_ENTRIES: usize = 16;

#[derive(Clone)]
struct ScalePayload {
    task: usize,
    stale: Arc<AtomicBool>,
}

fn scale_payload_is_stale(payload: &ScalePayload) -> bool {
    payload.stale.load(Ordering::Relaxed)
}

struct EntryRecord {
    entry: Arc<WaitEntryCore<ScalePayload>>,
    stale: Arc<AtomicBool>,
    in_queue: bool,
    claims: usize,
    observed_token: Option<usize>,
}

enum ScaleOperation {
    Prepare,
    Wake,
    Finish,
    MarkStale,
    Compact,
}

struct ScaleState {
    queue: WaitQueueCore<ScalePayload>,
    entries: Vec<EntryRecord>,
    last_wake: Option<WakeRecord>,
    preserved_after_finish: Vec<EntryId>,
}

impl ScaleState {
    fn new() -> Self {
        Self {
            queue: WaitQueueCore::new_with_stale(scale_payload_is_stale),
            entries: Vec::new(),
            last_wake: None,
            preserved_after_finish: Vec::new(),
        }
    }

    fn queued_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.in_queue.then_some(index))
            .collect()
    }

    fn choose_operation(&self, generator: &mut Lcg) -> ScaleOperation {
        let queued = self.queued_indices();
        let mut enabled = Vec::new();
        if queued.len() < MAX_LIVE_ENTRIES {
            enabled.push(ScaleOperation::Prepare);
        }
        if !queued.is_empty() {
            enabled.push(ScaleOperation::Wake);
            enabled.push(ScaleOperation::Finish);
            enabled.push(ScaleOperation::MarkStale);
        }
        if queued.iter().any(|index| {
            self.entries[*index].stale.load(Ordering::Relaxed)
        }) {
            enabled.push(ScaleOperation::Compact);
        }
        let index = generator.choose_index(enabled.len()).unwrap_or(0);
        enabled.swap_remove(index)
    }

    fn step(&mut self, generator: &mut Lcg) -> Result<(), String> {
        match self.choose_operation(generator) {
            ScaleOperation::Prepare => self.prepare(generator),
            ScaleOperation::Wake => self.wake(),
            ScaleOperation::Finish => self.finish(generator),
            ScaleOperation::MarkStale => self.mark_stale(generator),
            ScaleOperation::Compact => self.compact(),
        };
        self.check()?;
        self.entries.retain(|entry| entry.in_queue);
        Ok(())
    }

    fn prepare(&mut self, generator: &mut Lcg) {
        let task_index = generator.choose_index(TASK_POOL.len()).unwrap_or(0);
        let stale = Arc::new(AtomicBool::new(false));
        let entry = self.queue.prepare_entry(ScalePayload {
            task: TASK_POOL[task_index],
            stale: stale.clone(),
        });
        self.entries.push(EntryRecord {
            entry,
            stale,
            in_queue: true,
            claims: 0,
            observed_token: None,
        });
    }

    fn wake(&mut self) {
        let stale_ids = self
            .entries
            .iter()
            .filter(|entry| entry.in_queue && entry.stale.load(Ordering::Relaxed))
            .map(|entry| entry.entry.id())
            .collect::<Vec<_>>();
        let mut claimed_ids = Vec::new();
        let returned = self.queue.wake_at_most(2, |payload, id, attempt| {
            if scale_payload_is_stale(payload) {
                attempt.close();
                false
            } else if attempt.try_claim() {
                claimed_ids.push(id);
                true
            } else {
                false
            }
        });
        for entry in &mut self.entries {
            if claimed_ids.contains(&entry.entry.id()) {
                entry.claims += 1;
                entry.in_queue = false;
            }
            if stale_ids.contains(&entry.entry.id()) {
                entry.in_queue = false;
            }
        }
        self.last_wake = Some(WakeRecord {
            returned,
            claimed: claimed_ids.len(),
        });
    }

    fn finish(&mut self, generator: &mut Lcg) {
        let queued = self.queued_indices();
        let selected = queued[generator.choose_index(queued.len()).unwrap_or(0)];
        let target = self.entries[selected].entry.id();
        let task = self.entries[selected].entry.payload().task;
        self.preserved_after_finish = self
            .entries
            .iter()
            .filter(|entry| {
                entry.in_queue && entry.entry.id() != target && entry.entry.payload().task == task
            })
            .map(|entry| entry.entry.id())
            .collect();
        if self.queue.finish_entry(target) {
            self.entries[selected].in_queue = false;
        }
    }

    fn mark_stale(&mut self, generator: &mut Lcg) {
        let queued = self.queued_indices();
        let selected = queued[generator.choose_index(queued.len()).unwrap_or(0)];
        self.entries[selected].stale.store(true, Ordering::Relaxed);
    }

    fn compact(&mut self) {
        let stale_ids = self
            .entries
            .iter()
            .filter(|entry| entry.in_queue && entry.stale.load(Ordering::Relaxed))
            .map(|entry| entry.entry.id())
            .collect::<Vec<_>>();
        let removed = self.queue.remove_stale(scale_payload_is_stale);
        assert_eq!(removed, stale_ids.len());
        for entry in &mut self.entries {
            if stale_ids.contains(&entry.entry.id()) {
                entry.in_queue = false;
            }
        }
    }

    fn check(&mut self) -> Result<(), String> {
        if let Some(record) = self.last_wake {
            check_wake_record(record, 2).map_err(|violation| violation.detail)?;
        }
        for entry in &mut self.entries {
            let current = entry.entry.token().token_state();
            check_token_transition(entry.observed_token, current)
                .map_err(|violation| violation.detail)?;
            entry.observed_token = Some(current);
            if entry.claims > 1 {
                return Err(format!("entry {} claimed twice", entry.entry.id().get()));
            }
            if entry.in_queue != self.queue.contains_entry(entry.entry.id()) {
                return Err(format!("queue membership drift for entry {}", entry.entry.id().get()));
            }
        }
        for id in self.preserved_after_finish.drain(..) {
            if !self.queue.contains_entry(id) {
                return Err(format!("finish removed sibling entry {}", id.get()));
            }
        }
        Ok(())
    }
}

pub fn run_selected_scale(case: &str, operations: usize, seeds: &[u64]) {
    if !case_is_selected(case) {
        return;
    }
    for seed in seeds {
        let mut state = ScaleState::new();
        let mut generator = Lcg::new(*seed);
        for step in 0..operations {
            if let Err(detail) = state.step(&mut generator) {
                panic!(
                    "L2 WaitQueue: FAIL\ncase: {}\nseed: {}\nstep: {}\ninvariant: {}",
                    case, seed, step, detail
                );
            }
        }
    }
    println!("L2 WaitQueue: PASS");
    println!("case: {case}");
    println!("operations: {operations}");
}
