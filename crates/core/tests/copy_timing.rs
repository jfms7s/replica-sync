//! Timing check for copying many small files (`cargo test --test copy_timing -- --ignored`).
//!
//! Ignored by default: it needs a real disk, where a flush costs what it does on
//! a user's drive (tmpfs makes every flush free). It runs in Cargo's target temp
//! folder, or in `REPLICA_SYNC_BENCH_DIR` when set; never point that at real data.

mod common;

use common::*;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

const FILES: usize = 2000;

fn bench_dir() -> PathBuf {
    std::env::var_os("REPLICA_SYNC_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")))
}

#[test]
#[ignore = "timing: run on a real disk with --ignored"]
fn small_files_copy_at_least_5x_faster_than_one_flush_per_file() {
    let base = bench_dir();
    fs::create_dir_all(&base).unwrap();
    let d = tempfile::tempdir_in(&base).unwrap();
    let (src, rep, serial) = (
        d.path().join("src"),
        d.path().join("rep"),
        d.path().join("serial"),
    );
    for i in 0..FILES {
        write_file(
            &src,
            &format!("d{:02}/f{i:05}.txt", i / 500),
            i.to_string().as_bytes(),
            T0,
        );
    }
    fs::create_dir_all(&rep).unwrap();

    // What `execute` did before batching: temp, flush, rename, one file at a time.
    let t = Instant::now();
    for (rel, bytes) in files(&src, false) {
        let dest = serial.join(&rel);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let tmp = dest.with_extension("txt.replica-sync.tmp");
        let mut out = File::create_new(&tmp).unwrap();
        out.write_all(&bytes).unwrap();
        out.sync_all().unwrap();
        drop(out);
        fs::rename(&tmp, &dest).unwrap();
    }
    let per_file = t.elapsed();

    let t = Instant::now();
    let (_, report) = sync_all(&src, &rep);
    let batched = t.elapsed();

    assert_eq!(report.applied(), FILES, "{:?}", report.stopped);
    assert_eq!(files(&rep, true), files(&src, false));
    let ratio = per_file.as_secs_f64() / batched.as_secs_f64();
    eprintln!(
        "{FILES} small files in {}: one flush per file {per_file:.2?}, batched apply {batched:.2?} ({ratio:.1}x)",
        base.display()
    );
    if cfg!(target_os = "linux") {
        assert!(ratio >= 5.0, "only {ratio:.1}x faster");
    }
}
