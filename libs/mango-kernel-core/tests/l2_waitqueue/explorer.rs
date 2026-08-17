use crate::protocols::{ActorId, Operation};
use crate::invariants::InvariantViolation;

#[derive(Clone, Debug)]
pub struct StepRecord {
    pub actor: ActorId,
    pub operation: Operation,
}

pub trait SchedulableModel: Clone {
    fn enabled_actors(&self) -> Vec<ActorId>;
    fn step(&mut self, actor: ActorId) -> StepRecord;
    fn is_quiescent(&self) -> bool;
    fn check_invariants(&mut self) -> Result<(), InvariantViolation>;
}

#[derive(Clone, Copy)]
pub struct ExploreConfig {
    pub max_steps: usize,
    pub max_context_switches: usize,
    pub max_nodes: usize,
}

impl Default for ExploreConfig {
    fn default() -> Self {
        Self {
            max_steps: 8,
            max_context_switches: 8,
            max_nodes: 20_000,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExploreStats {
    pub explored_schedules: usize,
    pub max_depth: usize,
    pub max_context_switches: usize,
    pub visited_nodes: usize,
}

#[derive(Clone, Debug)]
pub struct Counterexample {
    pub invariant: InvariantViolation,
    pub schedule: Vec<usize>,
    pub operations: Vec<StepRecord>,
    pub stats: ExploreStats,
}

#[derive(Clone, Debug)]
pub enum ExplorationResult {
    Counterexample(Counterexample),
    ExhaustedWithinBounds(ExploreStats),
    InconclusiveResourceLimit(ExploreStats),
}

#[derive(Clone)]
struct Node<M> {
    model: M,
    last_actor: Option<ActorId>,
    steps_used: usize,
    context_switches_used: usize,
    schedule: Vec<usize>,
    operations: Vec<StepRecord>,
}

struct SearchState<M> {
    config: ExploreConfig,
    stats: ExploreStats,
    resource_limited: bool,
    initial: M,
}

pub fn explore<M: SchedulableModel>(model: M) -> ExplorationResult {
    explore_with_config(model, ExploreConfig::default())
}

pub fn explore_with_config<M: SchedulableModel>(model: M, config: ExploreConfig) -> ExplorationResult {
    let mut state = SearchState {
        config,
        stats: ExploreStats::default(),
        resource_limited: false,
        initial: model.clone(),
    };
    let node = Node {
        model,
        last_actor: None,
        steps_used: 0,
        context_switches_used: 0,
        schedule: Vec::new(),
        operations: Vec::new(),
    };
    if let Some(counterexample) = dfs(node, &mut state) {
        return ExplorationResult::Counterexample(counterexample);
    }
    if state.resource_limited {
        ExplorationResult::InconclusiveResourceLimit(state.stats)
    } else {
        ExplorationResult::ExhaustedWithinBounds(state.stats)
    }
}

fn dfs<M: SchedulableModel>(mut node: Node<M>, state: &mut SearchState<M>) -> Option<Counterexample> {
    if state.stats.visited_nodes >= state.config.max_nodes {
        state.resource_limited = true;
        return None;
    }
    state.stats.visited_nodes += 1;
    state.stats.max_depth = state.stats.max_depth.max(node.steps_used);
    state.stats.max_context_switches = state
        .stats
        .max_context_switches
        .max(node.context_switches_used);

    if let Err(invariant) = node.model.check_invariants() {
        return Some(Counterexample {
            invariant,
            schedule: node.schedule,
            operations: node.operations,
            stats: state.stats.clone(),
        });
    }
    if node.model.is_quiescent() || node.steps_used == state.config.max_steps {
        state.stats.explored_schedules += 1;
        return None;
    }

    for actor in node.model.enabled_actors() {
        let context_switches_used = match node.last_actor {
            Some(last_actor) if last_actor != actor => node.context_switches_used + 1,
            Some(_) | None => node.context_switches_used,
        };
        if context_switches_used > state.config.max_context_switches {
            continue;
        }
        let mut schedule = node.schedule.clone();
        schedule.push(actor.as_usize());
        let mut model = state.initial.clone();
        let mut operations = Vec::with_capacity(schedule.len());
        for scheduled_actor in schedule.iter().copied() {
            let Some(scheduled_actor) = ActorId::from_usize(scheduled_actor) else {
                return None;
            };
            operations.push(model.step(scheduled_actor));
        }
        let child = Node {
            model,
            last_actor: Some(actor),
            steps_used: node.steps_used + 1,
            context_switches_used,
            schedule,
            operations,
        };
        if let Some(counterexample) = dfs(child, state) {
            return Some(counterexample);
        }
        if state.resource_limited {
            return None;
        }
    }
    None
}

#[derive(Clone, Debug)]
pub enum ReplayError {
    Diverged { step: usize, actor: usize },
    CompletedWithoutCounterexample,
}

pub fn replay<M: SchedulableModel>(mut model: M, schedule: &[usize]) -> Result<Counterexample, ReplayError> {
    let mut operations = Vec::new();
    let mut last_actor = None;
    let mut context_switches = 0;
    for (step, raw_actor) in schedule.iter().copied().enumerate() {
        let Some(actor) = ActorId::from_usize(raw_actor) else {
            return Err(ReplayError::Diverged {
                step,
                actor: raw_actor,
            });
        };
        if !model.enabled_actors().contains(&actor) {
            return Err(ReplayError::Diverged {
                step,
                actor: raw_actor,
            });
        }
        if matches!(last_actor, Some(last) if last != actor) {
            context_switches += 1;
        }
        operations.push(model.step(actor));
        if let Err(invariant) = model.check_invariants() {
            return Ok(Counterexample {
                invariant,
                schedule: schedule[..=step].to_vec(),
                operations,
                stats: ExploreStats {
                    explored_schedules: 1,
                    max_depth: step + 1,
                    max_context_switches: context_switches,
                    visited_nodes: step + 1,
                },
            });
        }
        last_actor = Some(actor);
    }
    Err(ReplayError::CompletedWithoutCounterexample)
}
