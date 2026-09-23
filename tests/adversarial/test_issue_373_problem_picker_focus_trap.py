#!/usr/bin/env python3
"""Issue #373: Report-a-problem popup must disable D-pad on covered controls.

Expected behavior, derived from the issue and TV overlay invariants
before treating the delivery as authoritative:

1. Opening Report a problem from movie detail or the video pause overlay
   is a modal overlay. D-pad must not leave the picker onto dimmed
   controls after the last category (Subtitle).
2. The established TV pattern is CatalogScreen's search overlay: the
   covered tree sets focusProperties { canFocus = false } while the
   overlay is open, and restores focusability when it closes.
3. The picker itself must stay focusable. Disabling canFocus on an
   ancestor that also contains the picker would trap nothing and
   strand the remote.
4. Covered pause controls include Resume, Next Episode, Report a
   problem, audio/subtitle track pickers, and More like this.
   Covered movie-detail controls include Play, Like, Watchlist,
   Report a problem, and extras.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
PICKER = APP / "ui/components/ProblemReportPicker.kt"
MOVIE_DETAIL = APP / "ui/screens/MovieDetailScreen.kt"
PLAYER = APP / "ui/screens/PlayerScreen.kt"
CATALOG = APP / "ui/screens/CatalogScreen.kt"

COVERED_MOVIE_TAGS = (
    "MOVIE_DETAIL_PLAY_BUTTON",
    "MOVIE_DETAIL_LIKE_BUTTON",
    "MOVIE_DETAIL_WATCHLIST_BUTTON",
    "MOVIE_DETAIL_REPORT_PROBLEM_BUTTON",
)
COVERED_PAUSE_TAGS = (
    "PAUSE_RESUME_BUTTON",
    "PAUSE_REPORT_PROBLEM_BUTTON",
    "PAUSE_AUDIO_TRACK_PICKER",
    "PAUSE_SUBTITLE_TRACK_PICKER",
)


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


def compose_call(source: str, start: int) -> str:
    index = start
    while index < len(source) and (source[index].isalnum() or source[index] == "_"):
        index += 1
    while index < len(source) and source[index].isspace():
        index += 1
    if index < len(source) and source[index] == "(":
        index = match_braces(source, index, "(", ")") + 1
    while index < len(source) and source[index].isspace():
        index += 1
    if index < len(source) and source[index] == "{":
        index = match_braces(source, index, "{", "}") + 1
    return source[start:index]


def assert_catalog_search_establishes_overlay_focus_trap() -> None:
    source = read(CATALOG)
    if ".focusProperties { canFocus = !searchOpen }" not in source.replace("\n", " "):
        if "canFocus = !searchOpen" not in source:
            fail("CatalogScreen no longer traps D-pad under the search overlay")
    print("ok CatalogScreen still disables canFocus on content under search")


def _gated_column(host: str, host_name: str) -> tuple[str, str]:
    picker_idx = host.find("ProblemReportPicker(")
    if picker_idx == -1:
        fail(f"{host_name} never composes ProblemReportPicker")
    if "if (showProblemPicker)" not in host:
        fail(f"{host_name} picker is not gated on showProblemPicker")

    # The picker must be a sibling overlay of the covered content, not a
    # child of the canFocus=false tree.
    before_picker = host[:picker_idx]
    if "canFocus" in compose_call(host, picker_idx)[:400]:
        fail(f"{host_name} ProblemReportPicker itself disables canFocus")

    gate = re.search(
        r"focusProperties\s*\{[^}]*canFocus\s*=\s*!showProblemPicker[^}]*\}",
        host,
        re.DOTALL,
    )
    if not gate:
        fail(
            f"{host_name} does not set focusProperties {{ canFocus = !showProblemPicker }} "
            "on the tree covered by the report picker"
        )

    # Locate the Column/Box that carries that modifier and extract its body.
    modifier_at = gate.start()
    column_at = host.rfind("Column(", 0, modifier_at)
    if column_at == -1:
        fail(f"{host_name} focus gate is not on a Column covering the dimmed controls")
    gated = compose_call(host, column_at)
    if "ProblemReportPicker(" in gated:
        fail(
            f"{host_name} nests ProblemReportPicker inside the canFocus=false column, "
            "which would also disable the category buttons"
        )
    if picker_idx < column_at:
        fail(f"{host_name} composes the picker before the covered content column")
    return gated, before_picker


def assert_movie_detail_traps_focus_under_picker() -> None:
    host = function_body(read(MOVIE_DETAIL), "fun MovieDetailScreen(")
    gated, _ = _gated_column(host, "MovieDetailScreen")
    for tag in COVERED_MOVIE_TAGS:
        if tag not in gated:
            fail(f"movie-detail control {tag} is outside the picker focus trap")
    if "movie-extra-" not in gated:
        fail("movie extras row is outside the picker focus trap")
    if "canFocus = true" in gated and "canFocus = !showProblemPicker" not in gated:
        fail("movie detail hard-codes canFocus = true under the picker")
    print("ok movie detail disables D-pad on covered controls while the picker is open")


def assert_pause_overlay_traps_focus_under_picker() -> None:
    host = function_body(read(PLAYER), "private fun PauseOverlay(")
    gated, _ = _gated_column(host, "PauseOverlay")
    for tag in COVERED_PAUSE_TAGS:
        if tag not in gated:
            fail(f"pause control {tag} is outside the picker focus trap")
    if "PAUSE_NEXT_EPISODE_BUTTON" not in gated:
        fail("Next Episode is outside the picker focus trap when it is shown")
    if "PauseRecommendationCard" not in gated:
        fail("More like this recommendations are outside the picker focus trap")
    print("ok pause overlay disables D-pad on covered controls while the picker is open")


def assert_picker_stays_focusable_and_lands_on_first_option() -> None:
    picker = read(PICKER)
    if "firstOptionFocusRequester.requestFocus()" not in picker:
        fail("picker does not request focus on the first category when it opens")
    if "canFocus = false" in picker:
        fail("ProblemReportPicker disables canFocus on its own tree")
    if "BackHandler(onBack = onDismiss)" not in picker and "BackHandler(onBack=onDismiss)" not in picker.replace(
        " ", ""
    ):
        fail("picker no longer dismisses on physical Back")
    print("ok picker remains focusable and still lands on the first category")


def assert_closed_picker_restores_host_focus() -> None:
    movie = function_body(read(MOVIE_DETAIL), "fun MovieDetailScreen(")
    pause = function_body(read(PLAYER), "private fun PauseOverlay(")
    for name, host in (("MovieDetailScreen", movie), ("PauseOverlay", pause)):
        if "canFocus = !showProblemPicker" not in host:
            fail(f"{name} does not restore canFocus when showProblemPicker is false")
        # A one-way canFocus = false with no restoration would strand the
        # detail/pause remote after dismiss.
        if re.search(r"canFocus\s*=\s*false\s*(?!\s*\))", host) and "canFocus = !showProblemPicker" not in host:
            fail(f"{name} permanently disables canFocus instead of tying it to the picker")
    print("ok hosts restore D-pad on covered controls after the picker closes")


def main() -> None:
    assert_catalog_search_establishes_overlay_focus_trap()
    assert_movie_detail_traps_focus_under_picker()
    assert_pause_overlay_traps_focus_under_picker()
    assert_picker_stays_focusable_and_lands_on_first_option()
    assert_closed_picker_restores_host_focus()
    print("PASS: Issue #373 problem-report picker D-pad focus trap")


if __name__ == "__main__":
    main()
