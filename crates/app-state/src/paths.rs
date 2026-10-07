//! Where the app keeps its files (Tauri's app data folder on each OS).

use crate::error::AppError;
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

    /// `path` as a file inside the logs folder (both canonicalised, so `..`
    /// and links can't lead out of it). The UI may only open these.
    pub fn log_file(&self, path: &Path) -> Result<PathBuf, AppError> {
        let not_a_log = || AppError::new("io").with("detail", "not a log file");
        let logs = dunce::canonicalize(self.logs_dir()).map_err(|_| not_a_log())?;
        let file = dunce::canonicalize(path).map_err(|_| not_a_log())?;
        if file.starts_with(&logs) && file != logs && file.is_file() {
            Ok(file)
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
            dunce::canonicalize(&log).unwrap()
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
}
