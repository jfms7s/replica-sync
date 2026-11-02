# replica-sync

One-way folder mirror for Windows and Linux. One folder is the source of truth; the other (the replica) is brought up to date by copying, moving and removing only what changed — after the user approves every change in a preview. Removed and replaced files go to a `.sync-trash` folder on the replica and can be restored.

Status: desktop app (v0.6.0) for Windows and Linux, in English and Portuguese.

## Install

Download the installer for your system from the [latest release](https://github.com/jfms7s/replica-sync/releases):

- **Windows:** `replica-sync_0.6.0_x64_en-US.msi` or `replica-sync_0.6.0_x64-setup.exe`. The installers are not code-signed yet, so Windows SmartScreen shows "Windows protected your PC". Click **More info**, then **Run anyway**.
- **Linux:** `replica-sync_0.6.0_amd64.AppImage` (make it executable and run it) or the `.deb`. If the window doesn't open, run it with `WEBKIT_DISABLE_DMABUF_RENDERER=1` (already the app's default) or `GDK_BACKEND=x11`.

## Use

1. **Add pair:** choose the folder to back up and the backup folder (for example on a USB drive).
2. **Sync:** replica-sync compares both folders and shows every change in a folder tree.
3. Untick anything you don't want, then **Apply selected**. Deleted and replaced files go to `.sync-trash` on the backup drive and can be restored from **Trash**.

## Develop

The same checks CI runs (`.github/workflows/ci.yml`), per part. One Cargo workspace (one `Cargo.lock`, one `target/`) holds all four crates; the desktop crate `replica-sync-app` needs WebKit, so the engine commands exclude it:

```bash
# Engine, app state and CLI (repo root)
cargo fmt --all --check
cargo clippy --workspace --exclude replica-sync-app --all-targets -- -D warnings
cargo test --workspace --exclude replica-sync-app --no-fail-fast
cargo run -p replica-sync-cli -- plan <source> <replica>

# UI (app/, Node 22)
cd app && npm ci
npm run typecheck && npm run lint && npm test && npm run i18n:check

# Desktop shell (needs the Tauri system packages; repo root, then app/)
cargo clippy -p replica-sync-app --all-targets -- -D warnings
cd app && npx tauri build --debug --no-bundle
npm run tauri dev                      # run the app
```

`app/src/i18n/codes.json` is generated from Rust: after adding a reason or error code, run `UPDATE_CODES=1 cargo test -p replica-sync-app-state codes_json` and add the new keys to `en.json` and `pt-PT.json`. The permission tests refuse to run as root.
