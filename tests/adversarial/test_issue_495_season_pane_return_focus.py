#!/usr/bin/env python3
"""Adversarial UAT for issue #495: D-pad Left from the episode pane back to the
left season/specials list must land on (and scroll into view) the active season
rather than jumping to Season 1 and overwriting the displayed episode list.

Issue #495 (verbatim requirement):
  "I want it to view and navigate like Netflix does."

Defect probed:
  When returning from `PlayerScreen` to `SeasonScreen` on Season 2+, an
  off-screen Season 10, or Specials, `UiState.ShowSeasons.selectedSeason` is
  non-null (`inEpisodes = true`), so `SeasonScreen` mounts fresh with initial
  focus placed on `Episode 1` in `EpisodePane` while `SeasonPane` has never held
  focus in this mount:
  1. `LaunchedEffect(inEpisodes, show.show)` gates `seasonListState.scrollToItem`
     behind `if (!inEpisodes)`, leaving `seasonListState` stuck at index 0 so a
     season past the ~6-row viewport (e.g. Season 10 or Specials on a multi-season
     show) is off-screen on the left and `highlightedSeasonRequester` is not
     attached to any composed `SeasonRow`.
  2. `SeasonPane`'s `LazyColumn` uses bare `.focusRestorer()` (`fallback =
     FocusRequester.Default`). Because `SeasonPane` has no saved child on a
     post-playback mount, pressing D-pad Left from `Episode 1` falls back to
     Compose's 2D focus search (`TwoDimensionalFocusSearchKt`), which selects
     `Season 1` (at y = 200..244dp, overlapping `Episode 1` at y = 71..204dp),
     triggering `SeasonRow(1).onFocused` -> `onHighlightSeason(Season 1)` and
     replacing `selectedSeason`'s episodes with Season 1's episodes.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SEASON_SCREEN = (
    ROOT
    / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui/screens/SeasonScreen.kt"
)
CORE_SRC = ROOT / "clients/tv-android/core/src/main/kotlin/app/swarm/tv/core"
CORE_FILES = [
    CORE_SRC / "capability/Contracts.kt",
    CORE_SRC / "peer/Contracts.kt",
    CORE_SRC / "catalog/CatalogMerger.kt",
    CORE_SRC / "catalog/CatalogGrouping.kt",
    CORE_SRC / "watch/WatchState.kt",
]
DRIVER = Path(__file__).with_name("Issue495TwoPaneFocusDriver.kt")


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)


def match_braces(source: str, open_at: int, opener: str = "{", closer: str = "}") -> int:
    depth = 0
    for idx in range(open_at, len(source)):
        ch = source[idx]
        if ch == opener:
            depth += 1
        elif ch == closer:
            depth -= 1
            if depth == 0:
                return idx
    fail(f"unbalanced {opener}{closer} starting at {open_at}")
    raise AssertionError


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start == -1:
        fail(f"missing `{signature}` in SeasonScreen.kt")
    brace = source.find("{", start)
    if brace == -1:
        fail(f"`{signature}` has no body")
    return source[start : match_braces(source, brace, "{", "}") + 1]


def extract_pure_helpers(source: str) -> str:
    marker = "internal fun resumeEpisode("
    start = source.find(marker)
    if start == -1:
        fail("SeasonScreen.kt is missing `internal fun resumeEpisode(`")
    tail = source[start:].replace("\nprivate fun ", "\ninternal fun ")
    return (
        "package app.swarm.tv.app.ui.screens\n\n"
        "import app.swarm.tv.core.catalog.CatalogGrouping\n"
        "import app.swarm.tv.core.catalog.MergedEntry\n"
        "import app.swarm.tv.core.catalog.SeasonGroup\n"
        "import app.swarm.tv.core.catalog.ShowGroup\n"
        "import app.swarm.tv.core.watch.WatchState\n"
        "import app.swarm.tv.core.watch.stateFor\n\n"
        + tail
        + "\n"
    )


def detect_season_pane_return_redirect(source: str) -> bool:
    """Returns True if D-pad Left from EpisodePane into an unvisited SeasonPane
    redirects focus to `highlightedSeasonRequester` (or calls `onSelectSeason(null)`,
    which triggers `SeasonScreen`'s LaunchedEffect to scroll and focus
    `highlightedSeasonRequester`)."""
    season_pane = function_body(source, "private fun SeasonPane(")
    episode_pane = function_body(source, "private fun EpisodePane(")
    episode_row = (
        function_body(source, "private fun EpisodeRow(")
        if "private fun EpisodeRow(" in source
        else ""
    )

    # Mechanism 1: SeasonPane's focusRestorer passes a fallback referencing highlightedSeasonRequester.
    for match in re.finditer(r"focusRestorer\s*(\(|\{)", season_pane):
        opener = match.group(1)
        closer = ")" if opener == "(" else "}"
        arg_block = season_pane[
            match.start(1) : match_braces(season_pane, match.start(1), opener, closer) + 1
        ]
        inner = arg_block[1:-1].strip()
        if inner and inner != "FocusRequester.Default":
            if "highlightedSeasonRequester" in inner:
                return True
            for val_match in re.finditer(r"val\s+(\w+)\s*=\s*([^\n]+)", season_pane):
                name, expr = val_match.group(1), val_match.group(2)
                if name in inner and "highlightedSeasonRequester" in expr:
                    return True

    # Mechanism 2: focusProperties enter / onEnter / left targeting highlightedSeasonRequester
    for block in (season_pane, episode_pane, episode_row, source):
        for fp in re.finditer(r"focusProperties\s*\{", block):
            fp_body = block[fp.end() - 1 : match_braces(block, fp.end() - 1, "{", "}") + 1]
            if (
                any(k in fp_body for k in ("onEnter", "enter", "left"))
                and ("highlightedSeasonRequester" in fp_body or "onSelectSeason(null)" in fp_body)
            ):
                return True

    # Mechanism 3: explicit Key.DirectionLeft handling in EpisodePane / EpisodeRow
    for block in (episode_pane, episode_row, source):
        if "DirectionLeft" in block and (
            "highlightedSeasonRequester" in block or "onSelectSeason(null)" in block
        ):
            return True

    return False


def detect_scrolls_season_list_when_in_episodes(source: str) -> bool:
    """Returns True if `seasonListState` is positioned/scrolled to the displayed
    season on mount even when `inEpisodes` is true (e.g. returning from PlayerScreen
    with `selectedSeason != null`), or if DirectionLeft explicitly invokes
    `onSelectSeason(null)` so the `!inEpisodes` scroll-and-focus effect runs."""
    screen = function_body(source, "fun SeasonScreen(")

    # Check rememberLazyListState(initialFirstVisibleItemIndex = ...) with non-zero initial index
    state_match = re.search(r"rememberLazyListState\s*\(([^)]*)\)", screen)
    if state_match and state_match.group(1).strip():
        return True

    # Check every `seasonListState.scrollToItem(` call in SeasonScreen: is at least one
    # reachable when `inEpisodes == true` (i.e. not gated inside `if (!inEpisodes)`)?
    for call in re.finditer(r"seasonListState\.(?:animateScrollToItem|scrollToItem)\s*\(", screen):
        pos = call.start()
        gated_not_in_episodes = False
        for guard in re.finditer(r"if\s*\(([^)]*)\)\s*\{", screen[:pos]):
            cond = guard.group(1)
            brace_open = screen.find("{", guard.start())
            brace_close = match_braces(screen, brace_open, "{", "}")
            if brace_open < pos < brace_close and (
                "!inEpisodes" in cond or "selectedSeason == null" in cond
            ):
                gated_not_in_episodes = True
                break
        if not gated_not_in_episodes:
            return True

    return False


def jars_named(name: str) -> list[str]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return [
        str(p)
        for p in sorted(cache.glob(f"**/{name}-*.jar"))
        if "sources" not in p.name and "javadoc" not in p.name
    ]


def aars_under(coord: str) -> list[Path]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return sorted(cache.glob(f"{coord}/**/*.aar"))


def kotlin_compiler() -> list[str]:
    embedded = jars_named("kotlin-compiler-embeddable")
    if not embedded:
        fail("kotlin-compiler-embeddable is absent from Gradle cache")
    extras: list[str] = []
    for name in (
        "kotlin-stdlib",
        "kotlin-reflect",
        "kotlin-script-runtime",
        "kotlinx-coroutines-core-jvm",
        "annotations",
        "trove4j",
    ):
        extras.extend(jars_named(name)[:3])
    return [
        "java",
        "-cp",
        ":".join([embedded[-1], *extras]),
        "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
    ]


def main() -> None:
    if not SEASON_SCREEN.is_file():
        fail(f"missing {SEASON_SCREEN}")
    source = SEASON_SCREEN.read_text()

    has_return_redirect = detect_season_pane_return_redirect(source)
    scrolls_when_in_episodes = detect_scrolls_season_list_when_in_episodes(source)

    helpers_src = extract_pure_helpers(source)
    ui_aars = aars_under("androidx.compose.ui/ui-android")
    rt_aars = aars_under("androidx.compose.runtime/runtime-android")
    if not ui_aars or not rt_aars:
        fail("Compose ui-android / runtime-android AARs missing from Gradle cache")

    aars = [ui_aars[-1], rt_aars[-1]]
    for opt in ("androidx.compose.ui/ui-geometry-android", "androidx.compose.ui/ui-unit-android"):
        found = aars_under(opt)
        if found:
            aars.append(found[-1])

    with tempfile.TemporaryDirectory() as temp:
        temp_path = Path(temp)
        cp_jars: list[str] = []
        for idx, aar in enumerate(aars):
            sub = temp_path / f"aar_{idx}"
            sub.mkdir()
            with zipfile.ZipFile(aar) as zf:
                zf.extract("classes.jar", sub)
            cp_jars.append(str(sub / "classes.jar"))
        for jar_name in (
            "ui-geometry-jvm",
            "ui-unit-jvm",
            "collection-jvm",
            "kotlin-stdlib",
            "kotlinx-serialization-core-jvm",
            "kotlinx-serialization-json-jvm",
            "kotlinx-coroutines-core-jvm",
            "annotations",
        ):
            cp_jars.extend(jars_named(jar_name)[:3])
        classpath = ":".join(dict.fromkeys(cp_jars))

        helpers_file = temp_path / "SeasonScreenHelpers.kt"
        helpers_file.write_text(helpers_src)
        out_dir = temp_path / "classes"
        out_dir.mkdir()

        env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}
        compile_res = subprocess.run(
            kotlin_compiler()
            + [
                "-no-stdlib",
                "-no-reflect",
                "-cp",
                classpath,
                "-d",
                str(out_dir),
                *[str(p) for p in CORE_FILES],
                str(helpers_file),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
            env=env,
        )
        if compile_res.returncode:
            fail(
                "Driver compilation failed:\n"
                f"{compile_res.stdout}\n{compile_res.stderr}"
            )

        run_res = subprocess.run(
            [
                "java",
                "-cp",
                f"{out_dir}:{classpath}",
                "app.swarm.tv.app.ui.screens.Issue495TwoPaneFocusDriverKt",
                "--mode=season-return",
                f"--season-pane-has-return-redirect={'true' if has_return_redirect else 'false'}",
                f"--scrolls-season-list-when-in-episodes={'true' if scrolls_when_in_episodes else 'false'}",
            ],
            capture_output=True,
            text=True,
            timeout=120,
            env=env,
        )
        if run_res.stdout.strip():
            print(run_res.stdout.strip())
        if run_res.returncode or "ALL_OK" not in run_res.stdout:
            fail(
                "SeasonPane D-pad Left return focus / scroll failed after returning from playback "
                "with `selectedSeason != null`:\n"
                f"{run_res.stderr.strip()}"
            )

    print("PASS: Issue #495 D-pad Left from EpisodePane preserves and scrolls to the active season")


if __name__ == "__main__":
    main()
