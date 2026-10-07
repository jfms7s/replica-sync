//! One call that turns two folders into a plan: the entry point for the app and the CLI.

use crate::diff::{CaseMode, diff};
use crate::model::Snapshot;
use crate::moves::detect_moves;
use crate::plan::{Plan, PlanError, build_plan};
use crate::rules::SkipRules;
use crate::scan::{ScanCounters, ScanError, scan};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanStats {
    pub files: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Default)]
pub struct SessionCounters {
    pub source: ScanCounters,
    pub replica: ScanCounters,
}

impl SessionCounters {
    pub fn cancel(&self) {
        self.source.cancel();
        self.replica.cancel();
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Prepared {
    pub plan: Plan,
    pub source: ScanStats,
    pub replica: ScanStats,
    pub replica_files: u64,
    pub leftovers_removed: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("source: {0}")]
    Source(ScanError),
    #[error("replica: {0}")]
    Replica(ScanError),
    #[error(transparent)]
    Plan(#[from] PlanError),
}

fn stats(s: &Snapshot, elapsed: Duration) -> ScanStats {
    let files = s.file_count();
    let bytes = s.entries.iter().map(|e| e.size).sum();
    ScanStats {
        files,
        bytes,
        elapsed_ms: elapsed.as_millis() as u64,
    }
}

pub fn prepare(
    source_root: &Path,
    replica_root: &Path,
    rules: &SkipRules,
    case: CaseMode,
    counters: &SessionCounters,
) -> Result<Prepared, PrepareError> {
    let started = Instant::now();
    let timed = |root: &Path, c: &ScanCounters| {
        let t = Instant::now();
        scan(root, rules, c).map(|s| (s, t.elapsed()))
    };
    let (src, rep) = rayon::join(
        || timed(source_root, &counters.source),
        || timed(replica_root, &counters.replica),
    );
    let (src, src_t) = src.map_err(PrepareError::Source)?;
    let (rep, rep_t) = rep.map_err(PrepareError::Replica)?;
    // Temp files from an interrupted run: never user data, safe to delete.
    let leftovers_removed = rep
        .leftovers
        .iter()
        .filter(|r| fs::remove_file(r.to_path(replica_root)).is_ok())
        .count();
    let plan = build_plan(detect_moves(diff(&src, &rep, case), &src, &rep, case), case)?;
    Ok(Prepared {
        plan,
        source: stats(&src, src_t),
        replica: stats(&rep, rep_t),
        replica_files: rep.file_count(),
        leftovers_removed,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{T0, write_file};

    #[test]
    fn prepare_plans_and_removes_leftover_temp_files() {
        let d = tempfile::tempdir().unwrap();
        let (s, r) = (d.path().join("s"), d.path().join("r"));
        write_file(&s, "a.txt", b"1", T0);
        write_file(&r, "b.jpg.replica-sync.tmp", b"partial", T0);
        let p = prepare(
            &s,
            &r,
            &SkipRules::new(&[]).unwrap(),
            CaseMode::Sensitive,
            &SessionCounters::default(),
        )
        .unwrap();
        assert_eq!(p.plan.totals.creates, 1);
        assert_eq!(p.leftovers_removed, 1);
        assert!(!r.join("b.jpg.replica-sync.tmp").exists());
        assert_eq!((p.source.files, p.replica_files), (1, 0));
    }

    #[test]
    fn cancelled_prepare_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let counters = SessionCounters::default();
        counters.cancel();
        let err = prepare(
            d.path(),
            d.path(),
            &SkipRules::new(&[]).unwrap(),
            CaseMode::Sensitive,
            &counters,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PrepareError::Source(ScanError::Cancelled)
                | PrepareError::Replica(ScanError::Cancelled)
        ));
    }
}
