//! Plain-text plan and run logs, kept for 90 days.

use crate::execute::{Durability, Outcome, RunReport, StopReason};
use crate::model::{Change, MoveKind, SideKind};
use crate::plan::{ChangeId, Plan, Totals, selected_totals};
use crate::reason::{FailReason, SkipReason};
use crate::session::Prepared;
use crate::verify::SpotCheck;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
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
    // 1,048,575 B is 1023.999 KB, which rounds to "1024.0 KB": one unit up.
    if (v * 10.0).round() / 10.0 >= 1024.0 && i < UNITS.len() - 1 {
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
        SkipReason::ReservedName => {
            "uses a name replica-sync reserves for its temporary files".into()
        }
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
        FailReason::VerifyMismatch => {
            "the spot check read the copy back and it differs from the source".into()
        }
        FailReason::Io { detail } => detail.clone(),
    }
}

pub fn stop_text(r: &StopReason) -> String {
    match r {
        StopReason::Cancelled => "cancelled by the user".into(),
        StopReason::ReplicaDisconnected => "the backup drive was disconnected".into(),
        StopReason::SourceDisconnected => "the source drive was disconnected".into(),
        StopReason::ReplicaFull => "the backup drive is full".into(),
        StopReason::Internal => {
            "stopped by an internal error; scan again to see what is left".into()
        }
    }
}

/// A date as `2026-03-12` in the given zone.
fn day<Tz: chrono::TimeZone>(mtime_ns: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    chrono::DateTime::from_timestamp_nanos(mtime_ns)
        .with_timezone(tz)
        .format("%Y-%m-%d")
        .to_string()
}

/// One line for a change, with dates in the machine's local time zone.
pub fn describe(c: &Change) -> String {
    describe_in(c, &chrono::Local)
}

