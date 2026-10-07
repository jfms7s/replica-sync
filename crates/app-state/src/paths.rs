//! Where the app keeps its files (Tauri's app data folder on each OS).

use std::path::PathBuf;

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
}
