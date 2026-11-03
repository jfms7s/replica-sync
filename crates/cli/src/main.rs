//! Command-line harness for the replica-sync engine (manual testing and benchmarks).

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use replica_sync_core::execute::{Control, ExecContext, execute};
use replica_sync_core::plan::{mass_change_guard, space_shortfall};
use replica_sync_core::rules::SkipRules;
use replica_sync_core::runlog::{describe_totals, human_bytes, render_plan, render_run};
use replica_sync_core::safety::ReplicaRoot;
use replica_sync_core::session::{Prepared, SessionCounters, check_roots, prepare};
use replica_sync_core::{trash, volume};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "replica-sync-cli",
    about = "Test harness for the replica-sync engine"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan both folders and print the plan.
    Plan {
        source: PathBuf,
        replica: PathBuf,
        #[arg(long = "skip")]
        skip: Vec<String>,
        /// Print only totals and timing (for large trees).
        #[arg(long)]
        summary: bool,
    },
    /// Apply every change in the plan.
    Apply {
        source: PathBuf,
        replica: PathBuf,
        #[arg(long = "skip")]
        skip: Vec<String>,
        #[arg(long)]
        yes: bool,
        /// Apply even when most of the replica would be removed or replaced,
        /// or the replica drive looks too small.
        #[arg(long)]
        force: bool,
    },
    /// Show the drive id the app would store for a folder.
    Volume { path: PathBuf },
    /// Write a deterministic test tree.
    GenTree {
        dir: PathBuf,
        #[arg(long, default_value_t = 1_000_000)]
        files: u64,
        #[arg(long, default_value_t = 3)]
        large: u32,
    },
}

fn prepared(source: &Path, replica: &Path, skip: &[String]) -> Result<Prepared> {
    let rules = SkipRules::new(skip)?;
    // Before the case probe, which writes a file into the replica.
    check_roots(source, replica)?;
    let case = volume::case_mode(replica).context("probing the replica drive")?;
    Ok(prepare(
        source,
        replica,
        &rules,
        case,
        &SessionCounters::default(),
    )?)
}

fn print_summary(p: &Prepared) {
    let t = &p.plan.totals;
    println!(
        "source {} files ({}) · replica {} files ({}) · scan source {} ms · scan replica {} ms · scan+diff {:.1} s",
        p.source.files,
        human_bytes(p.source.bytes),
        p.replica.files,
        human_bytes(p.replica.bytes),
        p.source.elapsed_ms,
        p.replica.elapsed_ms,
        p.elapsed_ms as f64 / 1000.0
    );
    println!(
        "plan: {}, {} skipped · {} to copy",
        describe_totals(t),
        t.skipped,
        human_bytes(t.bytes_to_copy)
    );
}

/// The app's apply checks, minus the drive check (the CLI has no saved
/// pairs, so every run counts as a first sync). Returns why to refuse.
fn refusals(p: &Prepared, replica: &Path) -> Vec<String> {
    let approved = p.plan.actionable_ids();
    let mut why = Vec::new();
    if let Some(g) = mass_change_guard(&p.plan, &approved, p.replica.files, true) {
        why.push(format!(
            "{} of the replica's {} files would be removed or replaced: is this the right pair of folders?",
            g.affected, g.replica_files
        ));
    }
    match volume::free_space(replica) {
        Ok(free) => {
            if let Some(short) = space_shortfall(p.plan.totals.bytes_to_copy, free) {
                why.push(format!(
                    "not enough space on the replica drive: {} more needed",
                    human_bytes(short)
                ));
            }
        }
        Err(e) => why.push(format!(
            "could not read the free space on {}: {e}",
            replica.display()
        )),
    }
    why
}

/// Writes `path` with `content` and modified time `secs`, through one handle.
fn write_with_mtime(path: &Path, content: &[u8], secs: i64) -> Result<()> {
    let mut f = fs::File::create(path).with_context(|| format!("writing {}", path.display()))?;
    f.write_all(content)
        .with_context(|| format!("writing {}", path.display()))?;
    let t = filetime::FileTime::from_unix_time(secs, 0);
    filetime::set_file_handle_times(&f, None, Some(t))
        .with_context(|| format!("setting the time of {}", path.display()))
}