fn describe_in<Tz: chrono::TimeZone>(c: &Change, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match c {
        Change::Create { path, size, .. } => format!("create   {path} ({})", human_bytes(*size)),
        Change::Update {
            path,
            size,
            mtime_ns,
            replica_size,
            replica_mtime_ns,
            replica_newer,
        } => {
            format!(
                "update   {path} ({} -> {}, {} -> {}){}",
                day(*replica_mtime_ns, tz),
                day(*mtime_ns, tz),
                human_bytes(*replica_size),
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
        } => format!("move     {from} -> {to} ({files} files)"),
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
        "selected: {}; {} to copy; {} skipped",
        describe_totals(&t),
        human_bytes(t.bytes_to_copy),
        plan.totals.skipped
    );
    out
}

/// The change counts of a plan, as the plan log and the CLI summary print them.
pub fn describe_totals(t: &Totals) -> String {
    format!(
        "{} create, {} update, {} move, {} delete, {} folders",
        t.creates, t.updates, t.moves, t.deletes, t.folders
    )
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
    let _ = writeln!(out, "{}", durability_text(&report.durability));
    if let Some(c) = &report.spot_check {
        let _ = writeln!(out, "{}", spot_check_text(c));
    }
    out
}

/// e.g. `spot check: read back 50 copies, 1 differed from the source`.
pub fn spot_check_text(c: &SpotCheck) -> String {
    let mut line = format!(
        "spot check: read back {} copies, {} differed from the source",
        c.checked, c.mismatched
    );
    if c.cancelled {
        line.push_str(" (cancelled)");
    }
    line
}

/// How the run made copies durable, e.g. `durability: flushed in batches before renaming (btrfs)`.
pub fn durability_text(d: &Durability) -> String {
    let mut line = if d.batched {
        format!("durability: flushed in batches before renaming ({})", d.why)
    } else {
        format!("durability: one flush per file ({})", d.why)
    };
    if d.synced_separately > 0 {
        line += &format!(
            "; {} copies on another device (a mount or btrfs subvolume inside the replica) flushed one by one",
            d.synced_separately
        );
    }
    line
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

/// Writes a new log and returns its path. Never overwrites another log: two
/// runs in the same second, or two pair names `sanitise` maps to the same
/// text, get `-2`, `-3`, … before `.log` (as trash runs do).
pub fn write_log(dir: &Path, pair_name: &str, stamp: &str, text: &str) -> io::Result<PathBuf> {
    crate::private::create_dir_all(dir)?;
    let base = format!("{}-{stamp}", sanitise(pair_name));
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{base}.log")
        } else {
            format!("{base}-{n}.log")
        };
        let path = dir.join(name);
        match crate::private::create_new_file(&path) {
            Ok(mut f) => {
                f.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Removes the logs older than `max_age` and returns how many went. An entry
/// that can't be read or removed is left for the next run; the others still go.
pub fn prune(dir: &Path, max_age: Duration, now: SystemTime) -> io::Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let old = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > max_age);
        if old
            && e.path().extension().is_some_and(|x| x == "log")
            && fs::remove_file(e.path()).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CaseMode;
    use crate::model::Change;
    use crate::plan::build_plan;
    use crate::testutil::rel;
    use std::fs;

    #[test]
    fn human_bytes_matches_explorer_style() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(6_396_000), "6.1 MB");
        assert_eq!(human_bytes(1_048_575), "1.0 MB");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(u64::MAX), "16777216.0 TB");
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
    fn an_update_line_shows_the_replica_then_the_source_date_and_size() {
        let tz = chrono::FixedOffset::east_opt(0).unwrap();
        let day_ns = |d: &str| {
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_nanos_opt()
                .unwrap()
        };
        let mut c = Change::Update {
            path: rel("a.bin"),
            size: 93_184,
            mtime_ns: day_ns("2026-10-04"),
            replica_size: 90_112,
            replica_mtime_ns: day_ns("2026-03-12"),
            replica_newer: false,
        };
        assert_eq!(
            describe_in(&c, &tz),
            "update   a.bin (2026-03-12 -> 2026-10-04, 88.0 KB -> 91.0 KB)"
        );
        if let Change::Update { replica_newer, .. } = &mut c {
            *replica_newer = true;
        }
        assert_eq!(
            describe_in(&c, &tz),
            "update   a.bin (2026-03-12 -> 2026-10-04, 88.0 KB -> 91.0 KB) [replica is newer]"
        );
    }

    #[test]
    fn a_case_only_folder_rename_is_a_plain_move_line() {
        let c = Change::Move {
            from: rel("Photos"),
            to: rel("photos"),
            kind: MoveKind::DirCaseOnly,
        };
        assert_eq!(describe(&c), "move     Photos -> photos");
    }

    #[test]
    fn durability_line_names_the_mode_and_why() {
        let mut d = Durability {
            batched: true,
            why: "btrfs".into(),
            synced_separately: 0,
        };
        assert_eq!(
            durability_text(&d),
            "durability: flushed in batches before renaming (btrfs)"
        );
        d.synced_separately = 3;
        assert_eq!(
            durability_text(&d),
            "durability: flushed in batches before renaming (btrfs); 3 copies on another device (a mount or btrfs subvolume inside the replica) flushed one by one"
        );
        let d = Durability {
            batched: false,
            why: "exfat: syncfs may not reach the disk".into(),
            synced_separately: 0,
        };
        assert_eq!(
            durability_text(&d),
            "durability: one flush per file (exfat: syncfs may not reach the disk)"
        );
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
    fn prune_keeps_going_past_a_log_it_cannot_remove() {
        let d = tempfile::tempdir().unwrap();
        // A folder named like a log: `remove_file` fails on it.
        fs::create_dir(d.path().join("a-stuck.log")).unwrap();
        let old = write_log(d.path(), "old", "2026-01-01_000000", "x").unwrap();
        let later = SystemTime::now() + Duration::from_secs(100 * 86_400);
        assert_eq!(prune(d.path(), LOG_RETENTION, later).unwrap(), 1);
        assert!(!old.exists() && d.path().join("a-stuck.log").is_dir());
    }

    #[test]
    fn write_log_never_overwrites_another_log() {
        let d = tempfile::tempdir().unwrap();
        let a = write_log(d.path(), "Photos", "2026-10-07_120000", "first").unwrap();
        let b = write_log(d.path(), "Photos", "2026-10-07_120000", "second").unwrap();
        // Two names `sanitise` maps to the same text.
        let c = write_log(d.path(), "Fotos ä", "2026-10-07_120000", "third").unwrap();
        let e = write_log(d.path(), "Fotos ö", "2026-10-07_120000", "fourth").unwrap();
        assert_eq!(b.file_name().unwrap(), "Photos-2026-10-07_120000-2.log");
        assert_ne!(c, e);
        for (p, text) in [(a, "first"), (b, "second"), (c, "third"), (e, "fourth")] {
            assert_eq!(fs::read_to_string(p).unwrap(), text);
        }
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

    #[cfg(unix)]
    #[test]
    fn a_new_log_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let logs = d.path().join("logs");
        let path = write_log(&logs, "Photos", "2026-11-02_120000", "x").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&path), mode(&logs)), (0o600, 0o700));
    }

    #[test]
    fn describe_has_one_line_shape_per_change_kind() {
        let p = rel("a/b.txt");
        let lines = [
            (
                Change::Create {
                    path: p.clone(),
                    size: 1536,
                    mtime_ns: 0,
                },
                "create   a/b.txt (1.5 KB)",
            ),
            (
                Change::Update {
                    path: p.clone(),
                    size: 2,
                    mtime_ns: 0,
                    replica_newer: false,
                },
                "update   a/b.txt (2 B)",
            ),
            (
                Change::Update {
                    path: p.clone(),
                    size: 2,
                    mtime_ns: 0,
                    replica_newer: true,
                },
                "update   a/b.txt (2 B) [replica is newer]",
            ),
            (
                Change::Delete {
                    path: p.clone(),
                    size: 9,
                    mtime_ns: 0,
                },
                "delete   a/b.txt",
            ),
            (
                Change::Move {
                    from: rel("x.txt"),
                    to: p.clone(),
                    kind: MoveKind::File {
                        size: 1,
                        mtime_ns: 0,
                    },
                },
                "move     x.txt -> a/b.txt",
            ),
            (
                Change::Move {
                    from: rel("Old"),
                    to: rel("New"),
                    kind: MoveKind::Dir { files: 1, bytes: 5 },
                },
                "move     Old -> New (1 files)",
            ),
            (Change::MkDir { path: p.clone() }, "mkdir    a/b.txt"),
            (Change::RmDir { path: p.clone() }, "rmdir    a/b.txt"),
            (
                Change::Skipped {
                    path: p,
                    reason: SkipReason::FolderNotEmpty,
                },
                "skipped  a/b.txt: folder not empty",
            ),
        ];
        for (change, line) in lines {
            assert_eq!(describe(&change), line);
        }
    }

    fn prepared_with(changes: Vec<Change>) -> Prepared {
        Prepared {
            plan: build_plan(changes, CaseMode::Sensitive).unwrap(),
            source: crate::session::ScanStats {
                files: 3,
                bytes: 2048,
                elapsed_ms: 1,
            },
            replica: crate::session::ScanStats {
                files: 1,
                bytes: 10,
                elapsed_ms: 2,
            },
            elapsed_ms: 7,
        }
    }

    fn result(prepared: &Prepared, path: &str, outcome: Outcome) -> crate::execute::ChangeResult {
        let id = prepared
            .plan
            .changes
            .iter()
            .find(|p| p.change.path().as_str() == path)
            .unwrap()
            .id;
        crate::execute::ChangeResult {
            id,
            path: rel(path),
            outcome,
        }
    }

    #[test]
    fn render_run_lists_the_plan_each_result_and_the_totals() {
        let prepared = prepared_with(vec![
            Change::Create {
                path: rel("ok.txt"),
                size: 3,
                mtime_ns: 0,
            },
            Change::Create {
                path: rel("busy.txt"),
                size: 3,
                mtime_ns: 0,
            },
            Change::Delete {
                path: rel("gone.txt"),
                size: 1,
                mtime_ns: 0,
            },
        ]);
        let report = RunReport {
            results: vec![
                result(&prepared, "ok.txt", Outcome::Applied),
                result(&prepared, "busy.txt", Outcome::Failed(FailReason::InUse)),
                result(
                    &prepared,
                    "gone.txt",
                    Outcome::Skipped(SkipReason::AlreadyGone),
                ),
            ],
            stopped: None,
            trash_run: Some("2026-11-02_120000".into()),
            durability: Durability {
                batched: false,
                why: "test".into(),
                synced_separately: 0,
            },
            spot_check: Some(SpotCheck {
                checked: 5,
                mismatched: 1,
                cancelled: false,
            }),
        };
        let approved = prepared.plan.actionable_ids();
        let text = render_run("My Pair", &prepared, &approved, &report);
        let want = "pair: My Pair\n\
            scanned: source 3 files (2.0 KB), replica 1 files (10 B) in 7 ms\n\
            \n\
            plan as previewed:\n\
            [x] create   busy.txt (3 B)\n\
            [x] create   ok.txt (3 B)\n\
            [x] delete   gone.txt\n\
            selected: 2 create, 0 update, 0 move, 1 delete, 0 folders; 6 B to copy; 0 skipped\n\
            \n\
            results:\n\
            applied  ok.txt\n\
            FAILED   busy.txt: in use by another program\n\
            skipped  gone.txt: already gone\n\
            \n\
            applied 1, skipped 1, failed 1\n\
            trash run: 2026-11-02_120000\n\
            durability: one flush per file (test)\n\
            spot check: read back 5 copies, 1 differed from the source\n";
        assert_eq!(text, want);
    }

    #[test]
    fn render_run_of_a_stopped_run_says_why_and_omits_what_never_ran() {
        let prepared = prepared_with(vec![Change::Move {
            from: rel("Photos"),
            to: rel("photos"),
            kind: MoveKind::DirCaseOnly,
        }]);
        let report = RunReport {
            results: vec![],
            stopped: Some(StopReason::ReplicaDisconnected),
            trash_run: None,
            durability: Durability::default(),
            spot_check: None,
        };
        let text = render_run("p", &prepared, &prepared.plan.actionable_ids(), &report);
        assert!(text.contains("[x] move     Photos -> photos\n"), "{text}");
        assert!(
            text.contains(
                "\nresults:\n\napplied 0, skipped 0, failed 0\nstopped: the backup drive was disconnected\n"
            ),
            "{text}"
        );
        assert!(!text.contains("trash run:") && !text.contains("spot check:"));
        // An unapproved change is shown unticked and is not in the totals.
        let text = render_run("p", &prepared, &HashSet::new(), &report);
        assert!(text.contains("[ ] move     Photos -> photos\n"), "{text}");
        assert!(
            text.contains("selected: 0 create, 0 update, 0 move"),
            "{text}"
        );
    }

    #[test]
    fn spot_check_text_marks_a_cancelled_check() {
        let c = SpotCheck {
            checked: 2,
            mismatched: 0,
            cancelled: true,
        };
        assert_eq!(
            spot_check_text(&c),
            "spot check: read back 2 copies, 0 differed from the source (cancelled)"
        );
    }
}
