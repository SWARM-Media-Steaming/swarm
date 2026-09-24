#!/usr/bin/env python3
"""Adversarial UAT for #398: nested genre playback stays in its origin scope.

The user-visible contract is queue membership, not merely the initial
selection. A nested artist/show reached through a genre Browse All page must
not queue an entry outside that genre on explicit skip, automatic advance,
preload, shuffle/repeat recomputation, or music previous. Top-level and other
unscoped paths retain whole-catalog behavior.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
CORE = ROOT / "clients/tv-android/core"
BROWSE_ALL = APP / "data/BrowseAllShelf.kt"
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
DRIVER = Path(__file__).with_name("GenreScopedPlaybackQueueDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def body_after(source: str, marker: str, next_marker: str) -> str:
    start = source.find(marker)
    if start < 0:
        fail(f"missing queue operation: {marker}")
    end = source.find(next_marker, start + len(marker))
    return source[start : end if end >= 0 else len(source)]


def assert_view_model_routes_every_queue_operation_through_scope() -> None:
    source = VIEW_MODEL.read_text()

    scope = body_after(source, "private fun UiState.playbackGenreScope(", "/**\n * Screens")
    if "is UiState.ArtistAlbums -> genreScope.takeIf { kind == MediaKind.TRACK }" not in scope:
        fail("ArtistAlbums playback scope does not apply only to track queues")
    if "is UiState.ShowSeasons -> genreScope.takeIf { kind == MediaKind.EPISODE }" not in scope:
        fail("ShowSeasons playback scope does not apply only to episode queues")
    if re.search(r"BROWSE_ALL_(MUSIC|SHOWS)_TITLE", scope):
        fail("playback scope infers membership from a top-level heading")

    helper = body_after(source, "private fun playbackQueueEntries(", "/**\n * Screens")
    if "entriesForGenreScope(entries, previousScreen.playbackGenreScope(kind))" not in helper:
        fail("playback queue helper does not apply the nested screen's stored genre scope")

    # Each transition has a distinct lifecycle. Check them independently so a
    # later refactor cannot fix initial play while leaving preloads, previous,
    # or shuffle/repeat recomputation wide open.
    operations = [
        ("episode preload/autoplay", "fun preloadNextEpisode(", "fun preloadNextTrack(", "current.previous", "MediaKind.EPISODE"),
        ("track preload/autoplay", "fun preloadNextTrack(", "fun playNext(", "current.previous", "MediaKind.TRACK"),
        ("shuffle/repeat recomputation", "private fun recomputeActiveTrackSuccessor(", "fun playPrevious(", "current.previous", "MediaKind.TRACK"),
        ("music previous", "fun playPrevious(", "/**\n     * The actual negotiation", "current.previous", "MediaKind.TRACK"),
        ("initial play", "private fun playEntry(", "private fun back(", "previousScreen", "MediaKind"),
    ]
    for name, start, end, previous, kind in operations:
        body = body_after(source, start, end)
        if "playbackQueueEntries(" not in body or previous not in body or kind not in body:
            fail(f"{name} does not group its queue through the appropriate scoped previous screen")
        if "CatalogGrouping.groupTracksByArtistAlbum(catalog.entries)" in body:
            fail(f"{name} still groups the whole catalog for tracks")
        if "CatalogGrouping.groupEpisodesByShowSeason(catalog.entries)" in body:
            fail(f"{name} still groups the whole catalog for episodes")
    print("ok all initial, skip/autoplay, preload, shuffle/repeat, and previous queue paths use genre scope")


def jars_named(name: str) -> list[str]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return [str(p) for p in sorted(cache.glob(f"**/{name}-*.jar")) if "sources" not in p.name and "javadoc" not in p.name]


def core_classpath() -> str:
    result = subprocess.run([str(GRADLEW), "-p", str(ROOT / "clients/tv-android"), ":core:compileKotlin", "-q"], cwd=ROOT, capture_output=True, text=True)
    if result.returncode:
        fail(f"core compilation failed:\n{result.stdout}\n{result.stderr}")
    classes = CORE / "build/classes/kotlin/main"
    if not classes.is_dir():
        fail(f"core classes missing at {classes}")
    jars = [str(classes)]
    for name in ("kotlin-stdlib", "kotlin-stdlib-jdk8", "kotlin-stdlib-jdk7", "kotlinx-serialization-core-jvm", "kotlinx-serialization-json-jvm", "kotlinx-coroutines-core-jvm", "annotations"):
        jars.extend(jars_named(name))
    return ":".join(dict.fromkeys(jars))


def kotlin_compiler() -> list[str]:
    embedded = jars_named("kotlin-compiler-embeddable")
    if not embedded:
        fail("kotlin compiler is absent from Gradle cache")
    extras: list[str] = []
    for name in ("kotlin-stdlib", "kotlin-reflect", "kotlin-script-runtime", "kotlinx-coroutines-core-jvm", "annotations", "trove4j"):
        extras.extend(jars_named(name)[:3])
    return ["java", "-cp", ":".join([embedded[-1], *extras]), "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler"]


def assert_executable_queue_membership() -> None:
    classpath = core_classpath()
    with tempfile.TemporaryDirectory() as temp:
        out = Path(temp) / "classes"
        out.mkdir()
        compiled = subprocess.run(kotlin_compiler() + ["-no-stdlib", "-no-reflect", "-cp", classpath, "-d", str(out), str(BROWSE_ALL), str(DRIVER)], capture_output=True, text=True)
        if compiled.returncode:
            fail(f"queue driver compilation failed:\n{compiled.stdout}\n{compiled.stderr}")
        ran = subprocess.run(["java", "-cp", f"{out}:{classpath}", "GenreScopedPlaybackQueueDriverKt"], capture_output=True, text=True)
        if ran.returncode or "ALL_OK" not in ran.stdout:
            fail(f"genre-scoped playback queue membership failed:\n{ran.stdout}\n{ran.stderr}")
        print(ran.stdout.strip())


def main() -> None:
    assert_view_model_routes_every_queue_operation_through_scope()
    assert_executable_queue_membership()
    print("PASS: Issue #398 nested genre playback queue UAT")


if __name__ == "__main__":
    main()