/// `files` small files, 1,000 per folder `dNNNN`, written one folder per
/// thread; then `large` 64 MiB files. The same tree every time.
fn gen_tree(dir: &Path, files: u64, large: u32) -> Result<()> {
    const T0: i64 = 1_700_000_000;
    const PER_FOLDER: u64 = 1000;
    (0..files.div_ceil(PER_FOLDER))
        .into_par_iter()
        .try_for_each(|d| -> Result<()> {
            let folder = dir.join(format!("d{d:04}"));
            fs::create_dir_all(&folder)
                .with_context(|| format!("creating {}", folder.display()))?;
            for i in d * PER_FOLDER..((d + 1) * PER_FOLDER).min(files) {
                let path = folder.join(format!("f{i:07}.txt"));
                write_with_mtime(
                    &path,
                    i.to_string().as_bytes(),
                    T0 + (i % PER_FOLDER) as i64,
                )?;
            }
            Ok(())
        })?;
    if large == 0 {
        return Ok(());
    }
    let big = vec![0u8; 64 * 1024 * 1024];
    for k in 0..large {
        write_with_mtime(&dir.join(format!("large-{k}.bin")), &big, T0)?;
    }
    Ok(())
}

fn main() -> Result<ExitCode> {
    match Cli::parse().cmd {
        Cmd::Plan {
            source,
            replica,
            skip,
            summary,
        } => {
            let p = prepared(&source, &replica, &skip)?;
            if !summary {
                print!("{}", render_plan(&p.plan, &p.plan.actionable_ids()));
            }
            print_summary(&p);
        }
        Cmd::Apply {
            source,
            replica,
            skip,
            yes,
            force,
        } => {
            let p = prepared(&source, &replica, &skip)?;
            if !yes {
                print!("{}", render_plan(&p.plan, &p.plan.actionable_ids()));
                eprintln!("nothing applied: pass --yes to apply these changes");
                return Ok(ExitCode::from(2));
            }
            let why = refusals(&p, &replica);
            if !why.is_empty() && !force {
                for w in &why {
                    eprintln!("{w}");
                }
                eprintln!("nothing applied: pass --force to apply anyway");
                return Ok(ExitCode::from(3));
            }
            let approved = p.plan.actionable_ids();
            let replica_root = ReplicaRoot::new(&replica)
                .with_context(|| format!("opening replica {}", replica.display()))?;
            let control = Control::default();
            let ctx = ExecContext {
                source_root: &source,
                replica: &replica_root,
                control: &control,
                trash_stamp: trash::new_run_stamp(),
                drive_check: None,
            };
            let report = execute(&p.plan, &approved, &ctx, &mut |_| {});
            print!("{}", render_run("cli", &p, &approved, &report));
            if report.failed() > 0 || report.stopped.is_some() {
                return Ok(ExitCode::from(1));
            }
        }
        Cmd::Volume { path } => {
            use replica_sync_core::volume::Volumes;
            let v = volume::system()
                .volume_of(&path)
                .with_context(|| format!("reading the drive of {}", path.display()))?;
            println!(
                "id: {}\nmount root: {}\nlabel: {}",
                v.id,
                v.mount_root.display(),
                v.label
            );
            #[cfg(windows)]
            println!(
                "method: {}",
                volume::windows::id_method(&path)
                    .with_context(|| format!("reading the drive id of {}", path.display()))?
            );
        }
        Cmd::GenTree { dir, files, large } => {
            let not_empty = dir.exists()
                && fs::read_dir(&dir)
                    .with_context(|| format!("reading {}", dir.display()))?
                    .next()
                    .is_some();
            if not_empty {
                bail!("{} is not empty", dir.display());
            }
            gen_tree(&dir, files, large)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
