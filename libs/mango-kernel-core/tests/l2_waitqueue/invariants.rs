use mango_kernel_core::wait_queue_core::WaitToken;

use crate::explorer::SchedulableModel;
use crate::protocols::{ProtocolModel, WaitProtocol, WaiterState};

#[derive(Clone, Debug)]
pub struct InvariantViolation {
    pub name: &'static str,
    pub detail: String,
}

#[derive(Clone, Copy, Debug)]
pub struct WakeRecord {
    pub returned: usize,
    pub claimed: usize,
}

pub fn check_token_transition(previous: Option<usize>, current: usize) -> Result<(), InvariantViolation> {
    let Some(previous) = previous else {
        return Ok(());
    };
    let valid = match previous {
        WaitToken::WAITING => {
            current == WaitToken::WAITING
                || current == WaitToken::NOTIFIED
                || current == WaitToken::CLOSED
        }
        WaitToken::NOTIFIED => current == WaitToken::NOTIFIED,
        WaitToken::CLOSED => current == WaitToken::CLOSED,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(InvariantViolation {
            name: "token_monotonic",
            detail: format!("token transitioned from {previous} to {current}"),
        })
    }
}

pub fn check_wake_record(record: WakeRecord, limit: usize) -> Result<(), InvariantViolation> {
    if record.returned == record.claimed && record.returned <= limit {
        Ok(())
    } else {
        Err(InvariantViolation {
            name: "wake_claim_count",
            detail: format!(
                "returned={}, claimed={}, limit={limit}",
                record.returned, record.claimed
            ),
        })
    }
}

pub fn check_protocol<P: WaitProtocol>(
    model: &mut ProtocolModel<P>,
) -> Result<(), InvariantViolation> {
    if let Some(entry) = &model.waiter.entry {
        let current = entry.token().token_state();
        check_token_transition(model.observed_token, current)?;
        model.observed_token = Some(current);

        if model.waiter.claim_count > 1 {
            return Err(InvariantViolation {
                name: "single_claim",
                detail: format!("entry {} claimed {} times", entry.id().get(), model.waiter.claim_count),
            });
        }

        if model.waiter.state == WaiterState::Blocked && !entry.token().is_waiting() {
            return Err(InvariantViolation {
                name: "checked_block_after_claim",
                detail: "waiter remained Blocked after its token was claimed".to_string(),
            });
        }

        if model.waiter.state == WaiterState::Finished && model.queue.contains_entry(entry.id()) {
            return Err(InvariantViolation {
                name: "finish_entry_exact",
                detail: "finished entry remained queued".to_string(),
            });
        }
    }

    if let Some(record) = model.last_wake {
        check_wake_record(record, usize::MAX)?;
    }

    if model.is_quiescent()
        && model.condition
        && matches!(model.waiter.state, WaiterState::Blocked | WaiterState::Armed)
    {
        return Err(InvariantViolation {
            name: "quiescent_satisfied_condition",
            detail: "condition is true while waiter is permanently blocked or armed".to_string(),
        });
    }
    Ok(())
}
