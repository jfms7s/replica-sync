//! Plain-text plan and run logs, kept for 90 days.

use crate::execute::{Outcome, RunReport, StopReason};
use crate::model::{Change, MoveKind, SideKind};
use crate::plan::{ChangeId, Plan, selected_totals};
use crate::reason::{FailReason, SkipReason};
use crate::session::Prepared;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const LOG_RETENTION: Duration = Duration::from_secs(90 * 24 * 60 * 60);

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

pub fn skip_text(r: &SkipReason) -> String {
    match r {
        SkipReason::Link => "link (not followed)".into(),
        SkipReason::FileVsFolder => "a file on one side and a folder on the other".into(),
        SkipReason::CaseCollision => {
            "two names differ only by capital letters; the backup drive can't hold both".into()
        }
        SkipReason::NotRegularFile => "not a regular file".into(),
        SkipReason::Unreadable {
            side: SideKind::Source,
            detail,
        } => format!("could not read on the source: {detail}"),
        SkipReason::Unreadable {
            side: SideKind::Replica,
            detail,
        } => format!("could not read on the replica: {detail}"),
        SkipReason::DeletedSinceScan => "deleted since scan".into(),
        SkipReason::ChangedSincePreview => "changed since preview".into(),
        SkipReason::BackOnSource => "back on the source since the preview".into(),
        SkipReason::AlreadyGone => "already gone".into(),
        SkipReason::NoLongerInReplica => "no longer in the replica".into(),
        SkipReason::FolderNotEmpty => "folder not empty".into(),
    }
}

pub fn fail_text(r: &FailReason) -> String {
    match r {
        FailReason::InUse => "in use by another program".into(),
        FailReason::PermissionDenied => "permission denied".into(),
        FailReason::ChangedDuringCopy => "file changed during copy".into(),
        FailReason::TargetExists => "something already exists at the new location".into(),
        FailReason::OutsideReplica => "outside the replica folder".into(),
        FailReason::Interrupted => "interrupted: the run stopped".into(),
        FailReason::Io { detail } => detail.clone(),
    }
}

pub fn stop_text(r: &StopReason) -> String {
    match r {
        StopReason::Cancelled => "cancelled by the user".into(),
        StopReason::ReplicaDisconnected => "the backup drive was disconnected".into(),
        StopReason::SourceDisconnected => "the source drive was disconnected".into(),
        StopReason::ReplicaFull => "the backup drive is full".into(),
    }
}

pub fn describe(c: &Change) -> String {
    match c {
        Change::Create { path, size, .. } => format!("create   {path} ({})", human_bytes(*size)),
        Change::Update {
            path,
            size,
            replica_newer,
            ..
        } => {
            format!(
                "update   {path} ({}){}",
                human_bytes(*size),
                if *replica_newer {
                    " [replica is newer]"
                } else {
                    ""
                }
            )
        }
        Change::Delete { path, .. } => format!("delete   {path}"),
        Change::Move {
            from,
            to,
            kind: MoveKind::Dir { files, .. },
        } if *files > 0 => format!("move     {from} -> {to} ({files} files)"),
        Change::Move { from, to, .. } => format!("move     {from} -> {to}"),
        Change::MkDir { path } => format!("mkdir    {path}"),
        Change::RmDir { path } => format!("rmdir    {path}"),
        Change::Skipped { path, reason } => format!("skipped  {path}: {}", skip_text(reason)),
    }
}

pub fn render_plan(plan: &Plan, approved: &HashSet<ChangeId>) -> String {
    let mut out = String::new();
    for p in &plan.changes {
        let mark = match (&p.change, approved.contains(&p.id)) {
            (Change::Skipped { .. }, _) => " - ",
            (_, true) => "[x]",
            (_, false) => "[ ]",
        };
        let _ = writeln!(out, "{mark} {}", describe(&p.change));
    }
    let t = selected_totals(plan, approved);
    let _ = writeln!(
        out,
        "selected: {} create, {} update, {} move, {} delete, {} folders; {} to copy; {} skipped",
        t.creates,
        t.updates,
        t.moves,
        t.deletes,
        t.folders,
        human_bytes(t.bytes_to_copy),
        plan.totals.skipped
    );
    out
}

