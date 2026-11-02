//! Every name the engine itself writes into a replica, defined once, each with
//! the policy that keeps it from being treated as user data.
//!
//! `rules::BUILTIN_PATTERNS` repeats some of these as glob literals (it is a
//! `&[&str]` shown to the user, so it can't be built with `format!`); a test in
//! `rules.rs` fails if the two drift apart.

/// Folder at the replica root that holds trashed files.
/// Skip policy: skipped by the built-in rule `".sync-trash/"`.
pub const TRASH_DIR: &str = ".sync-trash";

/// Suffix of the temporary file a copy writes before renaming into place.
/// Skip policy: special-cased by the scan before any rule runs (an entry with
/// this suffix becomes a leftover or a problem); the built-in rule
/// `"*.replica-sync.tmp"` is kept so the app lists it, but the scan sees it first.
pub const TEMP_SUFFIX: &str = ".replica-sync.tmp";

/// Suffix of the intermediate name in a capital-letters-only rename.
/// Skip policy: deliberately visible. Matched by neither `TEMP_SUFFIX` nor any
/// skip rule, so a crash between the two renames leaves an ordinary, visible
/// replica entry, never one that is silently deleted as a leftover or hidden.
pub const CASE_RENAME_SUFFIX: &str = ".replica-sync-rename";

/// File written briefly to test whether a drive ignores capital letters.
/// Skip policy: skipped by the built-in rule `".replica-sync-case-probe"`.
pub const CASE_PROBE_NAME: &str = ".replica-sync-case-probe";
