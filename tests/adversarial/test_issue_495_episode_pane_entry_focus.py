#!/usr/bin/env python3
"""Adversarial UAT for issue #495: D-pad Right into the right-hand episode pane
must enter on the season's first episode instead of skipping past Episode 1.

Issue #495 (verbatim requirement):
  "For the TV apps i want the episode and shows to view differently. I want it
   to view and navigate like Netflix does. See attachment. Seasons and specials
   list on left and episodes show on right."

Defect probed:
  In `SeasonScreen.kt`, `EpisodePane` wraps each season's `LazyColumn` in
  `key(seasonKey(season))` with bare `.focusRestorer()` (defaulting `fallback`
  to `FocusRequester.Default`). When a viewer highlights a season or Specials
  on the left and presses D-pad Right to enter that season's episodes on the
  right, `FocusRestorerNode.onEnter` finds no previously focused child in that
  fresh `LazyColumn`, sees `fallback == FocusRequester.Default`, and falls back
  to Compose's geometric 2D focus search (`TwoDimensionalFocusSearchKt`).
  Because `SeasonPane`'s poster header and action buttons push every season row
  down to y >= 200dp while `Episode 1` sits at y = 71..204dp and `Episode 2` at
  y = 214..347dp, Compose's 2D search skips `Episode 1` even from `Season 1`
  (landing on `Episode 2` for Seasons 1-3 and `Episode 3` for Season 4 /
  Specials).
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
    tail = source[start:]
    # Widen file-private pure helpers so the driver in the same package can exercise them directly.
    tail = tail.replace("\nprivate fun ", "\ninternal fun ")
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


def detect_episode_pane_first_entry_redirect(source: str) -> bool:
    """Returns True if SeasonScreen routes first-time D-pad Right entry into
    EpisodePane to `firstEpisodeRequester` (via focusRestorer fallback,
    focusProperties custom enter/right, or explicit DirectionRight key handling)
    rather than leaving bare `.focusRestorer()` with `FocusRequester.Default`."""
    episode_pane = function_body(source, "private fun EpisodePane(")
    season_pane = function_body(source, "private fun SeasonPane(")
    season_row = (
        function_body(source, "private fun SeasonRow(")
        if "private fun SeasonRow(" in source
        else ""
    )

    # Mechanism 1: focusRestorer(firstEpisodeRequester) or focusRestorer { ... firstEpisodeRequester ... }
    for match in re.finditer(r"focusRestorer\s*(\(|\{)", episode_pane):
        opener = match.group(1)
        closer = ")" if opener == "(" else "}"
        arg_block = episode_pane[
            match.start(1) : match_braces(episode_pane, match.start(1), opener, closer) + 1
        ]
        # Must reference firstEpisodeRequester (or a local derived from it), not empty `()`.
        inner = arg_block[1:-1].strip()
        if inner and inner != "FocusRequester.Default":
            # Allow either direct `firstEpisodeRequester` or a local val initialized from `firstEpisodeRequester`.
            if "firstEpisodeRequester" in inner:
                return True
            for val_match in re.finditer(r"val\s+(\w+)\s*=\s*([^\n]+)", episode_pane):
                name, expr = val_match.group(1), val_match.group(2)
                if name in inner and "firstEpisodeRequester" in expr:
                    return True

    # Mechanism 2: explicit focusProperties enter / onEnter / right targeting firstEpisodeRequester
    for block in (episode_pane, season_pane, season_row, source):
        for fp in re.finditer(r"focusProperties\s*\{", block):
            fp_body = block[fp.end() - 1 : match_braces(block, fp.end() - 1, "{", "}") + 1]
            if (
                any(k in fp_body for k in ("onEnter", "enter", "right"))
                and ("firstEpisodeRequester" in fp_body or "onOpenSeason" in fp_body)
            ):
                return True

    # Mechanism 3: explicit Key.DirectionRight key handling in SeasonRow / SeasonPane
    for block in (season_pane, season_row):
        if "DirectionRight" in block and (
            "onOpenSeason" in block or "firstEpisodeRequester" in block
        ):
            return True

    return False


def assert_empty_season_fallback_is_safe(source: str) -> None:
    """If EpisodePane passes `firstEpisodeRequester` directly to `focusRestorer(...)`
    without guarding `episodes.isEmpty() && extras.isEmpty()` (or skipping the
    focus group when empty), an empty SeasonGroup could invoke an unattached
    FocusRequester."""
    episode_pane = function_body(source, "private fun EpisodePane(")
    for match in re.finditer(r"focusRestorer\s*(\(|\{)", episode_pane):
        opener = match.group(1)
        closer = ")" if opener == "(" else "}"
        arg_block = episode_pane[
            match.start(1) : match_braces(episode_pane, match.start(1), opener, closer) + 1
        ]
        inner = arg_block[1:-1].strip()
        if inner == "firstEpisodeRequester":
            # Acceptable if EpisodePane only renders the LazyColumn when non-empty.
            lazy_at = episode_pane.find("LazyColumn(")
            if lazy_at != -1 and "isNotEmpty()" not in episode_pane[:lazy_at]:
                fail(
                    "EpisodePane passes an unguarded `firstEpisodeRequester` to `focusRestorer(...)` "
                    "even when both `episodes` and `extras` are empty, which throws IllegalStateException "
                    "if FocusRestorerNode.onEnter fires on an empty season"
                )


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

    # Structural two-pane check: left SeasonPane and right EpisodePane exist side-by-side in a Row.
    screen_body = function_body(source, "fun SeasonScreen(")
    if "SeasonPane(" not in screen_body or "EpisodePane(" not in screen_body:
        fail("SeasonScreen must render SeasonPane (left) and EpisodePane (right) side-by-side")
    if screen_body.find("SeasonPane(") > screen_body.find("EpisodePane("):
        fail("SeasonPane (seasons and specials) must be on the left before EpisodePane (episodes) on the right")

    has_redirect = detect_episode_pane_first_entry_redirect(source)
    if has_redirect:
        assert_empty_season_fallback_is_safe(source)

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
                "--mode=episode-entry",
                f"--episode-pane-has-first-entry-redirect={'true' if has_redirect else 'false'}",
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
                "EpisodePane D-pad Right first-entry focus failed (bare `.focusRestorer()` "
                "without `firstEpisodeRequester` fallback lets Compose 2D focus search skip "
                "Episode 1 when pressing Right from Season 1..N or Specials):\n"
                f"{run_res.stderr.strip()}"
            )

    print("PASS: Issue #495 D-pad Right enters EpisodePane on the season's first episode")


if __name__ == "__main__":
    main()
