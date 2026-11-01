# Changelog

Notable changes to replica-sync. Releases are built from `v*` tags into draft GitHub releases.

## Unreleased

### Changed
- Windows: copies are now flushed in batches like on Linux. Up to 256 files (64 MB) are written to temps, each temp gets its own `FlushFileBuffers` (four at a time), and only then are they renamed into place. Before, each file was flushed and renamed before the next started, which made small-file mirrors slow on Windows and on exFAT sticks.
- Windows: a drive whose filesystem refuses `FileIdInfo` (some FAT32/exFAT drivers) now gets its own id form, `win32:<8 hex digits>` (the 32-bit volume serial), instead of sharing the `win:` prefix with the 64-bit `FileIdInfo` id. Pairs saved earlier with the old `win:00000000<serial>` form still find and verify their drive. `replica-sync-cli volume` still prints which method made the id.
