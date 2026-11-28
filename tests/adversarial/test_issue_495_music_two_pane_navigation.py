#!/usr/bin/env python3
"""Adversarial UAT for issue #495 (music two-pane UI amendment):
`AlbumScreen` must match `SeasonScreen`'s Netflix-style two-pane navigation
without vertical D-pad escaping across panes or desynchronizing `inTracks`
(which traps D-pad Left and makes Back exit the entire artist screen).

Trusted amendment (verbatim):
  "Good work.  Now I want to now update music UI to look and feel the same way."
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
ALBUM_SCREEN = SCREENS_DIR / "AlbumScreen.kt"
SEASON_SCREEN = SCREENS_DIR / "SeasonScreen.kt"
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
                # Expression-bodied Modifier extension; capture through next top-level declaration.
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


def has_vertical_exit_cancellation(block: str, helpers: dict[str, str], is_plain_column: bool) -> bool:
    """True if `block` cancels vertical FocusDirection.Up and FocusDirection.Down
    exit from the pane (either via `focusProperties { onExit / exit ... }` properly
    paired with a `focusGroup()` / `LazyColumn` target node, or via explicit
    DirectionUp + DirectionDown boundary key guards)."""
    text = expand_helpers(block, helpers)
    for fp in re.finditer(r"focusProperties\s*\{", text):
        fp_body = text[fp.end() - 1 : match_braces(text, fp.end() - 1, "{", "}") + 1]
        cancels_exit = False
        if ("onExit" in fp_body or "exit" in fp_body) and (
            "cancelFocusChange" in fp_body or "FocusRequester.Cancel" in fp_body
        ):
            # Unconditional exit cancellation or explicit Up + Down cancellation.
            if "FocusDirection." not in fp_body or (
                "FocusDirection.Up" in fp_body and "FocusDirection.Down" in fp_body
            ):
                cancels_exit = True
        if cancels_exit:
            if not is_plain_column:
                return True
            # On a plain Column, `.focusProperties` must precede `.focusGroup()` (or sit on LazyColumn).
            after_fp = text[fp.end() :]
            if "focusGroup()" in after_fp or "LazyColumn(" in text[: fp.start()]:
                return True

    if "Key.DirectionUp" in text and "Key.DirectionDown" in text and "onPreviewKeyEvent" in text:
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
    for path in (ALBUM_SCREEN, SEASON_SCREEN, BROWSE_PANE):
        if not path.is_file():
            fail(f"missing {path}")
    album_src = ALBUM_SCREEN.read_text()
    season_src = SEASON_SCREEN.read_text()
    browse_src = BROWSE_PANE.read_text()
    helpers = collect_modifier_helpers(browse_src, album_src, season_src)

    album_screen_body = function_body(album_src, "fun AlbumScreen(", "AlbumScreen.kt")
    if "UatTestTags.ALBUM_CARD_PREFIX" not in album_src or "UatTestTags.TRACK_ROW_PREFIX" not in album_src:
        fail("AlbumScreen must preserve ALBUM_CARD_PREFIX and TRACK_ROW_PREFIX test tags")
    if "Row(" not in album_screen_body:
        fail("AlbumScreen must lay out albums on the left and tracks on the right in a two-pane Row")

    album_tag_pos = album_screen_body.find("UatTestTags.ALBUM_CARD_PREFIX")
    track_pane_call_pos = album_screen_body.find("TrackList(")
    if album_tag_pos == -1 or track_pane_call_pos == -1 or album_tag_pos > track_pane_call_pos:
        fail("AlbumScreen must render the left album list before the right TrackList pane")

    # Slice the left pane Column cleanly before TrackList(...)
    row_at = album_screen_body.find("Row(")
    left_col_at = album_screen_body.find("Column(", row_at)
    left_pane_block = (
        album_screen_body[left_col_at:track_pane_call_pos]
        if left_col_at != -1
        else album_screen_body[:track_pane_call_pos]
    )
    track_call_and_body = (
        album_screen_body[track_pane_call_pos:]
        + "\n"
        + function_body(album_src, "private fun TrackList(", "AlbumScreen.kt")
    )

    left_blocks_vertical = has_vertical_exit_cancellation(left_pane_block, helpers, is_plain_column=True)
    right_blocks_vertical = has_vertical_exit_cancellation(
        track_call_and_body, helpers, is_plain_column="LazyColumn(" not in track_call_and_body
    )
    right_continuous_albums = (
        "for (" in function_body(album_src, "private fun TrackList(", "AlbumScreen.kt")
        and "artist.albums" in function_body(album_src, "private fun TrackList(", "AlbumScreen.kt")
    )

    # Check whether focusing a track / TrackList updates `inTracks = true` (or equivalent selected album state)
    track_focus_syncs = False
    on_focus_track = re.search(r"onFocusTrack\s*=\s*\{", album_screen_body)
    if on_focus_track:
        cb_body = album_screen_body[
            on_focus_track.end() - 1 : match_braces(album_screen_body, on_focus_track.end() - 1, "{", "}") + 1
        ]
        if re.search(r"\binTracks\s*=\s*true\b", cb_body) or "onSelectAlbum" in cb_body:
            track_focus_syncs = True
    # Also accept `.onFocusChanged { ... inTracks = true ... }` on TrackList's modifier/container
    for ofc in re.finditer(r"onFocusChanged\s*\{", track_call_and_body):
        ofc_body = track_call_and_body[
            ofc.end() - 1 : match_braces(track_call_and_body, ofc.end() - 1, "{", "}") + 1
        ]
        if re.search(r"\binTracks\s*=\s*true\b", ofc_body):
            track_focus_syncs = True

    left_returns_explicitly = False
    for pd in re.finditer(r"onPaneDirection\s*\(\s*Key\.DirectionLeft", track_call_and_body):
        tail = track_call_and_body[pd.start() : pd.start() + 300]
        if "albumFocusRequester" in tail or "focusBrowseItem" in tail:
            left_returns_explicitly = True

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
                "--mode=music-two-pane",
                f"--left-pane-blocks-vertical-exit={'true' if left_blocks_vertical else 'false'}",
                f"--right-pane-blocks-vertical-exit={'true' if right_blocks_vertical else 'false'}",
                f"--right-pane-continuous-across-albums={'true' if right_continuous_albums else 'false'}",
                f"--track-focus-syncs-in-tracks={'true' if track_focus_syncs else 'false'}",
                f"--left-returns-to-album-explicitly={'true' if left_returns_explicitly else 'false'}",
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
                "AlbumScreen two-pane D-pad boundary / focus-sync check failed:\n"
                f"{run_res.stderr.strip()}"
            )

    print("PASS: Issue #495 music two-pane UI preserves D-pad pane boundaries and track/album focus sync")


if __name__ == "__main__":
    main()
