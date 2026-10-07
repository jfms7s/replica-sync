//! The list of translatable codes the UI's i18n check reads (`app/src/i18n/codes.json`).

use crate::error::ERROR_CODES;
use replica_sync_core::reason::reason_codes;

pub fn codes_json() -> String {
    let value = serde_json::json!({ "reason": reason_codes(), "error": ERROR_CODES });
    serde_json::to_string_pretty(&value).expect("plain json") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Fails when an engine reason or app error was added without regenerating
    /// codes.json (and so, via `npm run i18n:check`, without translations).
    #[test]
    fn codes_json_is_up_to_date() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src/i18n/codes.json");
        let want = codes_json();
        if std::env::var_os("UPDATE_CODES").is_some() {
            std::fs::write(&path, &want).unwrap();
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            have, want,
            "codes.json is stale: run `UPDATE_CODES=1 cargo test -p replica-sync-app-state codes_json`"
        );
    }
}
