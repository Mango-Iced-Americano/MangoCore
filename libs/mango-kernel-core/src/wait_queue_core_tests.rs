use super::wait_queue_core::{WaitQueueCore, WaitToken};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Payload {
    task: usize,
    stale: bool,
}

fn payload(task: usize) -> Payload {
    Payload { task, stale: false }
}

fn payload_is_stale(payload: &Payload) -> bool {
    payload.stale
}

#[test]
fn token_is_monotonic_when_notified() {
    let token = WaitToken::new_waiting();

    assert_eq!(token.token_state(), WaitToken::WAITING);
    assert!(token.notify());
    assert_eq!(token.token_state(), WaitToken::NOTIFIED);
    token.close();
    assert_eq!(token.token_state(), WaitToken::NOTIFIED);
}

#[test]
fn notify_has_one_winner_when_called_twice() {
    let token = WaitToken::new_waiting();

    assert!(token.notify());
    assert!(!token.notify());
}

#[test]
fn close_prevents_a_later_notification() {
    let token = WaitToken::new_waiting();

    token.close();

    assert_eq!(token.token_state(), WaitToken::CLOSED);
    assert!(!token.notify());
}

#[test]
fn is_waiting_changes_after_notification() {
    let token = WaitToken::new_waiting();

    assert!(token.is_waiting());
    assert!(token.notify());

    assert!(!token.is_waiting());
}

#[test]
fn prepare_and_wake_preserve_fifo_order() {
    let mut queue = WaitQueueCore::new();
    let first = queue.prepare_to_wait(payload(1));
    let second = queue.prepare_to_wait(payload(2));
    let mut woken = [0; 2];
    let mut count = 0;

    let woke = queue.wake_at_most(2, |entry, _, attempt| {
        woken[count] = entry.task;
        count += 1;
        attempt.try_claim()
    });

    assert_eq!(woke, 2);
    assert_eq!(woken, [1, 2]);
    assert!(!queue.contains_entry(first));
    assert!(!queue.contains_entry(second));
}

#[test]
fn wake_at_most_stops_at_limit_and_keeps_later_entries() {
    let mut queue = WaitQueueCore::new();
    let first = queue.prepare_to_wait(payload(1));
    let second = queue.prepare_to_wait(payload(2));
    let third = queue.prepare_to_wait(payload(3));

    let woke = queue.wake_at_most(2, |_, _, attempt| attempt.try_claim());

    assert_eq!(woke, 2);
    assert!(!queue.contains_entry(first));
    assert!(!queue.contains_entry(second));
    assert!(queue.contains_entry(third));
}

#[test]
fn wake_one_removes_only_through_first_eligible_entry() {
    let mut queue = WaitQueueCore::new();
    let stale = queue.prepare_to_wait(payload(1));
    let first = queue.prepare_to_wait(payload(2));
    let later = queue.prepare_to_wait(payload(3));

    let woke = queue.wake_one(|entry, _, attempt| {
        if entry.task == 1 {
            attempt.close();
            false
        } else {
            attempt.try_claim()
        }
    });

    assert_eq!(woke, 1);
    assert!(!queue.contains_entry(stale));
    assert!(!queue.contains_entry(first));
    assert!(queue.contains_entry(later));
}

#[test]
fn finish_entry_removes_only_the_target_round() {
    let mut queue = WaitQueueCore::new();
    let first = queue.prepare_to_wait(payload(7));
    let second = queue.prepare_to_wait(payload(7));

    assert!(queue.finish_entry(first));

    assert!(!queue.contains_entry(first));
    assert!(queue.contains_entry(second));
}

#[test]
fn finish_wait_removes_all_rounds_for_matching_task() {
    let mut queue = WaitQueueCore::new();
    let first = queue.prepare_to_wait(payload(7));
    let second = queue.prepare_to_wait(payload(7));
    let other = queue.prepare_to_wait(payload(8));

    assert!(queue.finish_wait(|entry| entry.task == 7));

    assert!(!queue.contains_entry(first));
    assert!(!queue.contains_entry(second));
    assert!(queue.contains_entry(other));
}

#[test]
fn remove_stale_reclaims_only_stale_entries() {
    let mut queue = WaitQueueCore::new_with_stale(payload_is_stale);
    let stale = queue.prepare_to_wait(Payload {
        task: 1,
        stale: true,
    });
    let live = queue.prepare_to_wait(payload(2));

    assert_eq!(queue.remove_stale(payload_is_stale), 1);
    assert!(!queue.contains_entry(stale));
    assert!(queue.contains_entry(live));
}
