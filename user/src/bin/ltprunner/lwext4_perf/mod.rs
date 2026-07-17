mod snapshot;

use user_lib::println;

use snapshot::{perf_diag_enabled, read_lwext4_snapshot, Snapshot, SnapshotError};

enum DiagState {
    Disabled,
    Enabled,
    Unavailable {
        reason: DisabledReason,
        emitted: bool,
    },
}

#[derive(Clone, Copy)]
enum DisabledReason {
    FeatureProbe,
    BeforeSnapshotRead,
    BeforeSnapshotParse,
    AfterSnapshotRead,
    AfterSnapshotParse,
}

impl DisabledReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::FeatureProbe => "feature_probe",
            Self::BeforeSnapshotRead => "before_snapshot_read",
            Self::BeforeSnapshotParse => "before_snapshot_parse",
            Self::AfterSnapshotRead => "after_snapshot_read",
            Self::AfterSnapshotParse => "after_snapshot_parse",
        }
    }

    const fn before_snapshot(error: SnapshotError) -> Self {
        match error {
            SnapshotError::Read => Self::BeforeSnapshotRead,
            SnapshotError::StrictParse => Self::BeforeSnapshotParse,
        }
    }

    const fn after_snapshot(error: SnapshotError) -> Self {
        match error {
            SnapshotError::Read => Self::AfterSnapshotRead,
            SnapshotError::StrictParse => Self::AfterSnapshotParse,
        }
    }
}

pub struct LwExt4PerfDiag {
    state: DiagState,
}

impl LwExt4PerfDiag {
    pub fn new(log_enabled: bool) -> Self {
        let state = if !log_enabled {
            DiagState::Disabled
        } else if perf_diag_enabled() {
            DiagState::Enabled
        } else {
            DiagState::Unavailable {
                reason: DisabledReason::FeatureProbe,
                emitted: false,
            }
        };
        Self { state }
    }

    pub fn before_case(&mut self) -> Option<Snapshot> {
        if !matches!(self.state, DiagState::Enabled) {
            return None;
        }
        match read_lwext4_snapshot() {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                self.make_unavailable(DisabledReason::before_snapshot(error));
                None
            }
        }
    }

    pub fn after_case(&mut self, before: Option<Snapshot>, case_index: usize, exit_status: i32) {
        let Some(before) = before else {
            self.emit_unavailable_once();
            return;
        };
        if !matches!(self.state, DiagState::Enabled) {
            self.emit_unavailable_once();
            return;
        }
        let after = match read_lwext4_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.make_unavailable(DisabledReason::after_snapshot(error));
                self.emit_unavailable_once();
                return;
            }
        };
        print_delta(case_index, exit_status, after.wrapping_delta(before));
    }

    fn make_unavailable(&mut self, reason: DisabledReason) {
        self.state = DiagState::Unavailable {
            reason,
            emitted: false,
        };
    }

    fn emit_unavailable_once(&mut self) {
        if let DiagState::Unavailable { reason, emitted } = &mut self.state {
            if !*emitted {
                println!(
                    "[ltprunner] lwext4-perf status=unavailable reason={}",
                    reason.as_str()
                );
                *emitted = true;
            }
        }
    }
}

fn print_delta(case_index: usize, exit_status: i32, deltas: [u64; snapshot::COUNTER_COUNT]) {
    println!(
        "[ltprunner] lwext4-perf case_index={} exit_status={} deltas={},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        case_index,
        exit_status,
        deltas[0],
        deltas[1],
        deltas[2],
        deltas[3],
        deltas[4],
        deltas[5],
        deltas[6],
        deltas[7],
        deltas[8],
        deltas[9],
        deltas[10],
        deltas[11],
        deltas[12],
        deltas[13],
        deltas[14],
        deltas[15],
        deltas[16],
        deltas[17],
        deltas[18],
        deltas[19],
        deltas[20],
        deltas[21],
        deltas[22],
        deltas[23],
    );
}
