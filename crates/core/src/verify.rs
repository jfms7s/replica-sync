//! The post-apply spot check: read back a bounded sample of the files a run
//! just copied and compare their bytes with the source.
//!
//! The diff calls two files equal when size and modification time match, so a
//! copy with the right size and date but the wrong bytes (a flaky cable or USB
//! bridge) would look unchanged forever. This catches some of those. It is a
//! sample, so it is called a spot check, never "verified": on Linux the copy's
//! cached pages are dropped before reading it, elsewhere the read may still be
//! served from the page cache.

use crate::execute::{ChangeResult, Control, ExecContext, Outcome, RunReport};
use crate::model::{Change, RelPath, modified_ns};
use crate::plan::Plan;
use crate::reason::FailReason;
use serde::Serialize;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

/// How much a spot check reads at most.
#[derive(Clone, Copy, Debug)]
pub struct SpotCheckLimits {
    pub max_files: usize,
    pub max_bytes: u64,
}

impl Default for SpotCheckLimits {
    /// Up to 50 files or 1 GiB, whichever comes first.
    fn default() -> SpotCheckLimits {
        SpotCheckLimits {
            max_files: 50,
            max_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// What a spot check found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotCheck {
    /// Copies read back and compared to the end.
    pub checked: usize,
    /// Of those, copies whose bytes differ from the source; each is now
    /// `Failed(VerifyMismatch)` in the report.
    pub mismatched: usize,
    /// The user cancelled during the check: the rest were not read.
    pub cancelled: bool,
}

/// Progress of the check: copies compared so far, out of the sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckProgress {
    pub done: usize,
    pub total: usize,
}

const READ_CHUNK: usize = 1024 * 1024;

/// One copy chosen for the check.
struct Candidate {
    index: usize,
    size: u64,
    mtime_ns: i64,
}

/// Reads back a sample of the copies `report` lists as applied and rewrites
/// the outcome of every one whose bytes differ from the source to
/// `Failed(VerifyMismatch)`, so "Retry failed" copies it again. A copy whose
/// source changed since, or that cannot be read, is left out rather than
/// counted. Honours pause and cancel; outcomes already decided stay.
pub fn spot_check(
    plan: &Plan,
    report: &mut RunReport,
    ctx: &ExecContext,
    limits: SpotCheckLimits,
    on_progress: &mut dyn FnMut(CheckProgress),
) -> SpotCheck {
    let sample = choose(plan, &report.results, limits);
    let mut found = SpotCheck::default();
    let mut progress = CheckProgress {
        done: 0,
        total: sample.len(),
    };
    on_progress(progress);
    let mut bufs = (vec![0u8; READ_CHUNK], vec![0u8; READ_CHUNK]);
    for c in sample {
        if !ctx.control.proceed() {
            found.cancelled = true;
            break;
        }
        let path = report.results[c.index].path.clone();
        match compare(ctx, &path, c.size, c.mtime_ns, &mut bufs) {
            Compared::Same => found.checked += 1,
            Compared::Differs => {
                found.checked += 1;
                found.mismatched += 1;
                report.results[c.index].outcome = Outcome::Failed(FailReason::VerifyMismatch);
            }
            Compared::NotChecked => {}
            Compared::Cancelled => {
                found.cancelled = true;
                break;
            }
        }
        progress.done += 1;
        on_progress(progress);
    }
    found
}

/// The sample, in reading order: mostly the run's first copies (the ones
/// least likely to still be in the page cache), plus a few spread over the
/// rest of the run, within both caps.
fn choose(plan: &Plan, results: &[ChangeResult], limits: SpotCheckLimits) -> Vec<Candidate> {
    let copies: Vec<Candidate> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| r.outcome == Outcome::Applied)
        .filter_map(
            |(index, r)| match &plan.changes.get(r.id.0 as usize)?.change {
                Change::Create { size, mtime_ns, .. } | Change::Update { size, mtime_ns, .. } => {
                    Some(Candidate {
                        index,
                        size: *size,
                        mtime_ns: *mtime_ns,
                    })
                }
                _ => None,
            },
        )
        .collect();
    let spread = limits.max_files / 5;
    let first = limits.max_files - spread;
    let mut order: Vec<usize> = (0..copies.len().min(first)).collect();
    let rest = copies.len().saturating_sub(first);
    if rest > 0 && spread > 0 {
        let step = (rest / spread).max(1);
        order.extend((0..rest).step_by(step).take(spread).map(|i| first + i));
    }
    // Then anything else, so small runs or a byte cap hit early still fill the sample.
    let picked: std::collections::HashSet<usize> = order.iter().copied().collect();
    order.extend((0..copies.len()).filter(|i| !picked.contains(i)));
    let mut bytes = 0u64;
    let mut sample = Vec::new();
    for i in order {
        if sample.len() == limits.max_files {
            break;
        }
        let c = &copies[i];
        if bytes + c.size > limits.max_bytes {
            continue;
        }
        bytes += c.size;
        sample.push(Candidate {
            index: c.index,
            size: c.size,
            mtime_ns: c.mtime_ns,
        });
    }
    sample.sort_by_key(|c| c.index);
    sample
}

