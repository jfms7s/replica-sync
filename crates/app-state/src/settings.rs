//! User settings: language and the default trash age for new pairs.

use replica_sync_core::pairs::DEFAULT_TRASH_DAYS;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "pt-PT")]
    PtPt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub language: Language,
    pub default_trash_days: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            language: Language::Auto,
            default_trash_days: DEFAULT_TRASH_DAYS,
        }
    }
}

/// The OS language, e.g. "pt-PT" or "en-US".
pub fn os_locale() -> Option<String> {
    sys_locale::get_locale()
}

impl Settings {
    /// Settings are not precious: a missing or damaged file gives the defaults.
    pub fn load(path: &Path) -> Settings {
        fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut f = fs::File::create(&tmp)?;
        f.write_all(
            serde_json::to_string_pretty(self)
                .map_err(io::Error::other)?
                .as_bytes(),
        )?;
        f.sync_all()?;
        drop(f);
        fs::rename(tmp, path)
    }

    /// "en" or "pt-PT".
    pub fn resolved_language(&self, os_locale: Option<&str>) -> &'static str {
        match self.language {
            Language::En => "en",
            Language::PtPt => "pt-PT",
            Language::Auto if os_locale.is_some_and(|l| l.to_lowercase().starts_with("pt")) => {
                "pt-PT"
            }
            Language::Auto => "en",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_the_os_locale() {
        let s = Settings::default();
        assert_eq!(s.resolved_language(Some("pt-PT")), "pt-PT");
        assert_eq!(s.resolved_language(Some("PT_br")), "pt-PT");
        assert_eq!(s.resolved_language(Some("en-GB")), "en");
        assert_eq!(s.resolved_language(None), "en");
        let fixed = Settings {
            language: Language::En,
            ..Settings::default()
        };
        assert_eq!(fixed.resolved_language(Some("pt-PT")), "en");
    }

    #[test]
    fn missing_or_damaged_file_gives_defaults_and_save_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("settings.json");
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        let s = Settings {
            language: Language::PtPt,
            default_trash_days: 14,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"pt-PT\"")
        );
    }
}
