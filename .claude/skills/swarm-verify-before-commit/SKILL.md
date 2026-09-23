---
name: swarm-verify-before-commit
description: Use before every commit in this repo (Rust workspace and/or the Kotlin tv-android client) - the verification sequence that's been run before each of this project's commits, catching real bugs every time it surfaced a first-try failure.
---

# Verifying a change before committing (Rust + Kotlin)

This repo's history is unusually clean because nothing gets committed
without this sequence actually passing — not just "looks right." Several
real bugs (a wrong LAN candidate address, a kwik connection-lifecycle
issue, a roster-sync race, a misleading log line) were caught exactly
because this checklist was followed even when the change "obviously"
looked correct.

## Rust changes

```bash
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
```
(cargo is not on the default PATH in this environment — every Rust command
needs this first, in every fresh shell context.)

1. `cargo check -p <crate>` (or the specific crates touched) for a fast
   first pass.
2. `cargo test -p <crate>` (or `--test <name>` for one integration test
   file) and actually read the pass/fail output, not just the exit code.
3. **Re-run any new or timing-sensitive test 3-5 times** (`for i in 1 2 3;
   do cargo test ...; done`) before trusting it — this repo has hit real,
   rare flakiness (documented, not hidden, when found: see the
   `swarm-stun-client/tests/signaling.rs` doc comment for one such case)
   and a single green run doesn't rule it out.
4. `cargo test --workspace` — confirm nothing *else* broke. Changes to
   shared crates (`swarm-core`, `swarm-p2p`) have repeatedly had
   ripple effects on `apps/server`/`apps/stun-server` tests.
5. `cargo clippy --workspace --all-targets` — must be silent. This repo
   has zero tolerance for clippy warnings; every commit lands clean.
6. If you touched a binary the Kotlin interop tests spawn
   (`swarm-stun-server`), rebuild the release binary
   before re-running any Kotlin interop test that uses it — the interop
   tests silently run against a **stale** binary otherwise:
   ```bash
   cargo build --release -p stun-server --bin swarm-stun-server
   ```

The media server is GUI-owned; the supported desktop binary is
`swarm-server-app`. Check it with
`cargo check -p swarm-server --bin swarm-server-app`. Do not restore or build
the removed `swarm-serverd` just to satisfy an old fixture. Building the native
Whisper dependency requires CMake on the developer machine.

For changes under `apps/server/ui`, also run:

```bash
node --test apps/server/ui/test/*.test.js
```

## Kotlin changes (`clients/tv-android`)

```bash
export JAVA_HOME=/opt/homebrew/opt/openjdk@17   # NOT the default `java` — too new for AGP
export ANDROID_HOME=~/Library/Android/sdk        # only needed for :app tasks
```

1. `./gradlew :core:compileKotlin` first — fastest signal.
2. `./gradlew :core:test` — the fast suite (no subprocess, no Rust
   toolchain needed). Every fixture/unit test lives here.
3. `./gradlew :core:interopTest` — only if you touched anything that talks
   to a real Rust process (signaling, reflector, punch, QUIC, `CatalogSession`).
   Needs the applicable release binary built first (see above). Legacy tests
   that look for the removed `swarm-serverd` may skip; a skip is not validation
   of media-server interoperability. Re-run new tests 3+ times, same reasoning
   as the Rust side.
4. `./gradlew :app:compileDebugKotlin` — confirm the Android module still
   builds against whatever `:core` API changed. This has broken silently
   before (e.g. `refresh()` becoming `suspend` needed two existing test
   call sites wrapped in `runBlocking`).
5. For anything touching the manifest, a new dependency, or `:app` UI:
   `./gradlew :app:assembleDebug` then verify the real APK:
   ```bash
   BUILD_TOOLS="$ANDROID_HOME/build-tools/35.0.0"
   APK=$(find clients/tv-android/app/build/outputs/apk/debug -name "*arm64-v8a*.apk" | head -1)
   "$BUILD_TOOLS/aapt" dump badging "$APK" | grep -E "leanback-launchable|uses-feature-not-required|uses-feature: name"
   "$BUILD_TOOLS/aapt" dump badging "$APK" | grep -iE "google|gms"   # must find nothing — no Play Services allowed
   ```
   Never trust "it compiled" alone for anything manifest- or
   dependency-related — a new library can silently pull in something that
   breaks Fire TV Appstore compliance.
6. Do **not** run `:app:lintDebug` as a signal — it's a known-broken
   toolchain combination (AGP 8.7.3 / Kotlin 2.0.21,
   `IncompatibleClassChangeError` in stock lint detectors, unrelated to
   your code). `compileDebugKotlin`/`assembleDebug` are the real signal.

## Roku changes (`clients/tv-roku`)

1. `npm run build` in `clients/tv-roku` — BrighterScript compile + type-check
   (`bsc --project bsconfig.json`). This is the fast local check; run it after
   any `.bs`/`.xml` edit.
2. If you changed `src/source/CatalogGrouping.bs` (show grouping, 0-season
   filter, first-episode/track): `python3 tests/roku_catalog_grouping.py` from
   the repo root. There is no on-device BrightScript runner in this environment;
   that script source-checks the filter against Fire TV's `previewSeasons`
   contract and exercises an equivalent grouping.

## Before every commit, regardless of language

- `git status` and `git diff --stat` — confirm the file list matches what
  you actually intended to change, nothing stray (generated files,
  accidentally-staged secrets, leftover scratch files).
- If you created a throwaway file for investigation (a fixture-printer
  example, a debug script), delete it and confirm `git status` shows it
  gone *before* staging — never commit scratch.
- Write the commit message explaining **why**, not just what — this
  repo's commit messages consistently record the reasoning, the bug found
  (if any) and how it was diagnosed, and what was tried and rejected. A
  future session (or person) reading `git log` should understand the
  investigation, not just the diff.
