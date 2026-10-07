//! Errors the UI can translate: `error.<code>` with `params` filled in.

use replica_sync_core::model::SideKind;
use replica_sync_core::pairs::{PairError, ResolveError};
use replica_sync_core::plan::PlanError;
use replica_sync_core::rules::RuleError;
use replica_sync_core::scan::ScanError;
use replica_sync_core::session::PrepareError;
use replica_sync_core::trash::RestoreError;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppError {
    pub code: &'static str,
    pub params: BTreeMap<&'static str, String>,
}

impl AppError {
    pub fn new(code: &'static str) -> AppError {
        AppError {
            code,
            params: BTreeMap::new(),
        }
    }

    pub fn with(mut self, key: &'static str, value: impl ToString) -> AppError {
        self.params.insert(key, value.to_string());
        self
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {:?}", self.code, self.params)
    }
}

impl std::error::Error for AppError {}

/// Every code an `AppError` can carry. The UI's translation check reads this list.
pub const ERROR_CODES: &[&str] = &[
    "pair.sameFolder",
    "pair.nested",
    "pair.sameVolume",
    "pair.badRule",
    "pair.notFound",
    "pair.foldersRequired",
    "pair.nameRequired",
    "pair.badTrashDays",
    "drive.notConnected",
    "drive.folderMissing",
    "drive.wrongVolume",
    "scan.cancelled",
    "scan.unreadableRoot",
    "scan.nested",
    "scan.planConflict",
    "trash.conflict",
    "trash.notInRun",
    "trash.unsafe",
    "apply.wrongFolderUnconfirmed",
    "apply.noPlan",
    "apply.busy",
    "apply.spaceShortfall",
    "apply.nothingToRetry",
    "apply.nothingSelected",
    "store.unreadable",
    "io",
];

fn side_name(side: SideKind) -> &'static str {
    match side {
        SideKind::Source => "source",
        SideKind::Replica => "replica",
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> AppError {
        AppError::new("io").with("detail", e)
    }
}

impl From<RuleError> for AppError {
    fn from(e: RuleError) -> AppError {
        AppError::new("pair.badRule")
            .with("pattern", e.pattern)
            .with("detail", e.message)
    }
}

impl From<PairError> for AppError {
    fn from(e: PairError) -> AppError {
        match e {
            PairError::SameFolder => AppError::new("pair.sameFolder"),
            PairError::Nested => AppError::new("pair.nested"),
            PairError::SameVolume => AppError::new("pair.sameVolume"),
            PairError::Rule(r) => r.into(),
            PairError::NotFound(id) => AppError::new("pair.notFound").with("id", id),
            PairError::Io(e) => e.into(),
        }
    }
}

impl From<ResolveError> for AppError {
    fn from(e: ResolveError) -> AppError {
        match e {
            ResolveError::NotConnected { side, label } => AppError::new("drive.notConnected")
                .with("side", side_name(side))
                .with("label", label),
            ResolveError::FolderMissing(p) => {
                AppError::new("drive.folderMissing").with("path", p.display())
            }
            ResolveError::WrongVolume(p) => {
                AppError::new("drive.wrongVolume").with("path", p.display())
            }
            ResolveError::Io(e) => e.into(),
        }
    }
}

impl From<ScanError> for AppError {
    fn from(e: ScanError) -> AppError {
        match e {
            ScanError::Cancelled => AppError::new("scan.cancelled"),
            ScanError::Root { path, source } => AppError::new("scan.unreadableRoot")
                .with("path", path.display())
                .with("detail", source),
        }
    }
}

impl From<PrepareError> for AppError {
    fn from(e: PrepareError) -> AppError {
        match e {
            PrepareError::Source(e) | PrepareError::Replica(e) => e.into(),
            PrepareError::Nested => AppError::new("scan.nested"),
            PrepareError::Plan(PlanError::DuplicateTarget(p)) => {
                AppError::new("scan.planConflict").with("path", p)
            }
        }
    }
}

impl From<RestoreError> for AppError {
    fn from(e: RestoreError) -> AppError {
        match e {
            RestoreError::Conflict(p) => AppError::new("trash.conflict").with("path", p),
            RestoreError::NotInRun(p) => AppError::new("trash.notInRun").with("path", p),
            RestoreError::Unsafe(e) => AppError::new("trash.unsafe").with("detail", e),
            RestoreError::Io(e) => e.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica_sync_core::model::{RelPath, SideKind};
    use replica_sync_core::pairs::{PairError, ResolveError};
    use replica_sync_core::rules::RuleError;
    use replica_sync_core::trash::RestoreError;
    use std::path::PathBuf;

    fn samples() -> Vec<AppError> {
        vec![
            PairError::SameFolder.into(),
            PairError::Nested.into(),
            PairError::SameVolume.into(),
            PairError::Rule(RuleError {
                pattern: "[".into(),
                message: "bad".into(),
            })
            .into(),
            PairError::NotFound("x".into()).into(),
            ResolveError::NotConnected {
                side: SideKind::Replica,
                label: "USB".into(),
            }
            .into(),
            ResolveError::FolderMissing(PathBuf::from("/a")).into(),
            ResolveError::WrongVolume(PathBuf::from("/b")).into(),
            replica_sync_core::scan::ScanError::Cancelled.into(),
            replica_sync_core::session::PrepareError::Nested.into(),
            RestoreError::Conflict(RelPath::new("a").unwrap()).into(),
            RestoreError::NotInRun(RelPath::new("a").unwrap()).into(),
            std::io::Error::other("boom").into(),
        ]
    }

    #[test]
    fn every_conversion_uses_a_listed_code() {
        for e in samples() {
            assert!(
                ERROR_CODES.contains(&e.code),
                "{} not in ERROR_CODES",
                e.code
            );
        }
    }

    #[test]
    fn params_carry_raw_values() {
        let e: AppError = ResolveError::NotConnected {
            side: SideKind::Replica,
            label: "USB".into(),
        }
        .into();
        assert_eq!(e.code, "drive.notConnected");
        assert_eq!(e.params["label"], "USB");
        assert_eq!(e.params["side"], "replica");
        let e: AppError = PairError::Rule(RuleError {
            pattern: "[".into(),
            message: "bad".into(),
        })
        .into();
        assert_eq!(
            (e.code, e.params["pattern"].as_str()),
            ("pair.badRule", "[")
        );
    }

    #[test]
    fn serializes_as_code_and_params() {
        let v = serde_json::to_value(AppError::new("io").with("detail", "x")).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"code": "io", "params": {"detail": "x"}})
        );
    }

    #[test]
    fn error_codes_are_unique() {
        let set: std::collections::BTreeSet<_> = ERROR_CODES.iter().collect();
        assert_eq!(set.len(), ERROR_CODES.len());
    }
}
