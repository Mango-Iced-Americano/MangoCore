use std::marker::PhantomData;
use std::sync::Arc;

use mango_kernel_core::wait_queue_core::{EntryId, WaitEntryCore, WaitQueueCore};

use crate::explorer::{SchedulableModel, StepRecord};
use crate::invariants::{check_protocol, InvariantViolation, WakeRecord};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorId {
    Waiter,
    Producer,
}

impl ActorId {
    pub const fn as_usize(self) -> usize {
        match self {
            Self::Waiter => 0,
            Self::Producer => 1,
        }
    }

    pub fn from_usize(value: usize) -> Option<Self> {
        match value {
            0 => Some(Self::Waiter),
            1 => Some(Self::Producer),
            _ => None,
        }
    }
}

pub trait WaitProtocol: Clone {
    const CASE: &'static str;
    const RECHECKS_AFTER_PREPARE: bool;
}

#[derive(Clone)]
pub struct CorrectWaitProtocol;

impl WaitProtocol for CorrectWaitProtocol {
    const CASE: &'static str = "correct";
    const RECHECKS_AFTER_PREPARE: bool = true;
}

#[derive(Clone)]
pub struct B71LegacyProtocol;

impl WaitProtocol for B71LegacyProtocol {
    const CASE: &'static str = "b71_bad";
    const RECHECKS_AFTER_PREPARE: bool = false;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaiterPc {
    InitialCheck,
    Prepare,
    Recheck,
    BlockDecision,
    Finish,
    Done,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaiterState {
    Active,
    Armed,
    Blocked,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProducerPc {
    PublishCondition,
    Wake,
    Done,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    InitialCheck,
    Prepare(EntryId),
    Recheck,
    BlockDecision,
    Finish,
    PublishCondition,
    Wake { returned: usize, claimed: usize },
}

#[derive(Clone, Debug)]
pub struct TestPayload;

/// 测试 payload 永不 stale；B71 只验证登记与通知顺序。
fn test_payload_is_stale(_: &TestPayload) -> bool {
    false
}

#[derive(Clone)]
pub struct Waiter {
    pub pc: WaiterPc,
    pub state: WaiterState,
    pub entry: Option<Arc<WaitEntryCore<TestPayload>>>,
    pub claim_count: usize,
}

#[derive(Clone)]
pub struct ProtocolModel<P: WaitProtocol> {
    pub queue: WaitQueueCore<TestPayload>,
    pub condition: bool,
    pub waiter: Waiter,
    producer_pc: ProducerPc,
    pub last_wake: Option<WakeRecord>,
    pub observed_token: Option<usize>,
    marker: PhantomData<P>,
}

impl<P: WaitProtocol> ProtocolModel<P> {
    pub fn new() -> Self {
        Self {
            queue: WaitQueueCore::new_with_stale(test_payload_is_stale),
            condition: false,
            waiter: Waiter {
                pc: WaiterPc::InitialCheck,
                state: WaiterState::Active,
                entry: None,
                claim_count: 0,
            },
            producer_pc: ProducerPc::PublishCondition,
            last_wake: None,
            observed_token: None,
            marker: PhantomData,
        }
    }

    fn entry(&self) -> &Arc<WaitEntryCore<TestPayload>> {
        match &self.waiter.entry {
            Some(entry) => entry,
            None => panic!("waiter has no registered entry"),
        }
    }

    fn waiter_enabled(&self) -> bool {
        match self.waiter.pc {
            WaiterPc::InitialCheck
            | WaiterPc::Recheck
            | WaiterPc::BlockDecision => true,
            WaiterPc::Prepare => true,
            WaiterPc::Finish => match self.waiter.state {
                WaiterState::Blocked => !self.entry().token().is_waiting(),
                WaiterState::Active | WaiterState::Armed => true,
                WaiterState::Finished => false,
            },
            WaiterPc::Done => false,
        }
    }

    fn step_waiter(&mut self) -> Operation {
        match self.waiter.pc {
            WaiterPc::InitialCheck => {
                self.waiter.pc = if self.condition {
                    WaiterPc::Finish
                } else {
                    WaiterPc::Prepare
                };
                Operation::InitialCheck
            }
            WaiterPc::Prepare => {
                let entry = self.queue.prepare_entry(TestPayload);
                let id = entry.id();
                self.waiter.entry = Some(entry);
                self.waiter.state = WaiterState::Armed;
                self.waiter.pc = if P::RECHECKS_AFTER_PREPARE {
                    WaiterPc::Recheck
                } else {
                    WaiterPc::BlockDecision
                };
                Operation::Prepare(id)
            }
            WaiterPc::Recheck => {
                self.waiter.pc = WaiterPc::BlockDecision;
                if self.condition {
                    self.waiter.pc = WaiterPc::Finish;
                }
                Operation::Recheck
            }
            WaiterPc::BlockDecision => {
                let can_block = !P::RECHECKS_AFTER_PREPARE
                    || (!self.condition && self.entry().token().is_waiting());
                self.waiter.state = if can_block {
                    WaiterState::Blocked
                } else {
                    WaiterState::Armed
                };
                self.waiter.pc = WaiterPc::Finish;
                Operation::BlockDecision
            }
            WaiterPc::Finish => {
                if let Some(entry) = &self.waiter.entry {
                    entry.close();
                    let _ = self.queue.remove_entry(entry.id());
                }
                self.waiter.state = WaiterState::Finished;
                self.waiter.pc = WaiterPc::Done;
                Operation::Finish
            }
            WaiterPc::Done => panic!("disabled waiter was scheduled"),
        }
    }

    fn step_producer(&mut self) -> Operation {
        match self.producer_pc {
            ProducerPc::PublishCondition => {
                self.condition = true;
                self.producer_pc = ProducerPc::Wake;
                Operation::PublishCondition
            }
            ProducerPc::Wake => {
                let mut claimed = 0;
                let returned = self.queue.wake_at_most(usize::MAX, |_, _, attempt| {
                    if attempt.try_claim() {
                        claimed += 1;
                        true
                    } else {
                        false
                    }
                });
                self.waiter.claim_count += claimed;
                if claimed != 0 && self.waiter.state == WaiterState::Blocked {
                    self.waiter.state = WaiterState::Armed;
                }
                self.last_wake = Some(WakeRecord { returned, claimed });
                self.producer_pc = ProducerPc::Done;
                Operation::Wake { returned, claimed }
            }
            ProducerPc::Done => panic!("disabled producer was scheduled"),
        }
    }
}

impl<P: WaitProtocol> SchedulableModel for ProtocolModel<P> {
    fn enabled_actors(&self) -> Vec<ActorId> {
        let mut enabled = Vec::new();
        let producer_enabled =
            self.producer_pc != ProducerPc::Done && self.waiter.pc != WaiterPc::InitialCheck;
        if producer_enabled && self.waiter.pc == WaiterPc::Prepare {
            enabled.push(ActorId::Producer);
        }
        if self.waiter_enabled() {
            enabled.push(ActorId::Waiter);
        }
        if producer_enabled && self.waiter.pc != WaiterPc::Prepare {
            enabled.push(ActorId::Producer);
        }
        enabled
    }

    fn step(&mut self, actor: ActorId) -> StepRecord {
        let operation = match actor {
            ActorId::Waiter => self.step_waiter(),
            ActorId::Producer => self.step_producer(),
        };
        StepRecord { actor, operation }
    }

    fn is_quiescent(&self) -> bool {
        self.enabled_actors().is_empty()
    }

    fn check_invariants(&mut self) -> Result<(), InvariantViolation> {
        check_protocol(self)
    }
}