enum Compared {
    Same,
    Differs,
    NotChecked,
    Cancelled,
}

fn compare(
    ctx: &ExecContext,
    rel: &RelPath,
    size: u64,
    mtime_ns: i64,
    (a, b): &mut (Vec<u8>, Vec<u8>),
) -> Compared {
    let src_path = rel.to_path(ctx.source_root);
    // A source that changed since the copy proves nothing about the copy.
    match fs::symlink_metadata(&src_path) {
        Ok(m) if m.is_file() && m.len() == size && modified_ns(&m) == mtime_ns => {}
        _ => return Compared::NotChecked,
    }
    let Ok(target) = ctx.replica.target(rel) else {
        return Compared::NotChecked;
    };
    let (Ok(mut src), Ok(mut copy)) = (open_read(&src_path), open_read(&target)) else {
        return Compared::NotChecked;
    };
    match copy.metadata() {
        Ok(m) if !m.is_file() => return Compared::NotChecked,
        Ok(m) if m.len() != size => return Compared::Differs,
        Ok(_) => {}
        Err(_) => return Compared::NotChecked,
    }
    drop_cached_pages(&copy);
    match same_bytes(&mut src, &mut copy, a, b, ctx.control) {
        Ok(Some(true)) => Compared::Same,
        Ok(Some(false)) => Compared::Differs,
        Ok(None) => Compared::Cancelled,
        Err(_) => Compared::NotChecked,
    }
}

/// `None` when cancelled part-way.
fn same_bytes(
    src: &mut File,
    copy: &mut File,
    a: &mut [u8],
    b: &mut [u8],
    control: &Control,
) -> io::Result<Option<bool>> {
    loop {
        if !control.proceed() {
            return Ok(None);
        }
        let n = read_full(src, a)?;
        let m = read_full(copy, b)?;
        if n != m || a[..n] != b[..m] {
            return Ok(Some(false));
        }
        if n == 0 {
            return Ok(Some(true));
        }
    }
}

/// Fills `buf` unless the file ends first; returns how much it read.
fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Opens without following a final link (the scan never copies links).
fn open_read(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::other("link"));
        }
        File::open(path)
    }
}

/// Asks Linux to drop the copy's cached pages so the read comes from the
/// drive. The copy was flushed before it was renamed into place, so its
/// pages are clean and can be dropped. Best effort: the result is ignored.
#[cfg(target_os = "linux")]
fn drop_cached_pages(f: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: a valid open descriptor; the call only gives advice.
    let _ = unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
}

