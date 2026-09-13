---
name: swarm-oneoff-migration-binaries
description: Use when a real (not hypothetical) piece of user data needs a one-time repair or migration — a past bug left files/rows in the wrong shape and someone now has to fix the already-existing mess, not just the code going forward. Covers adding a lean, non-GUI `[[bin]]` target to apps/server, sharing GUI-private logic with it, and the dry-run-first/never-delete conventions this repo's two existing migrations (`migrate-whisper-subtitles`, `fix-movies-layout`) both follow. Not for regular features or tests — this is specifically the "there's an actual mess on an actual machine right now" shape of task.
---

# One-off migration binaries

`apps/server` is a Tauri desktop app, but it isn't *only* one. Two bugs so
far have needed a real, one-time repair pass over data a past bug already
corrupted — `migrate-whisper-subtitles` (issue #41: subtitles written to
the wrong directory before the fix) and `fix-movies-layout` (a mangled
Movies folder plus the `subtitles.rs` whisper-suffix parsing gap). Both
follow the same shape; use it rather than inventing a new one, and rather
than reaching for a throwaway shell/Python script that reimplements logic
this repo already has carefully tested.

## Add a lean `[[bin]]` target, not a Tauri feature

In `apps/server/Cargo.toml`:

```toml
[[bin]]
name = "your-migration-name"
path = "src/bin/your_migration_name.rs"
```

No `required-features = ["gui"]` — that's what keeps this buildable (and,
critically, cross-compilable) without pulling in Tauri/WebView/GTK at all.
Verify this actually holds with:

```bash
cargo build -p swarm-server --bin your-migration-name --no-default-features
```

## Reuse GUI logic by promoting it, never by duplicating it

`gui.rs` is itself a separate binary target (`path = "src/gui.rs"`) with its
own private `mod` tree, distinct from the crate's `lib.rs`. A private
`mod reorganize;` (or similar) declared inside `gui.rs` is invisible to a
new bin target in the same package — only what `lib.rs` marks `pub mod` is
shared. If the migration needs logic that currently lives GUI-side:

1. Move the `mod x;` line from `gui.rs` to `lib.rs` as `pub mod x;`
   (`transcription`, `ai`, and `reorganize` are all done this way already).
2. Update `gui.rs`'s own `use swarm_server::{...}` import list to pull `x`
   in from the shared lib instead of declaring its own copy.
3. Confirm both binaries still build: `cargo build -p swarm-server
   --no-default-features` (lean bins) and `cargo build -p swarm-server
   --bin swarm-server-app` (the real Tauri app) — a module that only
   compiled because it happened to sit next to Tauri-dependent code in the
   same file has bitten this exact refactor before.

This keeps exactly one implementation of the real logic (so the migration
gets the *same* classify()/reorganize quality as the live product feature,
not a second, drifting reimplementation), and the promoted module stays
available to the next migration too.

## Conventions every migration here follows

- **Dry-run by default.** No flag = print the full plan, touch nothing.
  `--apply` opts into real writes. (`migrate-whisper-subtitles` instead
  uses `--dry-run` to opt *into* safety — inconsistent with newer tools;
  prefer "print-only by default" for anything new, since a forgotten flag
  should never be the difference between safe and destructive.)
- **Never delete.** A file/folder that can't confidently be re-homed gets
  quarantined (moved somewhere clearly marked for manual review, e.g.
  `_cleanup_leftovers/<original name>/`) or left exactly in place — never
  removed outright, even when it looks obviously disposable.
- **Report, don't guess.** Every skip, conflict, or ambiguous match prints
  the specific reason. An item silently dropped from the report is a bug in
  the tool, not an acceptable outcome.
- **Idempotent-safe.** Re-running an already-applied migration should find
  nothing left to do (or safely no-op each item) rather than erroring or,
  worse, redoing something.
- **Test against a synthetic mock tree before a real target.** Build the
  exact directory/file shape the real bug produced with `mktemp -d` +
  a handful of `touch`/`mkdir`, run `--apply` against *that*, and inspect
  the result — before ever pointing the binary at production data.

## Cross-compiling for a remote target

`swarm-media`'s dependency tree (sqlx, reqwest, quinn) is not practical to
cross-link natively from macOS to Linux — use Docker (a plain
`rust:<version>` image mounting the workspace, or `cross`) rather than
fighting a manual cross toolchain. Confirm the target host's architecture
first (`ssh <host> uname -m`) — most Linux media-server boxes (including
Batocera) are `x86_64-unknown-linux-gnu`, not the `aarch64-apple-darwin`
this repo's dev machines usually target.

## See also

- `media-server-background-work` — if the new logic is a *durable, ongoing*
  worker rather than a one-time repair, that skill's conventions apply
  instead (or in addition, if the migration exists because a background
  worker's past output needs fixing, as with the Whisper-subtitle case).
- `swarm-verify-before-commit` — the standard verification sequence still
  applies in full to a new migration binary; it's product code like
  anything else in this repo.
