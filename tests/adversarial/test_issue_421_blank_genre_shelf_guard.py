#!/usr/bin/env python3
"""Adversarial UAT for Issue #421: genre shelves/rows must never surface a
blank or whitespace-only genre tag.

Reproduction (the finding filed against #397): scraped media can carry a
literal empty-string (or whitespace-only) genre tag on enough entries to
pass MIN_GENRE_SHELF_SIZE in topGenreShelves() (CatalogScreen.kt). Before
this fix, that produced a genre shelf/row keyed "" (or "   "); opening a
card from it resolved genreScope="" (only null-checked, not blank-checked)
in SwarmViewModel.openArtistAlbums/openShowSeasons, and
artistsForBrowseAll/showsForBrowseAll/moviesForBrowseAll (BrowseAllShelf.kt)
filtered via entry.genres.contains(""), which is almost always empty since
real genre lists rarely contain a blank string — producing a shelf that
opens into an empty/confusing nested screen.

This suite checks, beyond the worker's own unit test additions:
  1. topGenreShelves' own ranking/threshold step drops blank AND
     whitespace-only (including non-ASCII whitespace like U+00A0) genre
     keys before they can ever reach MIN_GENRE_SHELF_SIZE — not just that
     a downstream filter happens to yield an empty result;
  2. the real, compiled entriesForGenreScope/moviesForBrowseAll/
     showsForBrowseAll production functions treat "" and whitespace-only
     scopes/titles as absent scope, not as a literal (mostly unmatched)
     genre string;
  3. the SwarmViewModel open-time scope resolution treats a blank
     genreScope argument the same as a missing one.
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
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
CATALOG_SCREEN = APP / "ui/screens/CatalogScreen.kt"
BROWSE_ALL = APP / "data/BrowseAllShelf.kt"
DRIVER = Path(__file__).with_name("BlankGenreShelfGuardDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)


def function_body(source: str, name: str) -> str:
    match = re.search(rf"(?:private |internal |    )?fun (?:<[^>]+>\s*)?{re.escape(name)}\s*\(", source)
    if not match:
        fail(f"missing {name}")
    start = match.start()
    cuts = [
        i
        for i in (
            source.find("\n    fun ", start + 1),
            source.find("\n    private fun ", start + 1),
            source.find("\n    internal fun ", start + 1),
            source.find("\nprivate fun ", start + 1),
            source.find("\ninternal fun ", start + 1),
        )
        if i != -1
    ]
    end = min(cuts) if cuts else len(source)
    return source[start:end]


def assert_top_genre_shelves_drops_blank_keys_before_threshold() -> None:
    """The isNotBlank() guard must sit between ranking and the
    MIN_GENRE_SHELF_SIZE threshold check — dropping blank genre strings
    from the candidate set outright, not merely happening to produce an
    empty grouped list downstream (a coincidence that would break the
    moment any real content ever carried a blank genre tag)."""
    source = CATALOG_SCREEN.read_text()
    body = function_body(source, "topGenreShelves")
    if not re.search(r"\.filter\s*\{\s*\(genre,\s*_\)\s*->\s*genre\.isNotBlank\(\)\s*\}", body):
        fail("topGenreShelves does not filter out blank/whitespace-only genre keys before grouping")
    filter_pos = body.find(".filter { (genre, _) -> genre.isNotBlank() }")
    threshold_pos = body.find("MIN_GENRE_SHELF_SIZE")
    if filter_pos == -1 or threshold_pos == -1 or filter_pos >= threshold_pos:
        fail("blank-genre filter must run before the MIN_GENRE_SHELF_SIZE threshold check")
    print("ok topGenreShelves drops blank genre keys before the shelf-size threshold")


def assert_view_model_guards_blank_genre_scope() -> None:
    """openArtistAlbums/openShowSeasons must not just null-check genreScope
    — a "" or whitespace-only string is falsy for this purpose too, since a
    blank-genre shelf can exist and be tapped just like any other."""
    model = VIEW_MODEL.read_text()
    for name, shelf in (("openArtistAlbums", "ArtistShelf"), ("openShowSeasons", "ShowShelf")):
        body = function_body(model, name)
        if not re.search(
            rf"\(previous as\? UiState\.{shelf}\)\?\.takeIf \{{ it\.scopedToGenre \}}\?\.title\s*\n\s*\?\: genreScope\.takeIf \{{ previous is UiState\.Catalog && it\?\.isNotBlank\(\) == true \}}",
            body,
        ):
            fail(f"{name} does not guard genreScope with a blank check (it?.isNotBlank() == true)")
    print("ok openArtistAlbums/openShowSeasons guard genreScope against blank strings")


def jars_named(name: str) -> list[str]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return [
        str(p)
        for p in sorted(cache.glob(f"**/{name}-*.jar"))
        if "sources" not in p.name and "javadoc" not in p.name
    ]


def core_classpath() -> str:
    result = subprocess.run(
        [str(GRADLEW), "-p", str(ROOT / "clients/tv-android"), ":core:compileKotlin", "-q"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        fail(f"core compilation failed:\n{result.stdout}\n{result.stderr}")
    classes = CORE / "build/classes/kotlin/main"
    if not classes.is_dir():
        fail(f"core classes missing at {classes}")
    jars = [str(classes)]
    for name in (
        "kotlin-stdlib",
        "kotlin-stdlib-jdk8",
        "kotlin-stdlib-jdk7",
        "kotlinx-serialization-core-jvm",
        "kotlinx-serialization-json-jvm",
        "kotlinx-coroutines-core-jvm",
        "annotations",
    ):
        jars.extend(jars_named(name))
    return ":".join(dict.fromkeys(jars))


def kotlin_compiler() -> list[str]:
    embedded = jars_named("kotlin-compiler-embeddable")
    if not embedded:
        fail("kotlin compiler is absent from Gradle cache")
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
    return ["java", "-cp", ":".join([embedded[-1], *extras]), "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler"]


def assert_driver_passes() -> None:
    classpath = core_classpath()
    with tempfile.TemporaryDirectory() as temp:
        out = Path(temp) / "classes"
        out.mkdir()
        compile_result = subprocess.run(
            kotlin_compiler()
            + ["-no-stdlib", "-no-reflect", "-cp", classpath, "-d", str(out), str(BROWSE_ALL), str(DRIVER)],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            fail(
                "blank genre shelf guard driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "BlankGenreShelfGuardDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "blank genre shelf guard driver failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_top_genre_shelves_drops_blank_keys_before_threshold()
    assert_view_model_guards_blank_genre_scope()
    assert_driver_passes()
    print("PASS: Issue #421 blank genre shelf guard UAT")


if __name__ == "__main__":
    main()
