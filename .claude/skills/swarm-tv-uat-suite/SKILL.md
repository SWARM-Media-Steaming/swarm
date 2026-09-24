---
name: swarm-tv-uat-suite
description: Use when creating, running, extending, debugging, or explaining Fire TV UAT tests and scripts/tests/tv_uat_suite.sh — the real-D-pad suite against the real local media server. Covers stable Compose tags, bounded synchronization, fresh-process persistence tests, state cleanup, capability-aware media selection, debug-only test hooks, failure evidence, and focused/full real-device verification. Read swarm-e2e-suite-lockdown before editing test logic.
---

# TV UAT suite: real UI navigation against the real media server

`./scripts/tests/tv_uat_suite.sh` drives the actual Fire TV app UI (D-pad
navigation, real button presses) against a real, already-running local media
server, asserting on real Compose UI state, real persisted TV-side state
(Room `swarm.db` + the liked/watchlist/watch-state `SharedPreferences`
stores, read in-process), and real server-side SQLite state
(`library.sqlite`'s `library_entries`, `entry_likes`, `client_errors`,
`server_notifications` tables). Its change policy is the same explicit,
standing user rule as `scripts/tests/tv_e2e_suite.sh` — **read
`swarm-e2e-suite-lockdown` before editing anything here.**

## How this differs from `scripts/tests/tv_e2e_suite.sh`

