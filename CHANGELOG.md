# Changelog

Notable changes to replica-sync. Releases are built from `v*` tags into draft GitHub releases.

## Unreleased

### Changed
- Copies now keep the source's permission bits on Linux (an executable script stays executable, a private `0600` file stays private) and the read-only and hidden attributes on Windows, besides content and modification time. Not kept: ACLs, owners, alternate data streams, creation time, setuid/setgid. Where the backup drive can't hold them (FAT/exFAT on Linux) the copy still succeeds. Files already in a backup keep their old attributes until they next change.
- The "is this the right folder?" check now runs on every sync, not only a pair's first. When an Apply would remove or replace more than half of the backup's files on a later sync, the Preview asks "Is the source folder right?" (it may be empty, or another folder may now be at its place) and Apply stays off until you confirm.
- `replica-sync-cli apply --yes` refuses (exit code 3) when more than half of the replica would be removed or replaced, or the replica drive looks too small, unless `--force` is also given.

### Fixed
- Planning after renaming many folders is fast again: 8,000 renamed folders plan in 0.1 s instead of 2.3 s (move detection was quadratic in the number of renamed folders and in same-named files such as `cover.jpg`).
- An Apply now stops as "backup drive disconnected" (or "source drive disconnected") when the saved drive is no longer the one at that folder, checked during the run before each batch of copies is put in place and at least every 2 seconds (on Linux, a drive unmounted from under its folder is caught at the next change). Before, only a missing folder stopped it, so on Linux a backup drive unmounted from a fixed mount point (`/mnt/backup`) left an empty folder and the remaining copies went to the system disk.
- A move, delete or copy whose source can't be read (permission denied, in use) now fails with that reason and is offered by **Retry failed**; a move used to be skipped as "changed since preview". A new empty folder whose source folder was removed since the preview is skipped ("deleted since scan"), and a folder removal is skipped when the source has that folder again ("back on source").
- The Apply progress bar ends at 100 %: a copy that is skipped or fails is taken out of the total instead of leaving the bar short (or past the end).
- If a capital-letters-only rename fails half-way and can't be undone, the error now says where the file was left.

### Security
- Copies never follow a link on the source: a source file replaced by a link since the Preview is skipped as "changed since preview" instead of copying what the link points to (Linux `O_NOFOLLOW`, Windows opens the link itself). A link in a parent folder is still followed.
- A saved pair's folder path is now checked like every other path when the pairs file loads (`..`, empty parts, and on Windows `\` or `:`, are refused). Pairs saved by earlier versions load unchanged.
- On Linux the app's data folder, its `logs` folder, the pairs file, the settings file and new run logs are private to your user (folders 0700, files 0600).
- Every change to your files in the backup folder (copying, the rename of a finished copy into place, moving, creating and removing folders, trashing and restoring) now goes through one checked path, which checks right before the write that the folder still resolves inside the backup folder. A path under a link pointing outside is refused instead of followed. (The trash's own bookkeeping inside `.sync-trash` and the drive's case probe keep their own checks.)

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
