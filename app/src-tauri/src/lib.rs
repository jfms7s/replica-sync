//! Tauri shell: plugins, app state, the close guard and the command list.
//! All logic lives in `app_state`; this file only wires it to the webview.

mod commands;

use app_state::error::AppError;
use app_state::paths::AppPaths;
use app_state::session::Session;
use replica_sync_core::volume::{self, SystemVolumes};
use std::sync::Mutex;
use tauri::{Emitter, Manager, WindowEvent};

pub struct AppState {
    /// `None` when `pairs.json` could not be read (`open_error` says why).
    pub session: Mutex<Option<Session>>,
    pub open_error: Option<AppError>,
    pub data_dir: std::path::PathBuf,
    pub volumes: SystemVolumes,
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
                let guard = state.session.lock().unwrap();
                if let Some(s) = guard.as_ref() {
                    if s.is_applying() {
                        api.prevent_close();
                        let _ = window.emit("close-requested", ());
                    } else {
                        s.cancel_scan();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::startup_status,
            commands::data_folder,
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
            commands::empty_run,
            commands::open_logs_folder,
            commands::open_path,
            commands::open_data_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running replica-sync");
}
