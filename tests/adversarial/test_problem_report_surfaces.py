#!/usr/bin/env python3
"""Issue #354: Report-a-problem popup + pause-screen report for shows.

Expected behavior, derived from the issue before trusting the diff:

1. Selecting "Report a problem" opens a popup of the six issue categories.
   It must not send a report until a category is chosen.
2. Physical Back dismisses that popup without sending a report.
3. Choosing a category attaches that label to the payload the media server
   stores, together with the asset being viewed.
4. A show currently has no detail-page report control; the pause screen of
   an in-progress episode is the path that must expose "Report a problem".
   That control must still be present when there is no Next Episode button
   (single-episode shows / last episode).
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
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
ACTIVITY = APP / "MainActivity.kt"
TAGS = APP / "ui/UatTestTags.kt"
SEASON = APP / "ui/screens/SeasonScreen.kt"

ISSUE_LABELS = [
    "Playback Video",
    "Playback Audio",
    "Artwork",
    "Content",
    "Language",
    "Subtitle",
]
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


def block_containing(source: str, needle: str, kind: str = "Button(") -> str:
    pos = source.find(needle)
    if pos == -1:
        fail(f"could not find {needle!r}")
    start = source.rfind(kind, 0, pos)
    if start == -1:
        fail(f"{needle!r} is not inside a {kind}...)")
    return compose_call(source, start)


def innermost_if_around(source: str, needle: str) -> str | None:
    pos = source.find(needle)
    if pos == -1:
        fail(f"could not find {needle!r}")
    best: str | None = None
    search_from = 0
    while True:
        match = re.search(r"\bif\s*\(", source[search_from:pos])
        if not match:
            break
        abs_start = search_from + match.start()
        paren = source.find("(", abs_start)
        after_cond = match_braces(source, paren, "(", ")") + 1
        brace = source.find("{", after_cond)
        if brace == -1 or brace > pos:
            search_from = abs_start + 1
            continue
        end = match_braces(source, brace, "{", "}")
        if abs_start < pos <= end:
            best = source[abs_start : end + 1]
        search_from = abs_start + 1
    return best


def assert_picker_lists_issue_categories() -> None:
    picker = read(PICKER)
    if "ProblemReportCategory.entries" not in picker:
        fail("ProblemReportPicker does not iterate the production category enum")
    if "Text(category.label" not in picker.replace(" ", ""):
        # Allow typical Compose call shape Text(category.label, ...)
        if "Text(category.label" not in picker:
            fail("picker options are not the viewer-facing category labels")
    if "UatTestTags.PROBLEM_REPORT_PICKER" not in picker:
        fail("picker is missing a stable UAT tag")
    if "UatTestTags.PROBLEM_REPORT_OPTION_PREFIX" not in picker:
        fail("picker options are missing stable UAT tags")
    if "BackHandler(onBack = onDismiss)" not in picker and "BackHandler(onBack=onDismiss)" not in picker.replace(
        " ", ""
    ):
        fail("picker does not handle physical Back as dismiss-without-send")
    if "onReport(category)" not in picker:
        fail("picker does not send the selected category")
    if "onReportProblem" in picker:
        fail("picker must not report by itself; the host sends after a selection")
    if "firstOptionFocusRequester.requestFocus()" not in picker:
        fail("picker does not land D-pad focus on the first category when it opens")
    print("ok picker lists enum categories, tags them, and Back dismisses")


def assert_tags_cover_picker_and_pause() -> None:
    tags = read(TAGS)
    for const in (
        "MOVIE_DETAIL_REPORT_PROBLEM_BUTTON",
        "PROBLEM_REPORT_PICKER",
        "PROBLEM_REPORT_OPTION_PREFIX",
        "PAUSE_REPORT_PROBLEM_BUTTON",
    ):
        if f"const val {const}" not in tags:
            fail(f"UatTestTags is missing {const}")
    print("ok UAT tags exist for movie detail, picker, and pause report")


def assert_movie_detail_opens_picker_without_sending() -> None:
    source = read(MOVIE_DETAIL)
    button = block_containing(source, "UatTestTags.MOVIE_DETAIL_REPORT_PROBLEM_BUTTON")
    if "Report a problem" not in button:
        fail("movie detail is missing the Report a problem control")
    if "showProblemPicker = true" not in button:
        fail("movie detail Report a problem does not open the category popup")
    if "onReportProblem" in button:
        fail("movie detail Report a problem still sends a report before a category is chosen")

    picker_call = source[source.find("ProblemReportPicker(") :]
    if "ProblemReportPicker(" not in source:
        fail("movie detail never shows ProblemReportPicker")
    if "onReportProblem(entry, category)" not in picker_call[:800]:
        fail("movie detail picker selection does not report the chosen category for this asset")
    dismiss = re.search(r"onDismiss\s*=\s*\{([^}]*)\}", picker_call[:800])
    if not dismiss:
        fail("movie detail picker has no onDismiss handler")
    if "onReportProblem" in dismiss.group(1):
        fail("dismissing the movie detail picker still sends a report")
    if "showProblemPicker = false" not in dismiss.group(1):
        fail("dismissing the movie detail picker does not close it")
    if "if (showProblemPicker)" not in source:
        fail("movie detail picker is not gated on an explicit open flag")
    print("ok movie detail opens a picker and only reports after a category is chosen")


def assert_pause_screen_reports_for_shows() -> None:
    source = read(PLAYER)
    overlay = function_body(source, "private fun PauseOverlay(")
    if "UatTestTags.PAUSE_REPORT_PROBLEM_BUTTON" not in overlay:
        fail("pause overlay has no Report a problem button")
    button = block_containing(overlay, "UatTestTags.PAUSE_REPORT_PROBLEM_BUTTON")
    if "Report a problem" not in button:
        fail("pause overlay button is not labeled Report a problem")
    if "showProblemPicker = true" not in button:
        fail("pause Report a problem does not open the category popup")
    if "onReportProblem" in button:
        fail("pause Report a problem still sends a report before a category is chosen")

    next_episode = innermost_if_around(overlay, "UatTestTags.PAUSE_NEXT_EPISODE_BUTTON")
    if next_episode and "PAUSE_REPORT_PROBLEM_BUTTON" in next_episode:
        fail("pause Report a problem is nested under Next Episode and would vanish on the last episode")

    kind_gate = innermost_if_around(overlay, "UatTestTags.PAUSE_REPORT_PROBLEM_BUTTON")
    if kind_gate and "MediaKind.MOVIE" in kind_gate and "EPISODE" not in kind_gate:
        fail("pause Report a problem is movie-only; shows still cannot report")

    player = function_body(source, "fun PlayerScreen(")
    if "PauseOverlay(" not in player:
        fail("PlayerScreen never composes PauseOverlay")
    if "onReportProblem = onReportProblem" not in player:
        fail("PlayerScreen does not wire the pause overlay report callback")
    if re.search(
        r"if\s*\([^)]*MediaKind\.MOVIE[^)]*\)[\s\S]{0,200}PauseOverlay\(",
        player,
    ):
        fail("PauseOverlay is gated to movies, so a show still cannot report from pause")

    picker_idx = overlay.find("ProblemReportPicker(")
    if picker_idx == -1:
        fail("pause overlay never shows ProblemReportPicker")
    picker_call = overlay[picker_idx : picker_idx + 700]
    if "onReportProblem(entry, category)" not in picker_call:
        fail("pause picker selection does not report the chosen category for the playing asset")
    dismiss = re.search(r"onDismiss\s*=\s*\{([^}]*)\}", picker_call)
    if not dismiss:
        fail("pause picker has no onDismiss handler")
    if "onReportProblem" in dismiss.group(1):
        fail("dismissing the pause picker still sends a report")
    print("ok pause overlay exposes Report a problem for shows, including without Next Episode")


def assert_activity_wires_player_report() -> None:
    source = read(ACTIVITY)
    player_match = re.search(r"(?<![A-Za-z])PlayerScreen\(", source)
    if not player_match:
        fail("MainActivity does not compose PlayerScreen")
    window = compose_call(source, player_match.start())
    if "onReportProblem = onReportProblem" not in window:
        fail("MainActivity does not pass the report callback into PlayerScreen")
    movie_match = re.search(r"(?<![A-Za-z])MovieDetailScreen\(", source)
    if not movie_match:
        fail("MainActivity does not compose MovieDetailScreen")
    movie_call = compose_call(source, movie_match.start())
    if "onReportProblem = onReportProblem" not in movie_call:
        fail("MainActivity does not pass the report callback into MovieDetailScreen")
    print("ok MainActivity wires report callbacks into movie detail and the player")


def assert_server_payload_includes_selected_category() -> None:
    source = read(VIEW_MODEL)
    body = function_body(source, "fun reportAssetProblem(")
    if "category: ProblemReportCategory" not in body.split("{", 1)[0]:
        fail("reportAssetProblem no longer takes the selected issue category")
    if "category.reportMessage(" not in body:
        fail("reportAssetProblem does not put the selected category into the server message")
    if not re.search(r"reportClientError\s*\(", body):
        fail("reportAssetProblem never calls reportClientError")
    call = body[body.find("reportClientError(") :]
    header = call[:500]
    if "message = category.reportMessage(" not in header:
        fail("the client-error payload message is not the selected category")
    if "entry = entry" not in header:
        fail("the client-error payload dropped the asset the viewer was reporting")
    print("ok selected category and asset are attached to the media-server report")


def assert_season_screen_is_not_the_required_path() -> None:
    # The issue's chosen path is the pause screen, because season/show detail
    # currently has no report control. Do not require a season-screen button.
    season = read(SEASON)
    if "Report a problem" in season:
        print("ok season screen unexpectedly has a report control (allowed extra)")
    else:
        print("ok season screen still has no report control; pause is the show path")


def main() -> None:
    assert_picker_lists_issue_categories()
    assert_tags_cover_picker_and_pause()
    assert_movie_detail_opens_picker_without_sending()
    assert_pause_screen_reports_for_shows()
    assert_activity_wires_player_report()
    assert_server_payload_includes_selected_category()
    assert_season_screen_is_not_the_required_path()
    print("PASS: Issue #354 problem-report surface UAT contract")


if __name__ == "__main__":
    main()
