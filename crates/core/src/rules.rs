//! Which paths a scan ignores: built-in system junk plus the pair's own patterns.

use crate::model::RelPath;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Suffix of the temporary file a copy writes before renaming into place.
pub const TEMP_SUFFIX: &str = ".replica-sync.tmp";
/// File written briefly to test whether a drive ignores capital letters.
pub const CASE_PROBE_NAME: &str = ".replica-sync-case-probe";

pub const BUILTIN_PATTERNS: &[&str] = &[
    "$RECYCLE.BIN/",
    "System Volume Information/",
    "Thumbs.db",
    "desktop.ini",
    ".Trash-*",
    ".DS_Store",
    "lost+found/",
    ".sync-trash/",
    "*.replica-sync.tmp",
    ".replica-sync-case-probe",
];

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid skip rule {pattern:?}: {message}")]
pub struct RuleError {
    pub pattern: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct SkipRules {
    names: GlobSet,
    name_dir_only: Vec<bool>,
    paths: GlobSet,
    path_dir_only: Vec<bool>,
}

impl SkipRules {
    pub fn new(user: &[String]) -> Result<SkipRules, RuleError> {
        let (mut names, mut name_dir_only) = (GlobSetBuilder::new(), Vec::new());
        let (mut paths, mut path_dir_only) = (GlobSetBuilder::new(), Vec::new());
        let all = BUILTIN_PATTERNS
            .iter()
            .map(|s| s.to_string())
            .chain(user.iter().map(|s| s.trim().to_string()));
        for pattern in all {
            if pattern.is_empty() || pattern.starts_with('#') {
                continue;
            }
            let dir_only = pattern.ends_with('/');
            let body = pattern.trim_end_matches('/').trim_start_matches('/');
            if body.is_empty() {
                return Err(RuleError {
                    pattern,
                    message: "pattern is empty".into(),
                });
            }
            let glob = match GlobBuilder::new(body)
                .case_insensitive(true)
                .literal_separator(true)
                .build()
            {
                Ok(g) => g,
                Err(e) => {
                    return Err(RuleError {
                        message: e.to_string(),
                        pattern,
                    });
                }
            };
            if body.contains('/') {
                paths.add(glob);
                path_dir_only.push(dir_only);
            } else {
                names.add(glob);
                name_dir_only.push(dir_only);
            }
        }
        let build = |b: GlobSetBuilder| {
            b.build().map_err(|e| RuleError {
                pattern: String::new(),
                message: e.to_string(),
            })
        };
        Ok(SkipRules {
            names: build(names)?,
            name_dir_only,
            paths: build(paths)?,
            path_dir_only,
        })
    }

    pub fn is_skipped(&self, rel: &RelPath, is_dir: bool) -> bool {
        let hit = |set: &GlobSet, dir_only: &[bool], text: &str| {
            set.matches(text)
                .into_iter()
                .any(|i| is_dir || !dir_only[i])
        };
        hit(&self.names, &self.name_dir_only, rel.name())
            || hit(&self.paths, &self.path_dir_only, rel.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rel;

    fn rules(user: &[&str]) -> SkipRules {
        SkipRules::new(&user.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn builtins_skip_system_folders_and_junk_files() {
        let r = rules(&[]);
        assert!(r.is_skipped(&rel("$RECYCLE.BIN"), true));
        assert!(r.is_skipped(&rel("System Volume Information"), true));
        assert!(r.is_skipped(&rel("Photos/Thumbs.db"), false));
        assert!(r.is_skipped(&rel("Photos/THUMBS.DB"), false));
        assert!(r.is_skipped(&rel(".sync-trash"), true));
        assert!(r.is_skipped(&rel(".Trash-1000"), true));
        assert!(r.is_skipped(&rel("a/b.jpg.replica-sync.tmp"), false));
        assert!(!r.is_skipped(&rel("Photos/a.jpg"), false));
        // folder-only built-in does not hit a file of the same name
        assert!(!r.is_skipped(&rel("lost+found"), false));
    }

    #[test]
    fn user_name_path_and_folder_only_patterns() {
        let r = rules(&["*.tmp", "Temp/", "Docs/old/*", "# a comment", "  "]);
        assert!(r.is_skipped(&rel("a/b.tmp"), false));
        assert!(r.is_skipped(&rel("x/Temp"), true));
        assert!(!r.is_skipped(&rel("x/Temp"), false));
        assert!(r.is_skipped(&rel("Docs/old/a.txt"), false));
        assert!(!r.is_skipped(&rel("x/Docs/old/a.txt"), false));
        assert!(!r.is_skipped(&rel("Docs/old/sub/a.txt"), false));
    }

    #[test]
    fn invalid_pattern_is_reported_with_its_text() {
        let err = SkipRules::new(&["[".to_string()]).unwrap_err();
        assert_eq!(err.pattern, "[");
    }
}
