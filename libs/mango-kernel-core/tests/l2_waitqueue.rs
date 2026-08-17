#[path = "l2_waitqueue/explorer.rs"]
mod explorer;
#[path = "l2_waitqueue/generator.rs"]
mod generator;
#[path = "l2_waitqueue/invariants.rs"]
mod invariants;
#[path = "l2_waitqueue/protocols.rs"]
mod protocols;
#[path = "l2_waitqueue/scale.rs"]
mod scale;

use std::env;

use explorer::{explore, replay, Counterexample, ExplorationResult, ReplayError};
use protocols::{B71LegacyProtocol, CorrectWaitProtocol, ProtocolModel, WaitProtocol};

fn case_is_selected(case: &str) -> bool {
    match env::var("MANGO_L2_CASE") {
        Ok(selected) => selected == case,
        Err(_) => true,
    }
}

fn replay_schedule() -> Option<Vec<usize>> {
    let raw = env::var("MANGO_L2_SCHEDULE").ok()?;
    raw.split(',')
        .map(|part| part.parse::<usize>())
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn seed() -> u64 {
    env::var("MANGO_L2_SEED")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1234)
}

fn print_counterexample(case: &str, counterexample: &Counterexample) {
    let schedule = counterexample
        .schedule
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    println!("L2 WaitQueue: FAIL");
    println!("invariant: {}", counterexample.invariant.name);
    println!("detail: {}", counterexample.invariant.detail);
    println!("schedule: {schedule}");
    println!(
        "operations: {}",
        counterexample
            .operations
            .iter()
            .map(|record| format!("{}:{:?}", record.actor.as_usize(), record.operation))
            .collect::<Vec<_>>()
            .join(" -> ")
    );
    println!("explored schedules: {}", counterexample.stats.explored_schedules);
    println!("seed: {}", seed());
    println!(
        "replay: MANGO_L2_CASE={case} MANGO_L2_SEED={} MANGO_L2_SCHEDULE={schedule} cargo test -p mango-kernel-core --test l2_waitqueue -- --exact known_bad_b71_is_discovered --nocapture",
        seed()
    );
}

fn print_pass(result: &explorer::ExploreStats) {
    println!("L2 WaitQueue: PASS");
    println!("explored schedules: {}", result.explored_schedules);
    println!("max depth: {}", result.max_depth);
    println!("max context switches: {}", result.max_context_switches);
}

#[test]
fn known_bad_b71_is_discovered() {
    if !case_is_selected(B71LegacyProtocol::CASE) {
        return;
    }
    let result = match replay_schedule() {
        Some(schedule) => match replay(ProtocolModel::<B71LegacyProtocol>::new(), &schedule) {
            Ok(counterexample) => ExplorationResult::Counterexample(counterexample),
            Err(ReplayError::Diverged { step, actor }) => {
                panic!("schedule diverged at step {} for actor {}", step, actor)
            }
            Err(ReplayError::CompletedWithoutCounterexample) => {
                panic!("replay completed without counterexample")
            }
        },
        None => explore(ProtocolModel::<B71LegacyProtocol>::new()),
    };

    match result {
        ExplorationResult::Counterexample(counterexample) => {
            print_counterexample(B71LegacyProtocol::CASE, &counterexample);
            assert_eq!(counterexample.invariant.name, "quiescent_satisfied_condition");
        }
        ExplorationResult::ExhaustedWithinBounds(stats) => {
            print_pass(&stats);
            panic!("B71 legacy protocol must expose a counterexample")
        }
        ExplorationResult::InconclusiveResourceLimit(stats) => {
            print_pass(&stats);
            panic!("resource limit is not a passing result")
        }
    }
}

#[test]
fn correct_protocol_exhausts_without_lost_wake() {
    if !case_is_selected(CorrectWaitProtocol::CASE) {
        return;
    }
    match explore(ProtocolModel::<CorrectWaitProtocol>::new()) {
        ExplorationResult::ExhaustedWithinBounds(stats) => print_pass(&stats),
        ExplorationResult::Counterexample(counterexample) => {
            print_counterexample(CorrectWaitProtocol::CASE, &counterexample);
            panic!("correct protocol produced a counterexample")
        }
        ExplorationResult::InconclusiveResourceLimit(stats) => {
            print_pass(&stats);
            panic!("resource limit is not a passing result")
        }
    }
}

#[test]
fn l2_scale_10() {
    scale::run_selected_scale("scale_10", 10, &[11, 29, 47]);
}

#[test]
fn l2_scale_100() {
    scale::run_selected_scale("scale_100", 100, &[101, 211, 307]);
}

#[test]
fn l2_scale_1000() {
    scale::run_selected_scale("scale_1000", 1_000, &[1_001, 2_009, 3_007]);
}

#[test]
fn l2_scale_10000() {
    scale::run_selected_scale("scale_10000", 10_000, &[10_007, 20_011, 30_013]);
}