#[cfg(not(target_os = "linux"))]
fn drop_cached_pages(_: &File) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute::{Progress, execute};
    use crate::plan::ChangeId;
    use crate::safety::ReplicaRoot;
    use crate::testutil::{T0, plan_for, write_file};
    use std::collections::HashSet;
    use std::path::PathBuf;

    struct Fx {
        _dir: tempfile::TempDir,
        src: PathBuf,
        rep: PathBuf,
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let (src, rep) = (dir.path().join("src"), dir.path().join("rep"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&rep).unwrap();
        Fx {
            _dir: dir,
            src,
            rep,
        }
    }

    /// Plans and applies everything; returns the plan and the report.
    fn apply(f: &Fx, control: &Control) -> (Plan, RunReport) {
        let plan = plan_for(&f.src, &f.rep);
        let replica = ReplicaRoot::new(&f.rep).unwrap();
        let ctx = ExecContext {
            source_root: &f.src,
            replica: &replica,
            control,
            trash_stamp: "2026-10-07_120000".into(),
            drive_check: None,
        };
        let ids: HashSet<ChangeId> = plan.actionable_ids();
        let report = execute(&plan, &ids, &ctx, &mut |_: &Progress| {});
        (plan, report)
    }

    fn check(
        f: &Fx,
        plan: &Plan,
        report: &mut RunReport,
        control: &Control,
        limits: SpotCheckLimits,
        on_progress: &mut dyn FnMut(CheckProgress),
    ) -> SpotCheck {
        let replica = ReplicaRoot::new(&f.rep).unwrap();
        let ctx = ExecContext {
            source_root: &f.src,
            replica: &replica,
            control,
            trash_stamp: "2026-10-07_120000".into(),
            drive_check: None,
        };
        spot_check(plan, report, &ctx, limits, on_progress)
    }

    /// Overwrites the copy's bytes, keeping its size and modification time.
    fn corrupt(path: &Path) {
        let m = fs::metadata(path).unwrap();
        let mut bytes = fs::read(path).unwrap();
        bytes[0] ^= 0xff;
        fs::write(path, &bytes).unwrap();
        let t = filetime::FileTime::from_last_modification_time(&m);
        filetime::set_file_mtime(path, t).unwrap();
    }

    #[test]
    fn a_run_reports_its_spot_check_and_a_clean_run_changes_nothing() {
        let f = fx();
        for i in 0..60 {
            write_file(&f.src, &format!("f{i:02}.txt"), b"hello", T0);
        }
        let control = Control::default();
        let (_, report) = apply(&f, &control);
        assert_eq!(report.applied(), 60);
        assert_eq!(
            report.spot_check,
            Some(SpotCheck {
                checked: 50,
                mismatched: 0,
                cancelled: false
            })
        );
    }

    #[test]
    fn a_copy_with_the_wrong_bytes_becomes_a_failed_change() {
        let f = fx();
        write_file(&f.src, "a.txt", b"alpha", T0);
        write_file(&f.src, "b.txt", b"bravo", T0);
        let control = Control::default();
        let (plan, mut report) = apply(&f, &control);
        corrupt(&f.rep.join("b.txt"));
        let found = check(
            &f,
            &plan,
            &mut report,
            &control,
            SpotCheckLimits::default(),
            &mut |_| {},
        );
        assert_eq!((found.checked, found.mismatched), (2, 1));
        let b = report
            .results
            .iter()
            .find(|r| r.path.as_str() == "b.txt")
            .unwrap();
        assert_eq!(b.outcome, Outcome::Failed(FailReason::VerifyMismatch));
        assert!(report.failed_ids().contains(&b.id));
        assert_eq!(report.failed(), 1);

        // "Retry failed" copies it again (the bad copy goes to the trash).
        let replica = ReplicaRoot::new(&f.rep).unwrap();
        let ctx = ExecContext {
            source_root: &f.src,
            replica: &replica,
            control: &control,
            trash_stamp: "2026-10-07_120001".into(),
            drive_check: None,
        };
        let retry = execute(&plan, &report.failed_ids(), &ctx, &mut |_| {});
        assert_eq!((retry.applied(), retry.failed()), (1, 0), "{retry:?}");
        assert_eq!(fs::read(f.rep.join("b.txt")).unwrap(), b"bravo");
        assert_eq!(retry.spot_check.map(|s| s.mismatched), Some(0));
    }

    #[test]
    fn a_copy_of_the_wrong_length_is_a_mismatch() {
        let f = fx();
        write_file(&f.src, "a.txt", b"alpha", T0);
        let control = Control::default();
        let (plan, mut report) = apply(&f, &control);
        fs::write(f.rep.join("a.txt"), b"alph").unwrap();
        let found = check(
            &f,
            &plan,
            &mut report,
            &control,
            SpotCheckLimits::default(),
            &mut |_| {},
        );
        assert_eq!(found.mismatched, 1);
    }

    #[test]
    fn a_source_changed_since_the_copy_is_left_out() {
        let f = fx();
        write_file(&f.src, "a.txt", b"alpha", T0);
        let control = Control::default();
        let (plan, mut report) = apply(&f, &control);
        write_file(&f.src, "a.txt", b"other", T0 + 60);
        let found = check(
            &f,
            &plan,
            &mut report,
            &control,
            SpotCheckLimits::default(),
            &mut |_| {},
        );
        assert_eq!(found, SpotCheck::default());
        assert_eq!(report.failed(), 0);
    }

    #[test]
    fn a_run_without_copies_checks_nothing() {
        let f = fx();
        fs::create_dir(f.src.join("dir")).unwrap();
        write_file(&f.rep, "gone.txt", b"x", T0);
        let control = Control::default();
        let (_, report) = apply(&f, &control);
        assert_eq!(report.applied(), 2);
        assert_eq!(report.spot_check, Some(SpotCheck::default()));
    }

    #[test]
    fn the_byte_cap_and_file_cap_bound_the_sample() {
        let f = fx();
        for i in 0..10 {
            write_file(&f.src, &format!("f{i}.bin"), &[7u8; 100], T0);
        }
        let control = Control::default();
        let (plan, mut report) = apply(&f, &control);
        let bytes_capped = SpotCheckLimits {
            max_files: 50,
            max_bytes: 350,
        };
        let found = check(&f, &plan, &mut report, &control, bytes_capped, &mut |_| {});
        assert_eq!(found.checked, 3);
        let files_capped = SpotCheckLimits {
            max_files: 4,
            max_bytes: u64::MAX,
        };
        let found = check(&f, &plan, &mut report, &control, files_capped, &mut |_| {});
        assert_eq!(found.checked, 4);
    }

    #[test]
    fn the_sample_takes_the_first_copies_and_some_from_later_in_the_run() {
        let f = fx();
        for i in 0..200 {
            write_file(&f.src, &format!("f{i:03}.txt"), b"x", T0);
        }
        let control = Control::default();
        let (plan, report) = apply(&f, &control);
        let sample = choose(&plan, &report.results, SpotCheckLimits::default());
        assert_eq!(sample.len(), 50);
        let paths: Vec<&str> = sample
            .iter()
            .map(|c| report.results[c.index].path.as_str())
            .collect();
        assert_eq!(paths[0], "f000.txt");
        assert_eq!(paths[39], "f039.txt");
        assert!(paths[40..].iter().all(|p| *p >= "f040.txt"));
        assert!(paths.last().unwrap() > &"f150.txt", "{paths:?}");
    }

    #[test]
    fn cancelling_during_the_check_stops_it_and_keeps_decided_outcomes() {
        let f = fx();
        for i in 0..5 {
            write_file(&f.src, &format!("f{i}.txt"), b"hello", T0);
        }
        let control = Control::default();
        let (plan, mut report) = apply(&f, &control);
        corrupt(&f.rep.join("f0.txt"));
        corrupt(&f.rep.join("f4.txt"));
        let found = check(
            &f,
            &plan,
            &mut report,
            &control,
            SpotCheckLimits::default(),
            &mut |p| {
                if p.done == 2 {
                    control.cancel();
                }
            },
        );
        assert_eq!(
            found,
            SpotCheck {
                checked: 2,
                mismatched: 1,
                cancelled: true
            }
        );
        let outcome = |name: &str| {
            report
                .results
                .iter()
                .find(|r| r.path.as_str() == name)
                .unwrap()
                .outcome
                .clone()
        };
        assert_eq!(
            outcome("f0.txt"),
            Outcome::Failed(FailReason::VerifyMismatch)
        );
        // Not reached: stays applied although its copy is bad.
        assert_eq!(outcome("f4.txt"), Outcome::Applied);
    }
}
