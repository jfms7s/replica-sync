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
// is driven by this list.
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
