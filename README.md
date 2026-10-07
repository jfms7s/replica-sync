# replica-sync

One-way folder mirror for Windows and Linux. One folder is the source of truth; the other (the replica) is brought up to date by copying, moving and removing only what changed — after the user approves every change in a preview. Removed and replaced files go to a `.sync-trash` folder on the replica and can be restored.

Status: desktop app (v0.2.0) for Windows and Linux, in English and Portuguese.

## Install

Download the installer for your system from the [latest release](https://github.com/jfms7s/replica-sync/releases):

- **Windows:** `replica-sync_0.2.0_x64_en-US.msi` or `replica-sync_0.2.0_x64-setup.exe`. The installers are not code-signed yet, so Windows SmartScreen shows "Windows protected your PC". Click **More info**, then **Run anyway**.
- **Linux:** `replica-sync_0.2.0_amd64.AppImage` (make it executable and run it) or the `.deb`.

## Use

1. **Add pair:** choose the folder to back up and the backup folder (for example on a USB drive).
2. **Sync:** replica-sync compares both folders and shows every change in a folder tree.
3. Untick anything you don't want, then **Apply selected**. Deleted and replaced files go to `.sync-trash` on the backup drive and can be restored from **Trash**.

## Develop

```bash
cargo test --workspace                 # engine + app state
cd app && npm ci && npm test           # UI
npm run tauri dev                      # run the app (needs the Tauri system packages)
```
