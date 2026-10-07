//! Why a change was skipped or failed, or why a run stopped: stable codes the
//! UI translates (`reason.skip.<code>` …). English text for logs is in `runlog`.

use crate::model::SideKind;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "camelCase")]
pub enum SkipReason {
    Link,
    FileVsFolder,
    CaseCollision,
    NotRegularFile,
    Unreadable { side: SideKind, detail: String },
    DeletedSinceScan,
    ChangedSincePreview,
    BackOnSource,
    AlreadyGone,
    NoLongerInReplica,
    FolderNotEmpty,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "camelCase")]
pub enum FailReason {
    InUse,
    PermissionDenied,
    ChangedDuringCopy,
    TargetExists,
    OutsideReplica,
    /// The run stopped (drive gone or full) while this change was in progress.
    Interrupted,
    Io {
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "camelCase")]
pub enum StopReason {
    Cancelled,
    ReplicaDisconnected,
    SourceDisconnected,
    ReplicaFull,
}

impl SkipReason {
    pub fn code(&self) -> &'static str {
        match self {
            SkipReason::Link => "link",
            SkipReason::FileVsFolder => "fileVsFolder",
            SkipReason::CaseCollision => "caseCollision",
            SkipReason::NotRegularFile => "notRegularFile",
            SkipReason::Unreadable { .. } => "unreadable",
            SkipReason::DeletedSinceScan => "deletedSinceScan",
            SkipReason::ChangedSincePreview => "changedSincePreview",
            SkipReason::BackOnSource => "backOnSource",
            SkipReason::AlreadyGone => "alreadyGone",
            SkipReason::NoLongerInReplica => "noLongerInReplica",
            SkipReason::FolderNotEmpty => "folderNotEmpty",
        }
    }
}

impl FailReason {
    pub fn code(&self) -> &'static str {
        match self {
            FailReason::InUse => "inUse",
            FailReason::PermissionDenied => "permissionDenied",
            FailReason::ChangedDuringCopy => "changedDuringCopy",
            FailReason::TargetExists => "targetExists",
            FailReason::OutsideReplica => "outsideReplica",
            FailReason::Interrupted => "interrupted",
            FailReason::Io { .. } => "io",
        }
    }
}

impl StopReason {
    pub fn code(&self) -> &'static str {
        match self {
            StopReason::Cancelled => "cancelled",
            StopReason::ReplicaDisconnected => "replicaDisconnected",
            StopReason::SourceDisconnected => "sourceDisconnected",
            StopReason::ReplicaFull => "replicaFull",
        }
    }
}

// When adding a variant, add a sample here too: the UI's translation check
// is driven by this list. The tests' `*_sample_index` matches have no
// wildcard, so a new variant does not compile until it is given an index
// there, and `samples_cover_every_variant` then fails until it is listed here.
fn all_skip() -> Vec<SkipReason> {
    vec![
        SkipReason::Link,
        SkipReason::FileVsFolder,
        SkipReason::CaseCollision,
        SkipReason::NotRegularFile,
        SkipReason::Unreadable {
            side: SideKind::Source,
            detail: String::new(),
        },
        SkipReason::DeletedSinceScan,
        SkipReason::ChangedSincePreview,
        SkipReason::BackOnSource,
        SkipReason::AlreadyGone,
        SkipReason::NoLongerInReplica,
        SkipReason::FolderNotEmpty,
    ]
}

fn all_fail() -> Vec<FailReason> {
    vec![
        FailReason::InUse,
        FailReason::PermissionDenied,
        FailReason::ChangedDuringCopy,
        FailReason::TargetExists,
        FailReason::OutsideReplica,
        FailReason::Interrupted,
        FailReason::Io {
            detail: String::new(),
        },
    ]
}

fn all_stop() -> Vec<StopReason> {
    vec![
        StopReason::Cancelled,
        StopReason::ReplicaDisconnected,
        StopReason::SourceDisconnected,
        StopReason::ReplicaFull,
    ]
}

