//! Thin `#[tauri::command]` wrappers over `app_state::session::Session`.

use crate::AppState;
use app_state::error::AppError;
use app_state::progress::{JobDone, ScanProgressView, Throttle};
use app_state::session::{ApplyJob, PairInput, PairView, PreviewSummary, Session};
use app_state::settings::{self, Settings};
use app_state::tree::NodeView;
use replica_sync_core::execute::{Progress, RunReport};
use replica_sync_core::model::{RelPath, SideKind};
use replica_sync_core::pairs::Pair;
use replica_sync_core::reason::StopReason;
use replica_sync_core::trash::{TrashRunContents, TrashRunInfo};
use serde::Serialize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

type Res<T> = Result<T, AppError>;

fn with_session<T>(state: &AppState, f: impl FnOnce(&mut Session) -> Res<T>) -> Res<T> {
    let mut guard = state.session.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_mut() {
        Some(s) => f(s),
        None => Err(state
            .open_error
            .clone()
            .unwrap_or_else(|| AppError::new("store.unreadable"))),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    settings: Settings,
    resolved_language: &'static str,
}

fn settings_view(s: &Settings) -> SettingsView {
    SettingsView {
        settings: s.clone(),
        resolved_language: s.resolved_language(settings::os_locale().as_deref()),
    }
}

#[tauri::command]
pub fn startup_status(state: State<'_, AppState>) -> Res<()> {
    with_session(&state, |_| Ok(()))
}

#[tauri::command]
pub fn data_folder(state: State<'_, AppState>) -> String {
    state.data_dir.display().to_string()
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Res<SettingsView> {
    // Settings are readable even when the pairs store is damaged (the
    // blocking screen still needs a language).
    match state
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        Some(s) => Ok(settings_view(s.settings())),
        None => Ok(settings_view(&Settings::load(
            &state.data_dir.join("settings.json"),
        ))),
    }
}

#[tauri::command]
pub fn set_settings(state: State<'_, AppState>, settings: Settings) -> Res<SettingsView> {
    with_session(&state, |s| {
        s.set_settings(settings)?;
        Ok(settings_view(s.settings()))
    })
}

#[tauri::command]
pub fn builtin_rules() -> Vec<&'static str> {
    Session::builtin_rules()
}

#[tauri::command]
pub fn list_pairs(state: State<'_, AppState>) -> Res<Vec<PairView>> {
    with_session(&state, |s| Ok(s.pair_views(&state.volumes)))
}

#[tauri::command]
pub fn save_pair(state: State<'_, AppState>, input: PairInput) -> Res<Pair> {
    with_session(&state, |s| s.save_pair(input, &state.volumes))
}

#[tauri::command]
pub fn delete_pair(state: State<'_, AppState>, id: String) -> Res<()> {
    with_session(&state, |s| s.delete_pair(&id))
}

#[tauri::command]
pub fn relink(
    state: State<'_, AppState>,
    id: String,
    side: SideKind,
    folder: PathBuf,
) -> Res<Pair> {
    with_session(&state, |s| s.relink(&id, side, &folder, &state.volumes))
}

