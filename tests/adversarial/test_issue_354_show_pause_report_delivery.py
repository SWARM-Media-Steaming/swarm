#!/usr/bin/env python3
"""Issue #354 (trusted bug amendment): a show's "Report a problem" never
reaches the media server, even though the identical flow for a movie does.

Derived from the issue and the amendment *before* trusting the diff:

  "Everything works when doing this for a movie but it does not seem to
   work for a show. When I did it for both the media server correctly has
   the movie but the show never shows up on the media server notifications
   list."

A movie has two report surfaces: `MovieDetailScreen` (state =
`UiState.MovieDetail`) and the player's pause screen (state =
`UiState.Player`). A show has only one, because there is no season/show
detail report control (see `test_problem_report_surfaces.py`): the pause
screen. So whatever differs between `MovieDetail` and `Player` state
resolution is exactly where a show-only regression would hide, even though
UI-shape checks (button present, not gated on `MediaKind.MOVIE`, wired into
`PlayerScreen`) all pass.

`SwarmViewModel.reportAssetProblem` resolves its target device through
`UiState.embeddedCatalog()`, a `when` over the sealed `UiState` hierarchy.
That `when` has no `UiState.Player` branch (see its own doc comment) and
falls through to `else -> null`, so calling it directly on the *raw*
`_state.value` while paused returns null and the function returns before
`reportClientError` ever runs — the report is silently dropped, but nothing
in the UI shows an error (the "Report a problem" picker still closes
normally), which is exactly the symptom the amendment describes: the
button appears to work but nothing lands on the server.

This is not actually show-specific: a movie paused mid-playback and
reported from *its* pause screen goes through the identical `UiState.Player`
state and would be silently dropped too. It looks like a "shows only" bug
solely because the movie flow the issue's author exercised went through
`MovieDetailScreen` (state = `MovieDetail`, which `embeddedCatalog()` does
handle via `previous.embeddedCatalog()`), while shows have no equivalent
detail screen to fall back to. Every assertion below is written to catch a
fix that is *only* gated on `MediaKind`/episode-ness rather than on
"is the raw state a bare `Player`", because that narrower fix would leave
the movie-pause path broken and still regress the next report surface that
composes on top of `Player`.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
PLAYER = APP / "ui/screens/PlayerScreen.kt"
MAIN_ACTIVITY = APP / "MainActivity.kt"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def read(path: Path) -> str:
    if not path.is_file():
        fail(f"missing {path}")
    return path.read_text()


def match_braces(source: str, open_at: int, opener: str, closer: str) -> int:
    depth = 0
    for index, char in enumerate(source[open_at:], open_at):
        if char == opener:
            depth += 1
        elif char == closer:
            depth -= 1
            if depth == 0:
                return index
    fail(f"unbalanced {opener}{closer} at {open_at}")
    raise AssertionError


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start == -1:
        fail(f"missing `{signature}`")
    brace = source.find("{", start)
    if brace == -1:
        fail(f"`{signature}` has no body")
    end = match_braces(source, brace, "{", "}")
    return source[start : end + 1]


def when_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start == -1:
        fail(f"missing `{signature}`")
    brace = source.find("(", start)
    if brace == -1:
        fail(f"`{signature}` has no `when (...)`")
    open_paren_end = match_braces(source, brace, "(", ")")
    body_brace = source.find("{", open_paren_end)
    if body_brace == -1:
        fail(f"`{signature}` when-expression has no body")
    end = match_braces(source, body_brace, "{", "}")
    return source[body_brace : end + 1]


def assert_embedded_catalog_still_has_no_player_branch() -> None:
    """Documents the root cause: `embeddedCatalog()` falling through
    `Player -> else -> null` is *why* the naive call site broke for the
    pause-screen-only show flow. If a future change adds a `Player` branch
    here directly, the caller-side unwrap this suite checks for becomes
    redundant but harmless; if instead someone "fixes" this by deleting the
    caller-side unwrap while trusting a phantom Player branch that was never
    added, this guard is what catches it."""
    body = when_body(read(VIEW_MODEL), "private fun UiState.embeddedCatalog(): UiState.Catalog? = when")
    if "is UiState.ShowSeasons" not in body:
        fail(
            "embeddedCatalog() no longer resolves UiState.ShowSeasons directly — "
            "that is the state a Player.previous holds for an episode (see Player's "
            "own doc comment), so the pause-screen unwrap this suite requires would "
            "resolve to null for every show even with a correct Player-unwrap"
        )
    if not re.search(r"is\s+UiState\.Player\s*->", body):
        print(
            "ok embeddedCatalog() still has no Player branch (root cause of #354); "
            "reportAssetProblem must unwrap Player itself"
        )
        return
    print("ok embeddedCatalog() now resolves Player directly (still requires ShowSeasons)")


def assert_report_asset_problem_unwraps_player_before_lookup() -> None:
    body = function_body(read(VIEW_MODEL), "fun reportAssetProblem(entry: MergedEntry, category: ProblemReportCategory)")

    # The exact pre-#354-fix regression: calling embeddedCatalog() on the raw
    # state assigned straight from _state.value, with no Player unwrap at all.
    naive_bug_pattern = re.compile(r"\bcurrent\.embeddedCatalog\(\)")
    if naive_bug_pattern.search(body):
        fail(
            "reportAssetProblem calls `current.embeddedCatalog()` directly — this is "
            "the exact #354 regression: embeddedCatalog() has no UiState.Player "
            "branch, so this returns null and drops the report silently whenever "
            "reportAssetProblem is invoked from the pause screen (the *only* report "
            "surface a show has)"
        )

    if "current is UiState.Player" not in body:
        fail(
            "reportAssetProblem never checks `current is UiState.Player` — the pause "
            "screen (the only report surface for a show, and also a valid report "
            "surface for a paused movie) leaves the raw state as UiState.Player, "
            "which embeddedCatalog() cannot resolve without this unwrap"
        )

    # The unwrap must be unconditional on media kind. A fix gated on
    # `entry.entry.kind == MediaKind.EPISODE` (or similar) would repair shows
    # while leaving a movie reported from *its own* pause screen broken, and
    # would break the very next kind of asset (a music track, say) reported
    # from a paused player.
    player_check = re.search(r"if\s*\(\s*current\s+is\s+UiState\.Player\s*\)([\s\S]{0,240})", body)
    if not player_check:
        fail("could not locate the `current is UiState.Player` unwrap condition body")
    guard_window = player_check.group(1)
    if "MediaKind" in guard_window:
        fail(
            "the UiState.Player unwrap is conditioned on MediaKind — this would "
            "silently re-break reporting for any kind not covered (e.g. a paused "
            "movie, or a music track), reproducing the exact false report-with-no-"
            "server-side effect that #354 describes"
        )

    # The unwrap must reach for `.previous`, matching every other Player-aware
    # call site in this file (reportPlaybackRuntimeError, reportServerOffline,
    # recoverExpiredPlaybackSession, ...), not some other property.
    if not re.search(r"current\.previous", body):
        fail(
            "reportAssetProblem's Player-unwrap does not reference `current.previous` "
            "— every other Player-aware call site in SwarmViewModel.kt resolves its "
            "catalog via `current.previous.embeddedCatalog()`; a different unwrap "
            "path is unverified and likely wrong for the Player.previous contract "
            "documented on UiState.Player itself"
        )

    # Order matters: the resolved catalog must actually gate the device lookup
    # and the report call, not just be computed and discarded.
    catalog_assign = re.search(r"val\s+(\w+)\s*=.*embeddedCatalog\(\)\s*\?:\s*return", body)
    if not catalog_assign:
        fail("reportAssetProblem has no `val ... = ...embeddedCatalog() ?: return` guard")
    catalog_var = catalog_assign.group(1)
    device_lookup = re.search(rf"\b{re.escape(catalog_var)}\.devices\.find", body)
    if not device_lookup:
        fail(f"reportAssetProblem never looks up a device from `{catalog_var}`")
    if device_lookup.start() < catalog_assign.start():
        fail("device lookup happens before the catalog is resolved")
    report_call = body.find("reportClientError(")
    if report_call == -1 or report_call < device_lookup.start():
        fail("reportClientError is not called after the device lookup succeeds")

    print("ok reportAssetProblem unwraps a bare UiState.Player via `.previous` before catalog/device lookup, unconditionally on MediaKind")


def assert_unwrap_matches_sibling_player_aware_call_sites() -> None:
    """Cross-check against an already-correct, pre-existing call site so this
    suite is not just trusting the new code's own internal consistency."""
    source = read(VIEW_MODEL)
    sibling = function_body(source, "fun reportPlaybackRuntimeError(message: String, context: String? = null)")
    if "current.previous.embeddedCatalog()" not in sibling:
        fail(
            "reportPlaybackRuntimeError (the established, working Player-aware "
            "pattern) no longer resolves via current.previous.embeddedCatalog() — "
            "the reference pattern this suite checks reportAssetProblem against has "
            "itself drifted, so the comparison is no longer meaningful"
        )
    print("ok sibling Player-aware call site (reportPlaybackRuntimeError) still uses current.previous.embeddedCatalog() as the reference pattern")