The original suite (`swarm-closed-loop-tv-testing`) deliberately sends no
D-pad input at all — it proves the app launches and completes one real
catalog round-trip using only logcat/adb evidence. This suite exists because
the user explicitly asked for scenario coverage the original suite was never
meant to provide: opening a movie, liking it, adding to a watchlist,
reporting a problem and watching it resolve, playing and pausing video,
browsing a show's seasons/episodes, playing music, and exercising the filter
bar. That coverage requires real UI navigation, so this suite uses
`androidx.compose.ui.test` + `androidx.test.uiautomator` instrumented tests
instead of pure log-evidence — element lookup goes through stable Compose
`testTag`s (see `UatTestTags.kt` in the TV client's main UI source set), not
brittle text matching, so scenario tests survive copy changes.

Both suites coexist permanently and are frozen independently. Neither
suite's own file is touched by changes to the other.

## Scenario catalog

Each class lives under
`clients/tv-android/app/src/androidTest/kotlin/app/swarm/tv/app/uat/`; each
`@Test` method's KDoc references the scenario number from the conversation
that specified it.

| Class | Scenarios | Covers |
|---|---|---|
| `BrowseCatalogUatTest` | 1, 2, 3, 4, 5, 6, 16 | Browse auto-opens without clicking in; movies/shows/music load; box art loads for a movie, a show, a music artist; Continue Watching never shows more than 6; the filter bar's media-type (All/Movies/Shows/Music), Liked-only, and genre controls exist and work |
| `MovieDetailLikeUatTest` | 7, 8 | A random movie's detail screen has box art, year, genres, cast, description, Play/Like/Watchlist/Report-a-problem buttons; Like round-trips through the "Liked only" filter |
| `MovieWatchlistUatTest` | 9 | Watchlist add/remove round-trips, including the toast copy and the Watchlist genre row |
| `MovieProblemReportUatTest` | 10, 11 | Report-a-problem → toast → notification inbox → dismiss; and the server-resolve round-trip (see below) |
| `MoviePlaybackPauseUatTest` | 12 | Movie playback: play, fast-forward, rewind, pause overlay fields (asserts the aggregate rating shown, not free-text "reviews" — see Known deviations below), resume, settings, back-navigation stack |
| `ShowPlaybackPauseUatTest` | 13 | Same pause-overlay checks for an episode, plus the Next Episode button (present and functional — unlike the movie case) |
| `ShowSeasonsEpisodesWatchlistUatTest` | 14 | Season list with box art/episode counts, watchlist round-trip on a show, episode grid sequencing and non-duplication |
| `MusicPlaybackUatTest` | 15 | Artist → album → track browsing, per-track like round-trip through the filter, shuffle/skip/pause/resume, mini-player reopen/close |
| `NavigationSearchPersistenceUatTest` | Navigation/search/persistence | Pure remote navigation and focus restoration; search, empty results, clear, combined filters, Browse All ordering; likes/watchlist surviving a fresh Activity and ViewModel |
| `ContinuePlaybackLifecycleUatTest` | Playback lifecycle | Continue Watching save/resume/completion removal, persisted restoration, server release acknowledgement, audio/subtitle choice, browse-preview movement and preview-to-play |
| `KidModeUatTest` | Kid Mode | PIN setup, content-kind gating, wrong/correct PIN behavior, disabling Kid Mode, and persistence across a fresh Activity and ViewModel |
| `EndOfMediaUatTest` | End of media | Continue-overlay Play Now/Cancel behavior and automatic music-track advance at end of media |
| `ResilienceUatTest` | Resilience (opt-in) | Catalog and playback transport interruption/recovery; kept out of the default deterministic suite and run by `scripts/tests/tv_uat_resilience_suite.sh` |

The canonical, current scenario matrix is in `scripts/tests/TV_TESTING.md`; update
that matrix whenever the user explicitly authorizes adding a scenario.

## Baseline for writing reliable TV UAT tests

Use these rules for every new scenario. They encode failures already found on
real Fire TV hardware, not optional style preferences.

1. **Activate with real remote input.** Discover and assert elements through
   Compose semantics/test tags, but invoke user actions with actual D-pad
   directions and D-pad Center. Do not replace activation with `performClick`.
   A scenario specifically proving remote focus traversal must not use
   `RequestFocus`; other scenarios may use it only to establish a deterministic
   starting point before sending real D-pad input.
2. **Never wait for global UI idleness.** Testing mode has a visible countdown,
   so UIAutomator's default `waitForIdle` can consume the whole test without
   ever considering the app idle. Keep the configurator idle timeout bounded
   (currently 250 ms), send the key immediately, then wait for a named state:
   a tag appears/disappears, focus changes, a persisted value changes, or a
   server checkpoint is observed. Prefer condition polling over sleeps.
3. **Give startup its own budget.** The real desktop server, discovery, TLS,
   catalog negotiation, and image population are asynchronous. Use the shared
   `waitForCatalogReady` helper and its 45-second catalog budget instead of
   copying shorter arbitrary delays into individual tests. The host suite keeps
   its testing authorization token renewed for the duration of the run; the
   app's individual testing-mode activation remains limited to ten minutes.
4. **Tag behavior, not prose or coordinates.** Put additive tags in
   `UatTestTags.kt`. Use stable exact tags for singleton controls and stable
   prefixes plus real IDs for repeated/dynamic content. Copy changes and screen
   density must not break a scenario. Use visible text only when the text itself
   is the requirement.
5. **Select fixtures by capability.** The suite runs against the user's real,
   changing library. Do not hardcode a title, season, album, language, or row
   position when the assertion needs a capability. Inspect candidates and pick
   one that actually has subtitles, multiple audio tracks, a next episode, or
   another required property. Record the selected stable ID in tags/evidence.
6. **Make reruns non-destructive.** Snapshot pre-existing likes, watchlist,
   watch progress, Kid Mode, notifications, and similar persistent state before
   mutation. Restore exactly that state in `finally`/teardown, whether the test
   passes or fails. Tests must tolerate pre-existing Continue items and reports;
   identify the row created by this run rather than assuming the newest-looking
   UI item belongs to it.
7. **Use a genuinely fresh owner for persistence.** `scenario.recreate()` may
   retain process-scoped objects and is insufficient proof of store hydration.
   Use `restartActivityAndWaitForCatalog`, which closes the old scenario and
   launches a fresh Activity/ViewModel, then assert the UI and backing store.
8. **Keep hooks product-safe.** When end-of-media or transport recovery cannot
   be reached deterministically in reasonable time, add the smallest explicit
   hook. It must be gated by both `BuildConfig.DEBUG` and active testing mode,
   expose an observable completion marker, and leave release behavior unchanged.
   Never make the assertion weaker to accommodate the hook.
9. **Assert the real contract at every layer that matters.** UI state alone is
   insufficient for persistence/report/release cases. Cross-check the TV store,
   server SQLite/log checkpoint, or release acknowledgement as applicable. Do
   not remove evidence or loosen a threshold to turn a failure green.
10. **Keep deterministic and disruptive coverage separate.** Stable scenarios
    belong in `tv_uat_suite.sh`. Transport-drop, reconnection, and other
    environment-sensitive scenarios run through the opt-in resilience wrapper
    unless the user explicitly changes that contract.

The test-rule order is intentional: Compose rule outermost, Activity scenario
inside it, and failure capture inside the scenario lifecycle. This lets failure
capture see the live semantics tree while still guaranteeing cleanup. Reuse
`UatTestBase` navigation/wait/restart helpers and `UatMatchers` before inventing
local variants.

## Creating or changing a scenario

First read `swarm-e2e-suite-lockdown`. A failing test is not permission to
change its assertion; only a user's explicit request in the current
conversation changes the frozen scenario contract. Then:

1. State the user-visible behavior and the independent evidence proving it.
2. Add stable product tags or a narrowly gated debug hook if observability is
   missing.
3. Add the instrumented scenario under the existing `uat` package and register
   deterministic classes in `ALL_TEST_CLASSES`. Put disruptive coverage in the
   resilience wrapper.
4. Document the scenario in `scripts/tests/TV_TESTING.md`.
5. Compile locally, run the single class/method on the preferred real TV, repeat
   timing-sensitive coverage, and finally run the whole relevant suite.
6. Inspect the evidence bundle for every failure before modifying code. Fix
   product behavior or genuine orchestration/synchronization defects; preserve
   the scenario's intended assertions.

## Known deviations from the literal scenario wording (confirmed with the user)

- **"Reviews" doesn't exist as a field.** The pause overlay shows a
  community rating + vote count (`★ X.X/10 (N votes)`); scenario 12 asserts
  that instead of free-text reviews.
- **A movie's pause overlay correctly has no Next Episode button** (the app
  only shows it for `kind == EPISODE`). Scenario 12 asserts its absence;
  scenario 13 (a show/episode) asserts its presence and that it works.