pub fn render_run(
    pair_name: &str,
    prepared: &Prepared,
    approved: &HashSet<ChangeId>,
    report: &RunReport,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "pair: {pair_name}");
    let _ = writeln!(
        out,
        "scanned: source {} files ({}), replica {} files ({}) in {} ms",
        prepared.source.files,
        human_bytes(prepared.source.bytes),
        prepared.replica.files,
        human_bytes(prepared.replica.bytes),
        prepared.elapsed_ms
    );
    let _ = writeln!(out, "\nplan as previewed:");
    out.push_str(&render_plan(&prepared.plan, approved));
    let _ = writeln!(out, "\nresults:");
    for r in &report.results {
        let line = match &r.outcome {
            Outcome::Applied => format!("applied  {}", r.path),
            Outcome::Skipped(why) => format!("skipped  {}: {}", r.path, skip_text(why)),
            Outcome::Failed(why) => format!("FAILED   {}: {}", r.path, fail_text(why)),
        };
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(
        out,
        "\napplied {}, skipped {}, failed {}",
        report.applied(),
        report.skipped(),
        report.failed()
    );
    if let Some(why) = &report.stopped {
        let _ = writeln!(out, "stopped: {}", stop_text(why));
    }
    if let Some(run) = &report.trash_run {
        let _ = writeln!(out, "trash run: {run}");
    }
    out
}

fn sanitise(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn write_log(dir: &Path, pair_name: &str, stamp: &str, text: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}-{stamp}.log", sanitise(pair_name)));
    fs::write(&path, text)?;
    Ok(path)
}

pub fn prune(dir: &Path, max_age: Duration, now: SystemTime) -> io::Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for e in entries {
        let e = e?;
        let old = e
            .metadata()?
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > max_age);
        if old && e.path().extension().is_some_and(|x| x == "log") {
            fs::remove_file(e.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::CaseMode;
    use crate::model::Change;
    use crate::plan::build_plan;
    use crate::testutil::rel;
    use std::fs;

    #[test]
    fn human_bytes_matches_explorer_style() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(6_396_000), "6.1 MB");
    }

    #[test]
    fn render_plan_marks_selection_and_skipped() {
        let plan = build_plan(
            vec![
                Change::Create {
                    path: rel("a.txt"),
                    size: 3,
                    mtime_ns: 0,
                },
                Change::Delete {
                    path: rel("old.txt"),
                    size: 1,
                    mtime_ns: 0,
                },
                Change::Move {
                    from: rel("Old"),
                    to: rel("New"),
                    kind: MoveKind::Dir {
                        files: 212,
                        bytes: 9,
                    },
                },
                Change::Skipped {
                    path: rel("lnk"),
                    reason: SkipReason::Link,
                },
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        let approved = plan
            .changes
            .iter()
            .filter(|p| p.change.path().as_str() != "old.txt")
            .map(|p| p.id)
            .collect();
        let text = render_plan(&plan, &approved);
        assert!(text.contains("[x] move     Old -> New (212 files)"));
        assert!(text.contains("[x] create   a.txt (3 B)"));
        assert!(text.contains("[ ] delete   old.txt"));
        assert!(text.contains(" -  skipped  lnk: link (not followed)"));
    }

    #[test]
    fn write_log_sanitises_name_and_prune_removes_old_logs() {
        let d = tempfile::tempdir().unwrap();
        let p = write_log(d.path(), "My Photos/x", "2026-10-07_120000", "hello").unwrap();
        assert_eq!(p.file_name().unwrap(), "My_Photos_x-2026-10-07_120000.log");
        let old = write_log(d.path(), "old", "2026-01-01_000000", "x").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(100 * 86_400);
        filetime::set_file_mtime(&old, filetime::FileTime::from_system_time(long_ago)).unwrap();
        assert_eq!(
            prune(d.path(), LOG_RETENTION, SystemTime::now()).unwrap(),
            1
        );
        assert!(p.exists() && !old.exists());
        assert_eq!(fs::read_to_string(p).unwrap(), "hello");
    }

    #[test]
    fn log_texts_keep_the_v010_english() {
        assert_eq!(skip_text(&SkipReason::FolderNotEmpty), "folder not empty");
        assert_eq!(
            skip_text(&SkipReason::Unreadable {
                side: SideKind::Source,
                detail: "x".into()
            }),
            "could not read on the source: x"
        );
        assert_eq!(fail_text(&FailReason::InUse), "in use by another program");
        assert_eq!(
            stop_text(&StopReason::ReplicaDisconnected),
            "the backup drive was disconnected"
        );
    }
}