#[tauri::command]
pub fn start_scan(app: AppHandle, state: State<'_, AppState>, id: String) -> Res<()> {
    let job = with_session(&state, |s| s.begin_scan(&id, &state.volumes))?;
    let done = Arc::new(AtomicBool::new(false));
    {
        let (app, counters, done, approx) = (
            app.clone(),
            job.counters.clone(),
            done.clone(),
            job.approx_files,
        );
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let _ = app.emit("scan-progress", ScanProgressView::read(&counters, approx));
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }
    std::thread::spawn(move || {
        // A panic must still reach `finish_scan`, or the session stays busy.
        let result = catch_unwind(AssertUnwindSafe(|| job.run())).unwrap_or_else(|_| {
            Err(AppError::new("io").with("detail", "the scan stopped unexpectedly"))
        });
        done.store(true, Ordering::Relaxed);
        let state = app.state::<AppState>();
        let out = with_session(&state, |s| s.finish_scan(job, result, &state.volumes));
        let _ = app.emit("scan-done", JobDone::from(out));
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_scan(state: State<'_, AppState>) -> Res<()> {
    with_session(&state, |s| {
        s.cancel_scan();
        Ok(())
    })
}

#[tauri::command]
pub fn preview_summary(state: State<'_, AppState>) -> Res<PreviewSummary> {
    with_session(&state, |s| s.summary())
}

fn rel(path: &str) -> Res<RelPath> {
    if path.is_empty() {
        return Ok(RelPath::root());
    }
    RelPath::new(path).map_err(|e| AppError::new("io").with("detail", e))
}

#[tauri::command]
pub fn tree_children(state: State<'_, AppState>, path: String) -> Res<Vec<NodeView>> {
    let folder = rel(&path)?;
    with_session(&state, |s| s.children(&folder))
}

#[tauri::command]
pub fn toggle(state: State<'_, AppState>, path: String) -> Res<PreviewSummary> {
    let p = rel(&path)?;
    with_session(&state, |s| s.toggle(&p))
}

#[tauri::command]
pub fn select_all(state: State<'_, AppState>, on: bool) -> Res<PreviewSummary> {
    with_session(&state, |s| s.select_all(on))
}

#[tauri::command]
pub fn confirm_wrong_folder(state: State<'_, AppState>) -> Res<PreviewSummary> {
    with_session(&state, |s| s.confirm_guard())
}

#[tauri::command]
pub fn save_preview(state: State<'_, AppState>, file: PathBuf) -> Res<()> {
    with_session(&state, |s| s.save_preview(&file))
}

fn spawn_apply(app: AppHandle, job: ApplyJob) {
    std::thread::spawn(move || {
        let mut throttle = Throttle::new(Duration::from_millis(100));
        // A panic must still reach `finish_apply`, or the session stays busy.
        let report = catch_unwind(AssertUnwindSafe(|| {
            job.run(&mut |p: &Progress| {
                if throttle.ready() || p.changes_done == p.changes_total {
                    let _ = app.emit("apply-progress", p);
                }
            })
        }))
        .unwrap_or_else(|_| RunReport {
            stopped: Some(StopReason::Cancelled),
            ..RunReport::default()
        });
        let state = app.state::<AppState>();
        let out = with_session(&state, |s| s.finish_apply(job, report));
        let _ = app.emit("apply-done", JobDone::from(out));
    });
}

#[tauri::command]
pub fn apply(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let job = with_session(&state, |s| s.begin_apply(&state.volumes))?;
    spawn_apply(app, job);
    Ok(())
}

#[tauri::command]
pub fn retry_failed(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let job = with_session(&state, |s| s.begin_retry(&state.volumes))?;
    spawn_apply(app, job);
    Ok(())
}

#[tauri::command]
pub fn pause(state: State<'_, AppState>) -> Res<()> {
    with_session(&state, |s| {
        s.pause();
        Ok(())
    })
}

#[tauri::command]
pub fn resume(state: State<'_, AppState>) -> Res<()> {
    with_session(&state, |s| {
        s.resume();
        Ok(())
    })
}

#[tauri::command]
pub fn cancel(state: State<'_, AppState>) -> Res<()> {
    with_session(&state, |s| {
        s.cancel_apply();
        Ok(())
    })
}

/// "A sync is running. Stop after the current file?" -> Yes.
#[tauri::command]
pub fn stop_and_close(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    with_session(&state, |s| {
        s.cancel_apply();
        Ok(())
    })?;
    std::thread::spawn(move || {
        loop {
            let busy = app
                .state::<AppState>()
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|s| s.is_applying());
            if !busy {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        app.exit(0);
    });
    Ok(())
}

#[tauri::command]
pub fn trash_runs(state: State<'_, AppState>, id: String) -> Res<Vec<TrashRunInfo>> {
    with_session(&state, |s| s.trash_runs(&id, &state.volumes))
}

#[tauri::command]
pub fn trash_contents(
    state: State<'_, AppState>,
    id: String,
    run: String,
) -> Res<TrashRunContents> {
    with_session(&state, |s| s.trash_contents(&id, &run, &state.volumes))
}

#[tauri::command]
pub fn restore(
    state: State<'_, AppState>,
    id: String,
    run: String,
    paths: Vec<RelPath>,
    replace: bool,
) -> Res<usize> {
    with_session(&state, |s| {
        s.restore(&id, &run, &paths, replace, &state.volumes)
    })
}

#[tauri::command]
pub fn empty_run(state: State<'_, AppState>, id: String, run: String) -> Res<()> {
    with_session(&state, |s| s.empty_run(&id, &run, &state.volumes))
}

fn open(app: &AppHandle, path: &std::path::Path) -> Res<()> {
    std::fs::create_dir_all(path).ok();
    open_path_raw(app, path)
}

fn open_path_raw(app: &AppHandle, path: &std::path::Path) -> Res<()> {
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| AppError::new("io").with("detail", e))
}

#[tauri::command]
pub fn open_logs_folder(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    open(&app, &state.data_dir.join("logs"))
}

#[tauri::command]
pub fn open_data_folder(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    open(&app, &state.data_dir)
}

#[tauri::command]
pub fn open_path(app: AppHandle, path: PathBuf) -> Res<()> {
    open_path_raw(&app, &path)
}
