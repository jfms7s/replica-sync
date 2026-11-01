# Changelog

Notable changes to replica-sync. Releases are built from `v*` tags into draft GitHub releases.

## Unreleased

## 0.4.1 — 2026-11-01

Supersedes the unpublished 0.4.0 draft and includes all of its changes (below). Like 0.4.0, not yet checked on a real Windows PC with an exFAT/FAT32 stick; the release stays a draft until the owner does.

### Changed
- Builds use a pinned Rust toolchain (1.98.1, `rust-toolchain.toml`); the minimum supported Rust is declared (1.88, the desktop app 1.90) (#25).
- The Preview no longer re-sends the list of folders with deletes on every checkbox click; "Show them" fetches the first 50 when clicked. Faster clicks on previews with many deleted folders.

### Fixed
- Trash bookkeeping is sturdier when the disk fails mid-write: a torn manifest line no longer swallows the next one; a move whose manifest line fails is undone without overwriting a file that appeared at the old path (and says so if it can't be undone); a failed first move leaves no empty trash run; and restoring from the trash now records each file the same careful way, never replacing a file that appeared since the conflict check.
- A replica holding two names that differ only by capital letters (possible in a case-sensitive NTFS folder or a non-casefold ext4 folder) is now shown as skipped, "two names differ only by capital letters", and nothing changes at that name. Before, one of the two files was silently ignored and its folder could fail to be removed on every run.
- A folder named `*.replica-sync.tmp` is no longer silently left out of the backup (on the source) or left alone with no explanation (on the replica). It now shows in the Preview as skipped, "uses a name replica-sync reserves for its temporary files", and nothing under it changes. A link with that name is reported as a link.
- Opening a preview no longer deletes anything. A replica file named `*.replica-sync.tmp` (a temp left by an interrupted run, or a user's own file with that name) used to be removed for good during the scan; it now shows in the Preview as an ordinary delete and goes to `.sync-trash` on Apply, before any copy that would write a temp at that name. If that delete is unticked, the copy fails with "something already exists" instead of removing the file. These deletes do not count towards the first-sync wrong-folder warning.

## 0.4.0 — 2026-11-01

Not yet checked on a real Windows PC with an exFAT/FAT32 stick (drive id, copy speed); the release stays a draft until the owner does.

### Changed
- Windows: copies are now flushed in batches like on Linux. Up to 256 files (64 MB) are written to temps, each temp gets its own `FlushFileBuffers` (four at a time), and only then are they renamed into place. Before, each file was flushed and renamed before the next started, which made small-file mirrors slow on Windows and on exFAT sticks.
- Windows: a drive whose filesystem refuses `FileIdInfo` (some FAT32/exFAT drivers) now gets its own id form, `win32:<8 hex digits>` (the 32-bit volume serial), instead of sharing the `win:` prefix with the 64-bit `FileIdInfo` id. Pairs saved earlier with the old `win:00000000<serial>` form still find and verify their drive. `replica-sync-cli volume` still prints which method made the id.

### Security
- The desktop app's `tokio` is 1.54.1, for RUSTSEC-2026-0386 (#13).