- **"Favorites" and "Liked only" are the same feature** — there's no
  separate Favorites tab. Scenario 16 asserts the "Liked only" toggle.
- **Watchlist and continue-watching have no SQLite backing anywhere**
  (neither `library.sqlite` nor the TV's Room `swarm.db` — both live only in
  `SharedPreferences`). Those scenarios validate against the app's real
  persisted `SharedPreferences` store, read in-process from inside the
  instrumented test, rather than SQL.

## The server-resolve round-trip (scenario 11)

The dashboard's "Resolve" button is a Tauri desktop-GUI command with no
existing HTTP route, so an unattended run can't click it. `apps/server`
exposes a debug-build-only `POST /errors/{id}/resolve` endpoint (mirroring
the existing dismiss endpoint's gating) specifically for this suite. The
flow: the instrumented test opens the report picker, selects a real category
option by its `PROBLEM_REPORT_OPTION_PREFIX` tag with D-pad Center, then
confirms the "Problem Report Sent." toast before it emits a logcat checkpoint
(`UAT_AWAITING_SERVER_RESOLVE`) and waits. The orchestration script watches
for that checkpoint while the instrumentation run is still in progress,
queries `client_errors` for the most recently received still-unresolved row
(safe because scenario classes run sequentially per device — never two
report-a-problem tests concurrently against the same server), and calls the
debug resolve endpoint with `{"comments":"test"}` — then the test continues,
asserting the real notification-inbox UI shows the resolution (the app's own
`syncResolutionNotifications` polling, not a mock). This is the one scenario
where the orchestration script takes an action mid-run rather than only
observing. Scenario 10 (dismiss) uses the same mechanism to get a
notification to dismiss in the first place — the inbox only ever surfaces
*resolved* reports, so there is no unresolved-notification state to test
independently of resolution.

## Device targeting: preferred device vs. fan-out

Precedence: explicit `--device <ip-or-name>` > preferred device from
`scripts/tests/tv_test_device.local.json` (gitignored; example at
`scripts/tests/tv_test_device.local.json.example`) > every already-adb-connected Amazon
device > full LAN scan fan-out.

```json
{ "preferred_device_name": "Michael's 4th TV" }
```

The name is matched against the same `settings get global device_name` read
the original suite already uses. **Local runs should prefer one TV** (fast
iteration, one real device is enough evidence for most changes) — set this
file once. `--all` forces full fan-out across every discovered Fire TV
regardless of the preference file, for comprehensive runs. If the preferred
device isn't found on the LAN, the script logs that and falls back to full
fan-out rather than silently doing nothing.

`scripts/tests/tv_e2e_suite.sh` reads this exact same config file and follows the
identical precedence (see its own "Fan-out discovery" section in
`swarm-closed-loop-tv-testing`) — set the preferred device once and both
suites default to it.

## Invocation

```
./scripts/tests/tv_uat_suite.sh                        # preferred device if configured, else full fan-out
./scripts/tests/tv_uat_suite.sh --all                   # force full fan-out
./scripts/tests/tv_uat_suite.sh --device 192.168.0.148  # one specific device, by IP or device_name
./scripts/tests/tv_uat_suite.sh --test BrowseCatalogUatTest             # one scenario class
./scripts/tests/tv_uat_suite.sh --test MusicPlaybackUatTest#testLike    # one scenario method
./scripts/tests/tv_uat_suite.sh --no-issue              # skip filing to GitHub
./scripts/tests/tv_uat_suite.sh --skip-install          # smoke-test an already-installed build
./scripts/tests/tv_uat_resilience_suite.sh --device 192.168.0.148 --no-issue
```

Nothing here calls an LLM at run time — a human, a CI runner (once a
self-hosted LAN runner exists; there is no GitHub-hosted path, same
hardware/LAN/adb-trust reasons as the original suite), and an AI agent all
invoke the exact same deterministic script. `--test` is the cheap path for
iterating on one scenario without re-running all sixteen.

## Failure evidence: the full UI-to-server dump

Every FAIL writes a self-contained folder under
`.run/tv-uat-reports/<run>/<device>/<TestClass>#<method>/` (gitignored, not
attached to the filed issue) containing:

- **TV side:** a screenshot, the UIAutomator window-hierarchy XML, the
  Compose semantics tree, logcat since test start (all captured in-process
  by `UatFailureCaptureRule` and pulled off-device), a `swarm.db` dump, and
  the liked/watchlist/watch-state `SharedPreferences` XML files.
- **Server side:** the relevant rows from `library.sqlite`
  (`library_entries`/`entry_likes`/`client_errors`/`server_notifications`)
  and `server-state.sqlite`, and a tail of `logs/server.log` — the request
  tracing added alongside this suite (method/path/status/latency/device
  name) is what makes it possible to see what the server actually did in
  response to the TV action that failed.

This exists specifically so a follow-up debugging session doesn't need to
reproduce the failure on real hardware just to see what happened — the
bundle should be enough on its own, per the user's explicit requirement.

## Requirements this suite shares with the original

- ADB debugging authorized once per TV (human taps "Allow" on-device) — same
  Android security control, not automatable.
- The local media server must already be running (`./scripts/run_now.sh`);
  this suite never starts, stops, or restarts it.
- First-time D-pad pairing is never driven by this suite either — it uses
  the same debug-only testing-mode token mechanism as the original suite.
- The external automation worker requires a clean worktree. After verification,
  commit the authorized suite/test/product changes together with a message
  explaining the behavior and evidence; do not leave validated UAT work
  uncommitted.