/// Every reason code as `skip.<code>` / `fail.<code>` / `stop.<code>`.
pub fn reason_codes() -> Vec<String> {
    all_skip()
        .iter()
        .map(|r| format!("skip.{}", r.code()))
        .chain(all_fail().iter().map(|r| format!("fail.{}", r.code())))
        .chain(all_stop().iter().map(|r| format!("stop.{}", r.code())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SideKind;

    // One arm per variant and no wildcard: a new variant fails to compile here.
    // Each variant's index is its position in the sample list.
    fn skip_sample_index(r: &SkipReason) -> usize {
        match r {
            SkipReason::Link => 0,
            SkipReason::FileVsFolder => 1,
            SkipReason::CaseCollision => 2,
            SkipReason::NotRegularFile => 3,
            SkipReason::Unreadable { .. } => 4,
            SkipReason::DeletedSinceScan => 5,
            SkipReason::ChangedSincePreview => 6,
            SkipReason::BackOnSource => 7,
            SkipReason::AlreadyGone => 8,
            SkipReason::NoLongerInReplica => 9,
            SkipReason::FolderNotEmpty => 10,
        }
    }
    const SKIP_VARIANTS: usize = 11;

    fn fail_sample_index(r: &FailReason) -> usize {
        match r {
            FailReason::InUse => 0,
            FailReason::PermissionDenied => 1,
            FailReason::ChangedDuringCopy => 2,
            FailReason::TargetExists => 3,
            FailReason::OutsideReplica => 4,
            FailReason::Interrupted => 5,
            FailReason::Io { .. } => 6,
        }
    }
    const FAIL_VARIANTS: usize = 7;

    fn stop_sample_index(r: &StopReason) -> usize {
        match r {
            StopReason::Cancelled => 0,
            StopReason::ReplicaDisconnected => 1,
            StopReason::SourceDisconnected => 2,
            StopReason::ReplicaFull => 3,
        }
    }
    const STOP_VARIANTS: usize = 4;

    /// The sample at position i is the variant with index i, for every index
    /// up to the variant count: no variant is missing or listed twice.
    fn assert_covers<T: std::fmt::Debug>(samples: &[T], index: fn(&T) -> usize, variants: usize) {
        let got: Vec<usize> = samples.iter().map(index).collect();
        assert_eq!(got, (0..variants).collect::<Vec<_>>(), "{samples:?}");
    }

    #[test]
    fn samples_cover_every_variant() {
        assert_covers(&all_skip(), skip_sample_index, SKIP_VARIANTS);
        assert_covers(&all_fail(), fail_sample_index, FAIL_VARIANTS);
        assert_covers(&all_stop(), stop_sample_index, STOP_VARIANTS);
    }

    #[test]
    fn serde_tag_equals_code() {
        for r in all_skip() {
            let v = serde_json::to_value(&r).unwrap();
            assert_eq!(v["code"], r.code(), "{r:?}");
        }
        for r in all_fail() {
            let v = serde_json::to_value(&r).unwrap();
            assert_eq!(v["code"], r.code(), "{r:?}");
        }
        for r in all_stop() {
            let v = serde_json::to_value(r).unwrap();
            assert_eq!(v["code"], r.code(), "{r:?}");
        }
    }

    #[test]
    fn unreadable_carries_side_and_detail() {
        let v = serde_json::to_value(SkipReason::Unreadable {
            side: SideKind::Replica,
            detail: "boom".into(),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"code": "unreadable", "side": "Replica", "detail": "boom"})
        );
    }

    #[test]
    fn reason_codes_list_every_code_once_with_its_family() {
        let codes = reason_codes();
        assert_eq!(codes.len(), 11 + 7 + 4);
        assert!(codes.contains(&"skip.changedSincePreview".to_string()));
        assert!(codes.contains(&"fail.interrupted".to_string()));
        assert!(codes.contains(&"stop.sourceDisconnected".to_string()));
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len());
    }
}
