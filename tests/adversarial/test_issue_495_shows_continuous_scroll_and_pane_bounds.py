#!/usr/bin/env python3
"""Adversarial UAT for issue #495 (shows continuous cross-season scrolling &
left-pane vertical D-pad bounds):
1. Scrolling down/up through episodes in `EpisodePane` must traverse across all
   seasons continuously while updating the highlighted/selected season on the
   left and keeping `SeasonPane`'s ability to select any season.
2. Pressing D-pad Down at the bottom of `SeasonPane` (or Up at the top of
   `SeasonPane`) must stay within `SeasonPane` rather than escaping diagonally
   into mid-season episodes in `EpisodePane`.

Trusted amendment (verbatim):
  "I also want to update the shows UI so that as the user scrolls down on the
   episodes, and the end of the season is in focus, the user can keep scrolling
   down which moves to next season.  Keep the seasons on the left and ability
   to select a season but scrolling should navigate through all seasons not
   just the selected season."
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
SCREENS_DIR = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui/screens"
SEASON_SCREEN = SCREENS_DIR / "SeasonScreen.kt"
ALBUM_SCREEN = SCREENS_DIR / "AlbumScreen.kt"
BROWSE_PANE = SCREENS_DIR / "BrowsePane.kt"
CORE_SRC = ROOT / "clients/tv-android/core/src/main/kotlin/app/swarm/tv/core"
CORE_FILES = [
    CORE_SRC / "capability/Contracts.kt",
    CORE_SRC / "peer/Contracts.kt",
    CORE_SRC / "catalog/CatalogMerger.kt",
    CORE_SRC / "catalog/CatalogGrouping.kt",
    CORE_SRC / "watch/WatchState.kt",
]
DRIVER = Path(__file__).with_name("Issue495TwoPaneBoundariesDriver.kt")


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


def function_body(source: str, signature: str, label: str) -> str:
    start = source.find(signature)
    if start == -1:
        fail(f"missing `{signature}` in {label}")
    brace = source.find("{", start)
    if brace == -1:
        fail(f"`{signature}` has no body in {label}")
    return source[start : match_braces(source, brace, "{", "}") + 1]


def collect_modifier_helpers(*sources: str) -> dict[str, str]:
    helpers: dict[str, str] = {}
    pattern = re.compile(r"(?:internal\s+|private\s+)?fun\s+Modifier\.(\w+)\s*\(")
    for src in sources:
        for m in pattern.finditer(src):
            name = m.group(1)
            paren_end = match_braces(src, m.end() - 1, "(", ")")
            eq_or_brace = re.search(r"[={]", src[paren_end + 1 :])
            if not eq_or_brace:
                continue
            pos = paren_end + 1 + eq_or_brace.start()
            if src[pos] == "{":
                end = match_braces(src, pos, "{", "}") + 1
            else:
                nxt = re.search(r"\n(?:@\w+\s*\n)*(?:internal|private|public|fun|class|val|const)\b", src[pos + 1 :])
                end = (pos + 1 + nxt.start()) if nxt else len(src)
            helpers[name] = src[m.start() : end]
    return helpers


def expand_helpers(block: str, helpers: dict[str, str]) -> str:
    expanded = block
    for _ in range(3):
        added = []
        for name, body in helpers.items():
            if re.search(rf"\.\s*{re.escape(name)}\s*(?:\(|\{{)", expanded) and body not in expanded:
                added.append(body)
        if not added:
            break
        expanded = expanded + "\n" + "\n".join(added)
    return expanded


def season_pane_blocks_vertical_exit(
    season_pane_call: str,
    season_pane_body: str,
    helpers: dict[str, str],
) -> bool:
    """True if `SeasonPane`'s root container cancels vertical FocusDirection.Up
    and FocusDirection.Down exit so D-pad Up on Watchlist/Resume and D-pad Down
    on the bottom SeasonRow cannot jump across into `EpisodePane`."""
    combined = expand_helpers(season_pane_call + "\n" + season_pane_body, helpers)
    # Note: a focusProperties placed ONLY on SeasonPane's inner LazyColumn would
    # block Up from Season 1 to Resume/Watchlist! It must guard the SeasonPane
    # root container (before SeasonPane's `.focusGroup()`) or guard Up on the top
    # action row and Down on the bottom season list.
    call_expanded = expand_helpers(season_pane_call, helpers)
    root_col_at = season_pane_body.find("Column(")
    lazy_at = season_pane_body.find("LazyColumn(")
    root_header_slice = (
        expand_helpers(season_pane_body[root_col_at:lazy_at], helpers)
        if root_col_at != -1 and lazy_at != -1
        else ""
    )

    for candidate in (call_expanded, root_header_slice):
        for fp in re.finditer(r"focusProperties\s*\{", candidate):
            fp_body = candidate[fp.end() - 1 : match_braces(candidate, fp.end() - 1, "{", "}") + 1]
            if ("onExit" in fp_body or "exit" in fp_body) and (
                "cancelFocusChange" in fp_body or "FocusRequester.Cancel" in fp_body
            ):
                if "FocusDirection." not in fp_body or (
                    "FocusDirection.Up" in fp_body and "FocusDirection.Down" in fp_body
                ):
                    if "focusGroup()" in candidate[fp.end() :]:
                        return True

    if "Key.DirectionUp" in combined and "Key.DirectionDown" in combined and "onPreviewKeyEvent" in combined:
        return True
    return False


def extract_season_code(season_src: str) -> str:
    sec_start = season_src.find("private class EpisodeSection(")
    if sec_start == -1:
        fail("SeasonScreen.kt is missing `private class EpisodeSection(`")
    sec_brace = season_src.find("{", sec_start)
    sec_block = season_src[sec_start : match_braces(season_src, sec_brace, "{", "}") + 1].replace(
        "private class EpisodeSection(", "internal class EpisodeSection("
    )
    tail_start = season_src.find("internal fun resumeEpisode(")
    if tail_start == -1:
        fail("SeasonScreen.kt is missing `internal fun resumeEpisode(`")
    tail = season_src[tail_start:].replace("\nprivate fun ", "\ninternal fun ")
    return (
        "package app.swarm.tv.app.ui.screens\n\n"
        "import app.swarm.tv.core.catalog.CatalogGrouping\n"
        "import app.swarm.tv.core.catalog.MergedEntry\n"
        "import app.swarm.tv.core.catalog.SeasonGroup\n"
        "import app.swarm.tv.core.catalog.ShowGroup\n"
        "import app.swarm.tv.core.watch.WatchState\n"
        "import app.swarm.tv.core.watch.stateFor\n\n"
        + sec_block
        + "\n\n"
        + tail
        + "\n"
    )


def extract_album_code(album_src: str) -> str:
    album_for_key = function_body(album_src, "fun albumForKey(", "AlbumScreen.kt")
    m = re.search(r"private\s+fun\s+trackCount\s*\([^\n]+", album_src)
    if not m:
        fail("AlbumScreen.kt is missing `private fun trackCount(`")
    track_count = m.group(0).replace("private fun ", "internal fun ")
    return (
        "package app.swarm.tv.app.ui.screens\n\n"
        "import app.swarm.tv.core.catalog.AlbumGroup\n"
        "import app.swarm.tv.core.catalog.ArtistGroup\n\n"
        + album_for_key
        + "\n\n"
        + track_count
        + "\n"
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
    for path in (SEASON_SCREEN, ALBUM_SCREEN, BROWSE_PANE):
        if not path.is_file():
            fail(f"missing {path}")
    season_src = SEASON_SCREEN.read_text()
    album_src = ALBUM_SCREEN.read_text()
    browse_src = BROWSE_PANE.read_text()
    helpers = collect_modifier_helpers(browse_src, season_src, album_src)

    screen_body = function_body(season_src, "fun SeasonScreen(", "SeasonScreen.kt")
    season_pane_body = function_body(season_src, "private fun SeasonPane(", "SeasonScreen.kt")
    episode_pane_body = function_body(season_src, "private fun EpisodePane(", "SeasonScreen.kt")

    sp_call_at = screen_body.find("SeasonPane(")
    ep_call_at = screen_body.find("EpisodePane(")
    if sp_call_at == -1 or ep_call_at == -1 or sp_call_at > ep_call_at:
        fail("SeasonScreen must render SeasonPane (left) before EpisodePane (right)")
    sp_call = screen_body[sp_call_at : match_braces(screen_body, screen_body.find("(", sp_call_at), "(", ")") + 1]

    # EpisodePane must use one continuous LazyColumn across sections (not per-season `key(seasonKey(...))`).
    episode_pane_continuous = (
        episode_pane_body.count("LazyColumn(") == 1
        and "for (section in sections)" in episode_pane_body
        and "key(seasonKey(" not in episode_pane_body
    )

    # Episode focus callback must update highlightedKey + onSelectSeason(season), and initial focus
    # LaunchedEffect must not re-fire on `selectedSeason` changes while `inEpisodes` remains true.
    focus_ep_match = re.search(r"onFocusEpisode\s*=\s*\{", screen_body)
    focus_ep_body = (
        screen_body[
            focus_ep_match.end() - 1 : match_braces(screen_body, focus_ep_match.end() - 1, "{", "}") + 1
        ]
        if focus_ep_match
        else ""
    )
    le_match = re.search(r"LaunchedEffect\s*\(([^)]*)\)", screen_body)
    le_keys = le_match.group(1) if le_match else ""
    episode_focus_syncs = (
        "highlightedKey" in focus_ep_body
        and "onSelectSeason(" in focus_ep_body
        and "selectedSeason" not in le_keys
        and "highlightedKey" not in le_keys
    )

    blocks_vertical = season_pane_blocks_vertical_exit(sp_call, season_pane_body, helpers)

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

        season_file = temp_path / "SeasonScreenExtracted.kt"
        album_file = temp_path / "AlbumScreenExtracted.kt"
        season_file.write_text(extract_season_code(season_src))
        album_file.write_text(extract_album_code(album_src))
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
                str(season_file),
                str(album_file),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
            env=env,
        )
        if compile_res.returncode:
            fail(f"Driver compilation failed:\n{compile_res.stdout}\n{compile_res.stderr}")

        run_res = subprocess.run(
            [
                "java",
                "-cp",
                f"{out_dir}:{classpath}",
                "app.swarm.tv.app.ui.screens.Issue495TwoPaneBoundariesDriverKt",
                "--mode=shows-continuous-and-bounds",
                f"--episode-pane-is-continuous={'true' if episode_pane_continuous else 'false'}",
                f"--episode-focus-syncs-season-without-refocus={'true' if episode_focus_syncs else 'false'}",
                f"--season-pane-blocks-vertical-exit={'true' if blocks_vertical else 'false'}",
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
                "SeasonScreen continuous episode scroll / SeasonPane vertical boundary check failed:\n"
                f"{run_res.stderr.strip()}"
            )

    print("PASS: Issue #495 shows continuous cross-season scrolling and SeasonPane D-pad bounds verified")


if __name__ == "__main__":
    main()
