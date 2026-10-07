# replica-sync

One-way folder mirror for Windows and Linux. One folder is the source of truth; the other (the replica) is brought up to date by copying, moving and removing only what changed — after the user approves every change in a preview. Removed and replaced files go to a `.sync-trash` folder on the replica and can be restored.

Status: sync engine (`crates/core`) and a test CLI (`crates/cli`). The desktop app is not built yet.

```bash
cargo test --workspace
cargo run -p replica-sync-cli -- plan <source> <replica>
```
