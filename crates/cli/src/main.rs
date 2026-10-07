//! Command-line harness for the replica-sync engine (manual testing and benchmarks).

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use replica_sync_core::execute::{Control, ExecContext, execute};
use replica_sync_core::rules::SkipRules;
use replica_sync_core::runlog::{human_bytes, render_plan, render_run};
use replica_sync_core::safety::ReplicaRoot;
use replica_sync_core::session::{Prepared, SessionCounters, check_roots, prepare};
use replica_sync_core::{trash, volume};
use std::fs;
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
        "source {} files ({}) · replica {} files ({}) · scan+diff {:.1} s",
        p.source.files,
        human_bytes(p.source.bytes),
        p.replica.files,
        human_bytes(p.replica.bytes),
        p.elapsed_ms as f64 / 1000.0
    );
    println!(
        "plan: {} create, {} update, {} move, {} delete, {} folders, {} skipped · {} to copy",
        t.creates,
        t.updates,
        t.moves,
        t.deletes,
        t.folders,
        t.skipped,
        human_bytes(t.bytes_to_copy)
    );
}

fn gen_tree(dir: &Path, files: u64, large: u32) -> Result<()> {
    const T0: i64 = 1_700_000_000;
    for i in 0..files {
        let folder = dir.join(format!("d{:04}", i / 1000));
        if i % 1000 == 0 {
            fs::create_dir_all(&folder)?;
        }
        let path = folder.join(format!("f{i:07}.txt"));
        fs::write(&path, i.to_string())?;
        filetime::set_file_mtime(
            &path,
            filetime::FileTime::from_unix_time(T0 + (i % 1000) as i64, 0),
        )?;
    }
    let big = vec![0u8; 64 * 1024 * 1024];
    for k in 0..large {
        let path = dir.join(format!("large-{k}.bin"));
        fs::write(&path, &big)?;
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(T0, 0))?;
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
        } => {
            let p = prepared(&source, &replica, &skip)?;
            if !yes {
                print!("{}", render_plan(&p.plan, &p.plan.actionable_ids()));
                eprintln!("nothing applied: pass --yes to apply these changes");
                return Ok(ExitCode::from(2));
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
            };
            let report = execute(&p.plan, &approved, &ctx, &mut |_| {});
            print!("{}", render_run("cli", &p, &approved, &report));
            if report.failed() > 0 || report.stopped.is_some() {
                return Ok(ExitCode::from(1));
            }
        }
        Cmd::Volume { path } => {
            use replica_sync_core::volume::Volumes;
            let v = volume::system().volume_of(&path)?;
            println!(
                "id: {}\nmount root: {}\nlabel: {}",
                v.id,
                v.mount_root.display(),
                v.label
            );
            #[cfg(windows)]
            println!("method: {}", volume::windows::id_method(&path)?);
        }
        Cmd::GenTree { dir, files, large } => {
            if dir.exists() && fs::read_dir(&dir)?.next().is_some() {
                bail!("{} is not empty", dir.display());
            }
            gen_tree(&dir, files, large)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
