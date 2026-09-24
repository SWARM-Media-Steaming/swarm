# Closed-loop Fire TV testing

Three suites close the loop on the media server and the Amazon Fire TV
client, plus an orchestrator that runs all three together and a
continuous-checking wrapper around that. Two of the three suites require
real Fire TV hardware on the same LAN and can't run on GitHub-hosted CI —
see [Why local-only](#why-local-only-hardware). The third,
`media_server_uat_tests.sh`, is the media server's own backend/API UAT
suite — plain `cargo test`, no hardware, no LAN, CI-friendly. All of the
above (and the orchestrator/cron wrapper's own behavior) are
change-controlled: **read the `swarm-e2e-suite-lockdown` skill before
editing any of their test logic.** None of them is touched by editing
another.

## TL;DR — run everything

```bash
./scripts/run_now.sh                        # 1. start the local media server (if not already running — separate terminal, this blocks)
./scripts/tests/full_uat_suite.sh --github-issue  # 2. runs the three suites below in order, one consolidated report/issue
```

`full_uat_suite.sh` is the one-command entry point. It runs, in order:
`media_server_uat_tests.sh` → `tv_e2e_suite.sh` → `tv_uat_suite.sh`,
captures each one's own evidence exactly as it would produce running
standalone, and — only when **at least one test actually failed**, and only
when `--github-issue` was passed — files **one** consolidated GitHub issue
covering every suite's result, passes and failures both, instead of the up
to three separate issues the suites would otherwise file (each wrapped
suite always runs with its own issue-filing suppressed). A clean run with
`--github-issue` still prints "no failures — nothing to file" and opens
nothing. Exit code `0` only if nothing failed anywhere.

```bash
./scripts/tests/full_uat_suite.sh                       # local-only, no issue filed regardless of result
./scripts/tests/full_uat_suite.sh --skip-backend        # skip media_server_uat_tests.sh
./scripts/tests/full_uat_suite.sh --skip-e2e            # skip tv_e2e_suite.sh
./scripts/tests/full_uat_suite.sh --skip-uat            # skip tv_uat_suite.sh
./scripts/tests/full_uat_suite.sh --include-resilience  # also run the opt-in disruptive resilience suite
./scripts/tests/full_uat_suite.sh --device 192.168.0.148  # forwarded to both hardware suites
./scripts/tests/full_uat_suite.sh --all                   # forwarded to both hardware suites: force full fan-out
```

The two hardware suites still can't run **at the same time as each other**
outside the orchestrator either — they point the TV at one shared
testing-mode control file, the exact path the already-running server was
configured with at startup (`run_now.sh` sets `SWARM_TV_E2E_CONTROL_FILE`
once, and the server only ever reads that one path for the rest of its
life). Each suite writes a fresh token there for its own run and deletes it
on exit; if two runs overlap, whichever finishes first deletes the file the
other is still using. `full_uat_suite.sh` always runs them sequentially, so
this is handled for you; running any two of the suites concurrently
yourself, outside the orchestrator, is not safe.

No flags needed for the common case: both hardware suites run against your
preferred device (see below) if one is configured, otherwise they fan out
across every Fire TV they find on the LAN. All three suites need the server
already running — none of them starts it.

### Continuous checking: `full_uat_cron.sh`

`full_uat_suite.sh` above runs once. `full_uat_cron.sh` wraps it in a
foreground loop that runs **once a day, at a fixed local time (03:00 by
default)**, against whatever's on `main` (including local commits not yet
pushed) at that moment, skipping the run entirely if nothing changed since
yesterday:

```bash
./scripts/tests/full_uat_cron.sh          # run in a terminal you can Ctrl+C — checks once daily at 3am
SWARM_FULL_UAT_CRON_HOUR=5 ./scripts/tests/full_uat_cron.sh   # 5am instead
```

The once-a-day cadence is deliberate, not just a convenient default: if a
failure ever triggers a bad interaction with something else that reacts to
new activity on this tracking issue, the loop is bounded to once per 24
hours instead of firing on every commit — this happened for real once (see
below), which is why the schedule changed from checking every commit to a
single fixed daily time.

It's deliberately a plain foreground process, not a real system cron/
launchd job — leave it running in its own terminal/tmux session and
`Ctrl+C` it whenever you want to stop. It skips a check entirely if the
previous one is still running (never overlaps two `full_uat_suite.sh`
invocations), and — instead of filing a new GitHub issue every time it
finds a failure — reuses the same tracking issue across runs as long as
it's still open, so a break and its eventual fix show up as one timeline
in one ticket; a new failure after the old issue was closed opens a fresh
one.

**A word of caution from real experience:** assigning this tracking issue
to yourself can opt it into external automation that watches for issues
assigned to you. That automation cannot verify a fix against
`tv_uat_suite.sh`'s real-hardware
failures (it has no access to the physical Fire TV), so a "fix" it merges
can easily leave the exact same failures in place, this cron re-reports
them as a new comment, and the other automation reads that as more work to
do. That loop burned real AI usage before the daily-schedule change above
existed. Leaving this tracking issue unassigned avoids the interaction
entirely.

Two preconditions are checked before every run, once a real code change is
pending — neither one updates the "last tested" state on failure, so the
same pending commit is retried at the next check once the precondition
clears:

- The local media server must already be answering its health endpoint
  (same check the two hardware suites use themselves) — this script never
  starts one.
- The SMB share from `batocera.local` (real UAT media root storage) must
  be mounted **and** pass a real directory read, not just be listed by
  `mount` — a dropped SMB connection can sit there looking healthy until
  something actually tries to read it.

On a failure, if a Claude or Codex CLI has spare quota, it also asks one of
them — read-only, no file edits, no shell access — to post a plain-text
triage comment (likely cause, where to look) on the same tracking issue.
Which provider gets asked alternates every failure, preferring whichever
one didn't run the previous triage, falling back to the other if the
preferred one is over quota, and skipping
the triage step entirely (silently, no GitHub noise) if neither has
capacity right now.

See the script's own header comment for the full behavior and every env
var (`SWARM_FULL_UAT_CRON_HOUR` to change the daily hour,
`SWARM_FULL_UAT_CRON_INTERVAL` to replace the daily schedule with a fixed
interval — **testing this script only**, not for real use,
`SWARM_UAT_BATOCERA_HOST` to change the required SMB host,
`SWARM_UAT_TRIAGE_ENABLED=0` to disable AI triage entirely, `--once` to run
a single check-and-exit immediately for testing).

Fast local iteration on one scenario while developing (run the suite
standalone rather than through the orchestrator):

```bash
./scripts/tests/tv_uat_suite.sh --test BrowseCatalogUatTest
```

### What to expect

Both suites print progress as they go (device discovery, install, each test
case as it runs), then a Markdown results table, then exit. What a finished
run looks like:

- **Console:** a `PASS: N   FAIL: N   SKIP: N` summary line, followed by a
  `| Device | Serial | Test | Result | Evidence |` table — one row per test
  case per device (`tv_uat_suite.sh`'s rows are `<TestClass>#<method>`).
  `SKIP` means a prior test case on that device failed and cascaded (e.g. the
  app never launched, so nothing downstream could run).
- **Exit code:** `0` only if `FAIL` is `0` across every device tested; `2` if
  a precondition failed before any device was even reached (server not
  running, no server data dir found); otherwise non-zero. Safe to chain in CI
  or a script (`suite.sh && next_step` only proceeds on a real all-clear) and
  safe to check with `echo $?` locally.
- **Local report file:** `.run/tv-e2e-reports/<timestamp>/report.md` and
  `.run/tv-uat-reports/<timestamp>/report.md` respectively (gitignored).
  `tv_uat_suite.sh` also leaves one evidence folder per FAIL alongside its
  report — see [Failure evidence](#failure-evidence-tv_uat_suitesh) below;
  nothing extra is written for a clean PASS run beyond the report and
  per-device logcat.
- **GitHub issue:** `tv_uat_suite.sh` is local-only by default. Pass
  `--github-issue` to opt in to `gh issue create` with the `Testing` label when
  at least one test failed; a clean PASS run never files anything.
  `--no-issue` remains an explicit local-only compatibility flag. The older
  `tv_e2e_suite.sh` retains its existing reporting behavior.
- **No Fire TV found:** both suites treat this as a *finding*, not a hard
  failure — they'll say so in the report/console (`FAIL_COUNT` stays `0` if
  nothing else failed) rather than erroring out, since running on a network
  with no TV present is a legitimate state, not a bug.

## The three suites

| | `tv_e2e_suite.sh` | `tv_uat_suite.sh` | `media_server_uat_tests.sh` |
|---|---|---|---|
| What it proves | The app launches, pairs, and completes one real catalog round-trip | Seventeen real UI scenarios: browse, detail, like/watchlist/report-a-problem, playback, music, filters, STUN server activation | Media server command/API correctness: media roots, library scan, TV pairing approval, notifications/client-errors, metadata editing, MCP tokens |
| How it asserts | Logcat/adb evidence only — **sends no D-pad input** | Real Compose UI navigation (`androidx.compose.ui.test` + `androidx.test.uiautomator`) | Real `#[tauri::command]` handlers called directly against a real, isolated `ServerCore`/SQLite/filesystem behind a mocked Tauri runtime — **no UI, no IPC layer** |
| SQLite validation | None | Real server (`library.sqlite`) and TV-side (`swarm.db`, `SharedPreferences`) state, cross-checked | Real, per-test isolated `library.sqlite`/settings.json |
| Hardware needed | Real Fire TV on the LAN | Real Fire TV on the LAN | None — plain `cargo test`, CI-friendly |
| Device targeting | Preferred device by default (see below); `--all` for full fan-out | Preferred device by default (see below); `--all` for full fan-out | N/A |
| On failure | Per-device logcat dump | Full UI-to-server evidence bundle (see below) | `cargo test`'s own panic output (assertion diff, real error text) |
| Runtime | ~1 minute/device | Several minutes (17 scenarios, one plays real video/audio for 30s each) | ~1 second |
| Skill | `swarm-closed-loop-tv-testing` | `swarm-tv-uat-suite` | `swarm-media-server-uat-tests` |

Use `tv_e2e_suite.sh` as the fast "did I break the build" check after any
change touching the client, server, or LAN pairing path. Use
`tv_uat_suite.sh` when a change could affect actual screen content, like/
watchlist/report-a-problem behavior, playback, or the filter bar — or run
its `--test` form for just the one scenario your change touches. Use
`media_server_uat_tests.sh` for any change to a media server command
handler (`apps/server/src/gui.rs`) or the library/scan/pairing/notification
logic it calls into — it needs no hardware, so run it first; it'll catch a
real backend regression in about a second, before spending minutes on the
hardware suites.

## Prerequisites

- The local media server running (`./scripts/run_now.sh`) — both suites only
  health-check it, never start/stop it (GUI-owned lifecycle).
- ADB debugging enabled on each TV, with the one-time on-device "Allow" trust
  prompt already accepted (`deploy_fire_tv.sh` will prompt you through this
  the first time it hits an unauthorized device). This can't be automated —
  it's an Android security control, not a workflow gap.
- `ANDROID_HOME` / `JAVA_HOME` set, or the defaults
  (`~/Library/Android/sdk`, `/opt/homebrew/opt/openjdk@17`) already correct.
- For `tv_uat_suite.sh`'s server-side SQLite checks: the server's real data
  dir must be reachable (default macOS Tauri path, override with
  `SWARM_SERVER_DATA_DIR` if yours differs).

## Preferring one device for local runs

Both suites share one config file, `scripts/tests/tv_test_device.local.json`
(gitignored — not committed) — set it once and both default to that device:

```json
{ "preferred_device_name": "Michael's 4th TV" }
```

Copy `scripts/tests/tv_test_device.local.json.example` to get started. The name is
matched against the TV's own `settings get global device_name`. Local runs
should set this once and get fast, single-device iteration by default; use
`--all` on either suite to force full fan-out (e.g. for a pre-release
comprehensive pass) regardless of the preference file. If the preferred
device isn't found on the LAN, both suites log that and fall back to full
fan-out rather than silently doing nothing.

Override for a single run without touching the config file, on either suite:

```bash
./scripts/tests/tv_e2e_suite.sh --device 192.168.0.148
./scripts/tests/tv_e2e_suite.sh --device "Living Room TV"
./scripts/tests/tv_uat_suite.sh --device 192.168.0.148
./scripts/tests/tv_uat_suite.sh --device "Living Room TV"
```

Device-targeting precedence is the same on both: explicit `--device` (or, on
`tv_e2e_suite.sh`, a bare positional IP/name — kept for backward
compatibility) > preferred device from the config file > every
already-adb-connected Amazon device > full LAN scan fan-out.

## Scenario catalog (`tv_uat_suite.sh`)

| Class | Covers |
|---|---|
| `BrowseCatalogUatTest` | Browse auto-opens; movies/shows/music load with box art; Continue Watching capped at 6; filter bar (media type, Liked-only, genre) |
| `MovieDetailLikeUatTest` | Movie detail screen fields; Like round-trips through the "Liked only" filter |
| `MovieWatchlistUatTest` | Watchlist add/remove round-trip, toasts, Watchlist row |
| `MovieProblemReportUatTest` | Report-a-problem → choose a category → dismiss; and the server-resolve round-trip |
| `MoviePlaybackPauseUatTest` | Movie playback, FF/RW, pause overlay fields, resume, no Next Episode button |
| `ShowPlaybackPauseUatTest` | Episode playback, pause overlay, Next Episode button |
| `ShowSeasonsEpisodesWatchlistUatTest` | Season/episode structure, show watchlist round-trip |
| `MusicPlaybackUatTest` | Artist → album → track browsing, per-track Like, shuffle/skip, mini-player |
| `NavigationSearchPersistenceUatTest` | Pure D-pad traversal; focus/back-stack restoration; title search, no-results/clear, combined filters, alphabetical Browse All; Like/watchlist persistence across a fresh Activity |
| `ContinuePlaybackLifecycleUatTest` | Continue Watching save/resume/completion removal; acknowledged server session release; audio/subtitle selection; moving browse previews and preview-to-play handoff |
| `KidModeUatTest` | PIN setup/rejection, media-kind filtering, restart persistence, and disable/cleanup |
| `AddServerFromDashboardUatTest` | Main SWARM page "Add Server" visibility (next to "Browse library"); STUN activation-code creation; cancellation back to the dashboard |
| `EndOfMediaUatTest` | Episode Continue Play Now/Cancel and automatic next-track playback, using a debug testing-mode near-end seek to keep the real-player scenarios bounded |

The deliberately disruptive client-transport drop/recovery checks remain a
separate opt-in journey. They close real client connections but never start,
stop, or mutate the GUI-owned server:

```bash
./scripts/tests/tv_uat_resilience_suite.sh
./scripts/tests/tv_uat_resilience_suite.sh --device 192.168.0.148
```

Full detail on each scenario, known deviations from earlier scenario
wording (e.g. "reviews" doesn't exist as a field — the pause overlay shows
an aggregate rating instead), and the server-resolve mechanism live in the
`swarm-tv-uat-suite` skill.

## Scenario catalog (`media_server_uat_tests.sh`)

One Rust test file per category under `apps/server/src/gui_tests/`:

| File | Covers |
|---|---|
| `media_root_lifecycle.rs` | Add/list/remove a media root; duplicate-label rejection; last-root-removal guard |
| `library_scan.rs` | Rescan add/update/remove reconciliation against real files on disk; the empty-root mass-deletion safety guard |
| `tv_pairing.rs` | LAN pairing-code approval (real invalid-code error path); local peer listing |
| `notifications_and_errors.rs` | Client-error report → list → resolve → clear round-trip, seeded through the same write path a real client uses |
| `metadata_editing.rs` | Manual title/genres/overview/rating overrides on a real scanned entry |
| `mcp_tokens.rs` | MCP access-token generation/rotation and the enabled toggle |
| `ai_integration.rs` | AI provider settings round-trip (never leaking a saved key back to the UI); scan-assist/reorganize enablement gates; the reorganize scan/approve/reject lifecycle on real files (approve moves and rescans, reject never touches disk) |

Real UI-visible flows (browse, playback, watchlist, ...) aren't covered here
by design — see `swarm-media-server-uat-tests` (skill) for why (no reliable
macOS UI-automation path today) and what's covered instead. `harness.rs`
gives each test a real, isolated data directory and its own unique QUIC/HTTP
ports, so tests are safe to run concurrently in the same process — see its
doc comment for why that isolation is necessary (Tauri's `mock_context()`
otherwise gives every test the same shared path/ports).

## Failure evidence (`tv_uat_suite.sh`)

Every failed scenario writes a self-contained folder under
`.run/tv-uat-reports/<run>/<device>/<test>/` (gitignored) with everything
needed to debug without touching real hardware again:

- **TV side:** screenshot, UIAutomator window-hierarchy XML, Compose
  semantics tree, logcat since test start, a `swarm.db` dump, and the
  liked/watchlist/watch-state `SharedPreferences` files.
- **Server side:** the relevant `library.sqlite` rows (library entries,
  likes, problem reports, notifications), `server-state.sqlite`, and a tail
  of `logs/server.log` (server request tracing — method/path/status/latency/
  device name — added specifically to make this bundle useful).

`tv_e2e_suite.sh` keeps a simpler per-device logcat dump under
`.run/tv-e2e-reports/<run>/`.

## Invocation reference

```bash
# tv_e2e_suite.sh
./scripts/tests/tv_e2e_suite.sh                    # preferred device if configured, else full fan-out
./scripts/tests/tv_e2e_suite.sh --all              # force full fan-out
./scripts/tests/tv_e2e_suite.sh --device 192.168.0.148   # one device, by IP or device_name
./scripts/tests/tv_e2e_suite.sh 192.168.0.148      # same, positional form (backward compatible)
./scripts/tests/tv_e2e_suite.sh --no-issue         # skip filing a GitHub issue
./scripts/tests/tv_e2e_suite.sh --skip-install     # smoke-test an already-installed build

# tv_uat_suite.sh
./scripts/tests/tv_uat_suite.sh                        # preferred device if configured, else full fan-out
./scripts/tests/tv_uat_suite.sh --all                  # force full fan-out
./scripts/tests/tv_uat_suite.sh --device 192.168.0.148 # one device, by IP or device_name
./scripts/tests/tv_uat_suite.sh --test BrowseCatalogUatTest            # one scenario class
./scripts/tests/tv_uat_suite.sh --test MusicPlaybackUatTest#testLike   # one scenario method
./scripts/tests/tv_uat_suite.sh --github-issue          # opt in to filing findings on GitHub
./scripts/tests/tv_uat_suite.sh --no-issue              # explicit local-only mode (the default)
./scripts/tests/tv_uat_suite.sh --skip-install         # smoke-test an already-installed build

# media_server_uat_tests.sh
./scripts/tests/media_server_uat_tests.sh              # run every backend UAT test
./scripts/tests/media_server_uat_tests.sh media_root    # run only tests whose name contains this substring

# full_uat_suite.sh (orchestrator — runs all three suites above, in order)
./scripts/tests/full_uat_suite.sh                       # local-only report, no issue filed regardless of result
./scripts/tests/full_uat_suite.sh --github-issue        # file one consolidated issue if TOTAL_FAIL > 0
./scripts/tests/full_uat_suite.sh --skip-backend        # skip media_server_uat_tests.sh
./scripts/tests/full_uat_suite.sh --skip-e2e            # skip tv_e2e_suite.sh
./scripts/tests/full_uat_suite.sh --skip-uat            # skip tv_uat_suite.sh
./scripts/tests/full_uat_suite.sh --include-resilience  # also run tv_uat_resilience_suite.sh
./scripts/tests/full_uat_suite.sh --device 192.168.0.148  # forwarded to both hardware suites
./scripts/tests/full_uat_suite.sh --all                   # forwarded to both hardware suites
```

Every suite here is a plain deterministic script — a human, a CI runner
(once a self-hosted LAN runner exists; see below), and an AI agent all
invoke the exact same command with the exact same result. Nothing in any of
them calls an LLM at run time. `full_uat_suite.sh` always passes
`--no-issue` down to `tv_e2e_suite.sh`/`tv_uat_suite.sh` regardless of its
own `--github-issue` flag — it's the one that decides whether to file,
once, after seeing every suite's result, not each wrapped suite on its own.

## Why local-only hardware

Both suites need: real Fire TV hardware physically present, on the same LAN
as the machine running the suite, already `adb`-trust-authorized (a one-time
human tap on the device), plus the local media server already running.
GitHub-hosted runners can provide none of that, so there is no
`.github/workflows` entry for either suite today — they're designed to be
CI-friendly (clean exit codes, machine-parseable results) for whenever a
self-hosted LAN runner exists, without one being wired up now.

## Changing either suite

Both suites' test logic, thresholds, fan-out behavior, and (for the UAT
suite) evidence-bundle contents are frozen by explicit user policy — **read
`swarm-e2e-suite-lockdown` before editing either.** A FAIL is a real bug
report: fix the product code, don't loosen the test. Genuine infrastructure
bugs in the scripts themselves (shell quoting, adb timing races, an
instrumentation-output parsing edge case) are fair game to fix without
asking — see the skill for exactly where that line is.
