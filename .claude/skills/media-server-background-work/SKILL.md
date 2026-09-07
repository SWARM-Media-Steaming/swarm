---
name: media-server-background-work
description: Use when adding or changing durable background media processing in the native Tauri media server, such as local Whisper subtitles, waveform generation, analysis, or another long-running queue. Covers the GUI-owned ServerCore lifecycle, relational SQLite jobs and checkpoints, restart recovery, model/tool downloads, playback-priority pausing, live dashboard progress, completed-artifact serving, and TV client delivery.
---

# Durable media-server background work

The desktop GUI is the product and the server process. Do not reintroduce a
separate headless daemon. `AppState::core()` lazily owns one `Arc<ServerCore>`;
closing the window hides it to the tray, while **Quit SWARM** ends the process.
Background work therefore continues while hidden and must recover from a real
quit, crash, upgrade, or reboot.

## Persist the work, not only its percentage

Use `library.sqlite` through `swarm_media::store::Library`. Model a worker with
relational rows like local transcription does:

- one job row keyed to `library_entries`, with a foreign key and `ON DELETE
  CASCADE`;
- checkpoint rows keyed by `(entry_key, segment_index)`, foreign-keyed to the
  job and written idempotently with an upsert;
- completed artifact rows foreign-keyed to the library entry;
- `CHECK` constraints for statuses/counts and an index beginning with the
  queue-claim status/order columns.

Persist the source fingerprint and processor/model version on the job. Preserve
checkpoints only when all three still match; reset stale work atomically when
the media or processing version changes. On startup, turn claimed
`transcribing`/`finalizing` rows back into `queued` rows. Never advertise a
partial artifact to a client.

Split expensive work into bounded checkpoints. Whisper uses ten-minute source
sections: enough to avoid excessive setup overhead, but small enough that a
restart loses at most one section. Commit each section before starting the
next, then assemble the final WebVTT into a `.part` file and atomically rename
it before marking the job complete.

## Keep blocking compute off Tokio

Own the worker under `ServerCore` and start one long-lived `tokio::spawn` loop.
Use async I/O for SQLite, downloads, and FFmpeg subprocesses, but run native CPU
inference inside `tokio::task::spawn_blocking`. Keep enable state and
fine-grained inference progress in atomics; use `Notify` to wake a disabled or
idle worker without polling rapidly.

Playback always wins. Check `TranscodeManager::active_sessions()` before
claiming and between checkpoints, and install the native engine's abort
callback so a stream that starts mid-checkpoint stops compute promptly. Requeue
that interruption without recording a failure. Limit compute threads rather
than consuming every logical CPU. A real inference error is a failed job, not
an interruption to retry forever.

## Reuse the filesystem snapshot/diff scanner

Library maintenance must begin with `swarm_media::scan`'s two-pass design:
walk each root once, recording relative path, absolute path, size, and mtime
in the SQLite-backed scan manifest, then reconcile that point-in-time
snapshot against `library_entries`. SQLite staging is intentional: it gives
the same keyed snapshot/diff semantics as an in-memory map without allowing a
very large or remote library to grow process memory without bound.

Treat path as identity for reconciliation. A new path is an add, an absent
path is a deletion/tombstone, and a move is consequently one delete plus one
add. Only new files or files whose size/mtime changed may enter fingerprint,
tag, and probe work during the normal fast scan. Never add an unconditional
hash/fingerprint pass to scheduled scans.

`ScanOptions::comprehensive_check` is the explicit exception: run the normal
snapshot/diff first, then reuse `swarm_core::fingerprint::fingerprint_file`
for metadata-identical existing files in path order. Keep that loop
sequential to prevent disk-I/O fan-out. A mismatch follows the ordinary
changed-file upsert path and marks provider metadata stale for incremental
re-scraping. Preserve incomplete-root quarantine and suspicious-empty-root
protection when extending this flow; those prevent transient SMB/NFS states
from wiping or churning the served catalog.

Every new root has a declared `MediaRootAssetType`; `Mixed` is compatibility
only for older settings. Use the type to filter and classify candidates
before expensive processing. Individual audio-track indexing is a separate,
default-off desktop preference. Music provider work remains grouped once per
artist/album, and its not-found/failure issues must continue through the
server notification path.

## Install optional models safely

Do not ask users to install a transcription executable. Link `whisper.cpp`
through `whisper-rs`; only the model downloads at runtime. Native desktop builds
need CMake, but packaged end users do not.

For a large model download:

1. Write to a server-owned `.part` path and resume with HTTP `Range`.
2. Expose downloaded and total bytes in worker status.
3. Verify the upstream-published digest before installation.
4. Atomically rename only after verification.
5. Delete a corrupt completed partial so a `416 Range Not Satisfiable` cannot
   trap every future retry.
6. Preserve a valid partial when the feature is disabled or the app exits.

Use a descriptive user agent and an upstream project/model URL, not an
unversioned third-party binary mirror. Explain download size, CPU/time cost,
local-only data handling, restart behavior, and playback pausing before or when
the user enables the feature.

## Wire settings and status through the native app

Add a serde-defaulted field to `apps/server/src/settings.rs` so old
`settings.json` files remain compatible. Apply the setting immediately after
`ServerCore::start`, and expose narrow Tauri commands to set it and fetch a
serializable status snapshot.

On the dashboard:

- put the opt-in toggle on **Details** with a Bootstrap icon and a shared
  `INFO_TOPICS` modal; enabling must explicitly say the model downloads
  automatically;
- keep progress markup outside any library `.innerHTML` re-render;
- show the progress panel even while disabled, and poll status silently in the
  background (one second is appropriate for active progress);
- combine checkpoint totals with the current native callback percentage so
  the bar does not freeze during a long section;
- make the panel sticky only within the Media card so it remains visible while
  browsing without covering content outside that page.

Extend `apps/server/ui/test/boot_order.test.js` whenever a new cross-file
function or startup poll is introduced. Its invoke stub must return the full
new status shape.

## Deliver completed artifacts through playback

Add artifact metadata to `swarm-core`'s `PlaybackPlan` with `#[serde(default,
skip_serializing_if = ...)]`, then mirror it in Kotlin with a default value.
During negotiation, include only rows whose fingerprint still matches and
whose completed file exists.

Serve an artifact by looking up `(entry_key, artifact_id)` in SQLite. Never
turn request text directly into a filesystem path. Return the peer path in the
plan; `CatalogSession` must convert both media and artifact paths through the
authenticated loopback proxy. For VTT, attach Media3
`SubtitleConfiguration`s to the same `MediaItem` so `PlayerView` supplies its
normal subtitle selector/off control.

## Verify the whole slice

At minimum run:

```bash
cargo check -p swarm-server --bin swarm-server-app
cargo test -p swarm-server --lib
cargo test -p swarm-media --test library --test playback
node --test apps/server/ui/test/*.test.js
cd clients/tv-android && ./gradlew :core:test :app:compileDebugKotlin
```

Test restart recovery from a committed checkpoint, foreign-key cascade cleanup,
completed artifact negotiation/serving, Rust/Kotlin contract decoding, and the
disabled-state progress UI. Do not download the full production model in a
routine unit test.
