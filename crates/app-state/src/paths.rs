//! Where the app keeps its files (Tauri's app data folder on each OS).

use crate::error::AppError;
use replica_sync_core::safety::canonical;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn new(data_dir: PathBuf) -> AppPaths {
        AppPaths { data_dir }
    }
    pub fn pairs_file(&self) -> PathBuf {
        self.data_dir.join("pairs.json")
    }
    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// `path` as a file inside the logs folder (both canonicalised with
    /// `safety::canonical`, so `..` and links can't lead out of it, and a long
    /// path compares like a short one). The UI may only open these. The result
    /// is `dunce::simplified` (no `\\?\` prefix below 260 characters) because
    /// it is handed to the OS opener, not compared again.
    pub fn log_file(&self, path: &Path) -> Result<PathBuf, AppError> {
        let not_a_log = || AppError::new("io").with("detail", "not a log file");
        let logs = canonical(&self.logs_dir()).map_err(|_| not_a_log())?;
        let file = canonical(path).map_err(|_| not_a_log())?;
        if file.starts_with(&logs) && file != logs && file.is_file() {
            Ok(dunce::simplified(&file).to_path_buf())
        } else {
            Err(not_a_log())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn only_files_inside_the_logs_folder_are_log_files() {
        let t = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(t.path().join("data"));
        fs::create_dir_all(paths.logs_dir()).unwrap();
        let log = paths.logs_dir().join("run.log");
        fs::write(&log, "x").unwrap();
        fs::write(t.path().join("secret.txt"), "s").unwrap();
        assert_eq!(
            paths.log_file(&log).unwrap(),
            dunce::simplified(&canonical(&log).unwrap())
        );
        let refused = [
            t.path().join("secret.txt"),
            paths.logs_dir().join("../../secret.txt"),
            paths.logs_dir(),
            paths.logs_dir().join("missing.log"),
            paths.pairs_file(),
        ];
        for p in refused {
            let e = paths.log_file(&p).unwrap_err();
            assert_eq!(
                (e.code, e.params["detail"].as_str()),
                ("io", "not a log file"),
                "{p:?}"
            );
        }
    }

    /// The logs folder below 260 characters and the log file above: on Windows
    /// `dunce` gave one a `\\?\` prefix and not the other, so a real log was
    /// refused. Both sides now come from `safety::canonical`.
    #[test]
    fn a_log_past_the_long_path_limit_is_still_a_log_file() {
        let t = tempfile::tempdir().unwrap();
        let mut data = t.path().to_path_buf();
        while canonical(&data).unwrap().join("logs").as_os_str().len() < 225 {
            data.push("0123456789");
            fs::create_dir_all(&data).unwrap();
        }
        let paths = AppPaths::new(data);
        fs::create_dir_all(paths.logs_dir()).unwrap();
        assert!(
            dunce::simplified(&canonical(&paths.logs_dir()).unwrap())
                .as_os_str()
                .len()
                < 260
        );
        let log = paths.logs_dir().join(format!("{}.log", "r".repeat(50)));
        fs::write(&log, "x").unwrap();
        assert!(log.as_os_str().len() > 260);
        assert_eq!(
            canonical(&paths.log_file(&log).unwrap()).unwrap(),
            canonical(&log).unwrap()
        );
    }
}