def assert_pause_overlay_reports_the_live_playing_entry_not_a_stale_one() -> None:
    """The device/catalog fix is moot if the picker on the pause screen sends
    the wrong asset. Confirm the entry threaded into onReportProblem from the
    pause screen is the actually-playing entry, not e.g. a recommendation
    card or a cached reference, since a "report succeeded" toast for the
    wrong entry_key would be just as invisible a failure on the server side
    as a dropped report."""
    source = read(PLAYER)
    overlay = function_body(source, "private fun PauseOverlay(")
    signature = overlay.split("{", 1)[0]
    if not re.search(r"\bentry\s*:\s*MergedEntry\b", signature):
        fail("PauseOverlay no longer takes `entry: MergedEntry` as a direct parameter")
    if "onReportProblem(entry, category)" not in overlay:
        fail("pause overlay's picker selection no longer reports the PauseOverlay `entry` parameter itself")
    # Reject the specific class of stale-entry bug: reporting a recommendation
    # card's entry instead of the entry actually being paused/played.
    picker_idx = overlay.find("ProblemReportPicker(")
    picker_call = overlay[picker_idx : picker_idx + 400]
    if re.search(r"onReportProblem\(\s*recommendation\s*,", picker_call):
        fail("pause overlay reports a recommendation card's entry instead of the entry actually playing")

    player_fn = function_body(source, "fun PlayerScreen(")
    pause_overlay_call = player_fn[player_fn.find("PauseOverlay(") :]
    pause_overlay_call = pause_overlay_call[: pause_overlay_call.find("\n\n") if pause_overlay_call.find("\n\n") != -1 else 600]
    if not re.search(r"\bentry\s*=\s*entry\b", pause_overlay_call):
        fail("PlayerScreen does not forward its own `entry` parameter into PauseOverlay's `entry`")

    main = read(MAIN_ACTIVITY)
    player_match = re.search(r"(?<![A-Za-z])PlayerScreen\(", main)
    if not player_match:
        fail("MainActivity does not compose PlayerScreen")
    start = player_match.start()
    end = match_braces(main, main.find("(", start), "(", ")")
    call = main[start : end + 1]
    if not re.search(r"entry\s*=\s*state\.entry\b", call):
        fail(
            "MainActivity does not bind PlayerScreen's `entry` to `state.entry` — "
            "the live UiState.Player.entry — so the pause screen could report a "
            "stale or wrong asset even if the catalog/device resolution is fixed"
        )
    print("ok pause screen's report picker sends the live UiState.Player.entry end-to-end (MainActivity -> PlayerScreen -> PauseOverlay -> reportAssetProblem)")


def main() -> None:
    assert_embedded_catalog_still_has_no_player_branch()
    assert_report_asset_problem_unwraps_player_before_lookup()
    assert_unwrap_matches_sibling_player_aware_call_sites()
    assert_pause_overlay_reports_the_live_playing_entry_not_a_stale_one()
    print("PASS: Issue #354 show pause-screen report delivery (adversarial)")


if __name__ == "__main__":
    main()
