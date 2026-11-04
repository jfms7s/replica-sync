# Changelog

Notable changes to replica-sync. Releases are built from `v*` tags into draft GitHub releases.

## Unreleased

### Changed
- App state: a pair's backup disks are chosen per scan. Sync goes to the plugged-in disk, and with both plugged in to the one synced longest ago (a never-synced disk counts as the oldest); the Preview data names the target and both disks so the other can be picked. A sync records only its own disk's last sync and scan size, and the Trash follows the disk it was asked for. New commands add, remove and relink a disk; the screens for them come next. The one-way store change below applies.
- Core: a pair now keeps a list of backup disks (one or two) in `pairs.json`, each with its own last sync and scan size, so the first-sync guard is per disk. Nothing changes on screen yet: the app still works on a pair's first backup disk.
- **One-way store change:** the next save writes `pairs.json` as format 2 (`version: 2`, a `replicas` list). An existing store is read as before and is not touched until something is saved, but a replica-sync older than this change shows its Broken screen on a format 2 file (without overwriting it). Keep the file from before the upgrade if you may go back.

## 0.7.0 — 2026-11-03

Supersedes the unpublished 0.6.0 draft (and through it 0.5.1, 0.5.0, 0.4.1 and 0.4.0) and includes all of their changes (below). Like them, not yet checked on a real Windows PC with an exFAT/FAT32 stick; the release stays a draft until the owner does.

### Added
- The Trash screen offers "Show in the file manager" for a trash run that holds files its record does not list, so they can be found and copied back by hand (spec §6.5). Only that run's `files` folder is shown; nothing in it is opened or run.

### Changed
- A file moved to another folder and renamed only in capital letters (`A.jpg` → `other/a.jpg`) is planned as one move on a backup that ignores capital letters (Windows, exFAT), instead of a delete and a full copy.
- A saved-pairs file written by a newer replica-sync is refused with a clear error instead of being read as if it were this version's format.

### Fixed
- Opening the logs or data folder, the folder and save dialogs, and "Edit pair" after a failed scan now show an error when they fail instead of doing nothing.
- The pairs list clears an old error once a later action works; the "same drive" answer in the pair editor is forgotten when a folder is changed.
- Dialogs close with Escape, start with focus on their first button and keep Tab inside.
- Settings no longer reload over what you typed after saving a new language; the trash age is limited to 36,500 days (in the app and in the engine).
- Preview tree: keyboard focus survives collapsing a folder with the mouse, Left arrow on a file goes to its folder, and "Show them" says how many folders it could not open because they are past the first rows.
- A scan cancelled before it started goes back to the pairs list; "Restore all" on a trash run with nothing recorded says so instead of "Restored: 0"; the trash buttons no longer flicker.
- Clearer texts: the Result title says "Sync stopped" for a stopped run, the not-done line reads "Not done: N", and the top-level "more not shown" note no longer mentions a checkbox the root does not have.
- Changing or re-linking a pair's folder onto the drive of its other side now asks first, as creating a pair does.
- A pair folder deleted while its drive is plugged in is reported as missing, not as "drive not connected".
- A folder whose name is not valid Unicode is refused when the pair is created, instead of being saved with a replacement character and later reported missing.
- Two pairs created in quick succession can no longer get the same id (which replaced the first one).
- A scan finishes and the Preview opens even when the pair list can't be saved or the free space can't be read; the Preview shows a note instead (with unknown free space, the "not enough space" check is skipped).
- Problems pruning old logs or listing old trash runs after a sync are shown as notes on the Result screen instead of being ignored.
- "The drive is not the one saved" names the source folder when it was the source that moved.
- If a sync stops on an internal error, the Result screen says so and offers to scan again (before, it said "cancelled").
- "Stop and close" exits after at most a minute even if the current file never finishes, and closing the window no longer freezes the app while a slow trash operation runs.
- Cancel stops a scan quickly even inside one folder with hundreds of thousands of files.
- A folder whose listing fails part-way shows one problem row, not one per failed item.
- Sizes just under a unit boundary print as `1.0 MB`, not `1024.0 KB`.
- Old run logs are still pruned when one of them can't be removed.
- A failed move into the trash no longer leaves empty folders in that trash run.
- Changing a pair's folder no longer skips the "one folder inside the other" check when the other drive can't be queried; the error is shown instead.

## 0.6.0 — 2026-11-02

Supersedes the unpublished 0.5.1 draft (and through it 0.5.0, 0.4.1 and 0.4.0) and includes all of their changes (below). Like them, not yet checked on a real Windows PC with an exFAT/FAT32 stick; the release stays a draft until the owner does.

### Added
- **Spot check after every sync.** When a sync finishes without stopping, replica-sync reads back up to 50 of the files it just copied (at most 1 GB) and compares their bytes with the source. The Applying screen shows "Checking copies", the Result screen says how many were checked, and a copy whose bytes differ is listed as failed ("the copy, read back, differs from the source") so **Retry failed** copies it again (the bad copy goes to the trash). Pause and Cancel work during the check. On Linux the copy is read from the drive, not from memory; on Windows the read may come from the cache, which is why it is called a spot check. `replica-sync-cli apply` does the same and prints it in its log.
- Release drafts carry `SHA256SUMS.txt` for every file.

### Changed
- Copying many small files is about three times faster (5,000 files: 0.95 s → 0.33 s on a RAM disk): the copy buffer is allocated once per sync and each backup folder is made once.
- Release builds are smaller (link-time optimisation, stripped): the CLI shrinks from 3.6 MB to 2.3 MB.
- The Preview's folder tree draws only the rows on screen, so a big first sync with many open folders stays responsive; ticking a box reloads only the folders it changes.
- The built-in "Always skipped" list no longer shows `*.replica-sync.tmp`: those names were never skipped by that rule (a leftover temp on the backup is offered as a delete).
- One Cargo workspace and lockfile for the whole project (developers: engine commands now pass `--exclude replica-sync-app`, see the README).

### Fixed
- A source folder that holds only links (or other skipped entries) is now created on the backup; before, it was missing.
- Run logs never overwrite each other (two syncs of one pair in the same second, or two pair names that differ only in non-ASCII letters); the second gets `-2` in its name.
- The Trash screen orders more than nine trash runs from the same second correctly (`-10` after `-9`).
- On Windows, looking for a saved drive no longer leaves the "no insert-a-disk dialogs" error mode switched on for the rest of that thread.
- `replica-sync-cli` errors name the file or folder they failed on.
- A release tag that does not match the version in the project files now fails before anything is built, and a release makes exactly one draft.

## 0.5.1 — 2026-11-02

Supersedes the unpublished 0.5.0 draft (and through it 0.4.1 and 0.4.0) and includes all of their changes (below). Like them, not yet checked on a real Windows PC with an exFAT/FAT32 stick; the release stays a draft until the owner does.

### Fixed
- Opening a log from the app no longer refuses a log whose full path is longer than 260 characters on Windows (the logs folder and the file were compared in two different path forms).

## 0.5.0 — 2026-11-02

Supersedes the unpublished 0.4.1 draft (which superseded 0.4.0) and includes all of its changes (below). Like them, not yet checked on a real Windows PC with an exFAT/FAT32 stick; the release stays a draft until the owner does.

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
