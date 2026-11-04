//! Tauri shell: plugins, app state, the close guard and the command list.
//! All logic lives in `app_state`; this file only wires it to the webview.

mod commands;

use app_state::error::AppError;
use app_state::paths::AppPaths;
use app_state::session::Session;
use replica_sync_core::volume::{self, SystemVolumes};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, WindowEvent};

pub struct AppState {
    /// `None` when `pairs.json` could not be read (`open_error` says why).
    pub session: Mutex<Option<Session>>,
    pub open_error: Option<AppError>,
    pub data_dir: std::path::PathBuf,
    pub volumes: SystemVolumes,
}

/// Whether a close must wait for the user (a sync is running). A running scan
/// is cancelled, since the window is about to go.
fn keep_open_for_apply(session: Option<&Session>) -> bool {
    match session {
        Some(s) if s.is_applying() => true,
        Some(s) => {
            s.cancel_scan();
            false
        }
        None => false,
    }
}

/// Set while a thread waits to decide a close that found the session busy.
static CLOSE_WAITER: AtomicBool = AtomicBool::new(false);

/// How long that thread waits for the session lock.
const CLOSE_WAIT: Duration = Duration::from_secs(10);

/// Whether a sync is running, once the session lock is free; `None` when it
/// stays busy for [`CLOSE_WAIT`].
fn wait_for_close_decision(window: &tauri::Window) -> Option<bool> {
    let state = window.state::<AppState>();
    let deadline = Instant::now() + CLOSE_WAIT;
    while Instant::now() < deadline {
        match state.session.try_lock() {
            Ok(guard) => return Some(guard.as_ref().is_some_and(|s| s.is_applying())),
            Err(TryLockError::Poisoned(e)) => {
                return Some(e.into_inner().as_ref().is_some_and(|s| s.is_applying()));
            }
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    None
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first: a second launch focuses the existing window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let (session, open_error) = match Session::open(AppPaths::new(data_dir.clone())) {
                Ok(s) => (Some(s), None),
                Err(e) => (None, Some(e)),
            };
            app.manage(AppState {
                session: Mutex::new(session),
                open_error,
                data_dir,
                volumes: volume::system(),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                // Don't wait for the session lock here: a slow restore or trash
                // listing can hold it for a long time. (Other commands still
                // take it on the main thread; see review-preview-commands-on-main-thread.)
                let applying = match state.session.try_lock() {
                    Ok(guard) => Some(keep_open_for_apply(guard.as_ref())),
                    Err(TryLockError::Poisoned(e)) => {
                        Some(keep_open_for_apply(e.into_inner().as_ref()))
                    }
                    Err(TryLockError::WouldBlock) => None,
                };
                match applying {
                    Some(false) => {}
                    Some(true) => {
                        api.prevent_close();
                        let _ = window.emit("close-requested", ());
                    }
                    None => {
                        // Decide once the lock is free, off the event loop;
                        // `close` then asks again through this handler. One
                        // waiter at a time; if the lock stays busy past the
                        // deadline the window stays open and a later click
                        // tries again.
                        api.prevent_close();
                        if CLOSE_WAITER.swap(true, Ordering::AcqRel) {
                            return;
                        }
                        let window = window.clone();
                        std::thread::spawn(move || {
                            let decided = wait_for_close_decision(&window);
                            CLOSE_WAITER.store(false, Ordering::Release);
                            match decided {
                                Some(true) => {
                                    let _ = window.emit("close-requested", ());
                                }
                                Some(false) => {
                                    let _ = window.close();
                                }
                                None => {}
                            }
                        });
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::startup_status,
            commands::get_settings,
            commands::set_settings,
            commands::builtin_rules,
            commands::list_pairs,
            commands::save_pair,
            commands::delete_pair,
            commands::relink,
            commands::start_scan,
            commands::cancel_scan,
            commands::preview_summary,
            commands::tree_children,
            commands::tree_find,
            commands::delete_folders,
            commands::toggle,
            commands::select_all,
            commands::confirm_wrong_folder,
            commands::save_preview,
            commands::apply,
            commands::retry_failed,
            commands::pause,
            commands::resume,
            commands::cancel,
            commands::stop_and_close,
            commands::trash_runs,
            commands::trash_contents,
            commands::restore,
            commands::open_trash_run,
            commands::empty_run,
            commands::open_logs_folder,
            commands::open_path,
            commands::open_data_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running replica-sync");
}
