#!/usr/bin/env python3
"""Adversarial UAT for Issue #397: genre-row artist/show cards open scoped.

Reproduction (the finding filed against #369): on the home Catalog, opening
an artist from a Music genre row (or a show from a Shows genre row) directly
— not via that genre's "Browse All" shelf — used to always resolve
genreScope=null. #369 had already made the *Browse All* nested-open path and
the catalog-delta rebuild path (replaceEmbeddedCatalog) genre-aware; this is
the third, previously-uncovered entry point: the genre row that lives
directly on Catalog (CatalogScreen's per-genre ArtistShelfRow/ShowShelfRow,
and GenreFilteredGrid's cards when a genre is selected on Catalog itself).

This suite checks two things #369's suite does not:
  1. every direct-from-Catalog call site threads (or deliberately withholds)
     a genre string into onOpenArtist/onOpenShow — the wiring the bug report
     says was missing;
  2. openArtistAlbums/openShowSeasons resolve that genre into genreScope
     with the right precedence (a genre-scoped Browse All shelf always wins
     over the param; the param only ever applies to a *direct* Catalog
     open; a top-level Music/Shows row or the watchlist must stay unscoped
     even though they too open straight from Catalog).
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
MAIN_ACTIVITY = APP / "MainActivity.kt"
DRIVER = Path(__file__).with_name("GenreCardOpenScopeDriver.kt")
BROWSE_ALL = APP / "data/BrowseAllShelf.kt"
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
        )
        if i != -1
    ]
    end = min(cuts) if cuts else len(source)
    return source[start:end]


def assert_open_signatures_carry_a_genre_slot() -> None:
    """onOpenArtist/onOpenShow must be able to carry a genre string end to
    end from a Catalog genre row to the ViewModel. A 1-arg callback cannot
    express "no genre" vs "this genre" and is exactly what left the direct
    genre-row path unscoped before #397."""
    model = VIEW_MODEL.read_text()
    catalog_screen = CATALOG_SCREEN.read_text()
    activity = MAIN_ACTIVITY.read_text()

    if not re.search(r"fun openArtistAlbums\(artist: ArtistGroup, genreScope: String\?", model):
        fail("openArtistAlbums lost its genreScope parameter; direct genre-row opens cannot scope")
    if not re.search(r"fun openShowSeasons\(show: ShowGroup, genreScope: String\?", model):
        fail("openShowSeasons lost its genreScope parameter; direct genre-row opens cannot scope")

    if "onOpenArtist: (ArtistGroup, String?) -> Unit" not in catalog_screen:
        fail("CatalogScreen's onOpenArtist callback no longer carries a genre slot")
    if "onOpenShow: (ShowGroup, String?) -> Unit" not in catalog_screen:
        fail("CatalogScreen's onOpenShow callback no longer carries a genre slot")
    if "onOpenArtist: (ArtistGroup, String?) -> Unit" not in activity:
        fail("MainActivity's onOpenArtist callback no longer carries a genre slot")
    if "onOpenShow: (ShowGroup, String?) -> Unit" not in activity:
        fail("MainActivity's onOpenShow callback no longer carries a genre slot")

    # The 2-arg method reference is what actually plumbs the param through
    # to the ViewModel; a lambda that drops the second arg would silently
    # discard it despite the signature matching.
    if "onOpenArtist = viewModel::openArtistAlbums" not in activity:
        fail("MainActivity does not bind onOpenArtist straight to openArtistAlbums(artist, genreScope)")
    if "onOpenShow = viewModel::openShowSeasons" not in activity:
        fail("MainActivity does not bind onOpenShow straight to openShowSeasons(show, genreScope)")
    print("ok onOpenArtist/onOpenShow carry a genre slot end to end")


def assert_genre_rows_pass_their_genre_and_others_pass_null() -> None:
    """Every direct-from-Catalog call site must make a deliberate choice:
    the per-genre Music/Shows rows and GenreFilteredGrid's cards (Catalog's
    own genre-filtered grid) pass their genre; the top-level Music/Shows
    rows and the watchlist quick-access row must pass null, since a
    top-level row is not scoped to any single genre."""
    source = CATALOG_SCREEN.read_text()

    genre_scoped_call_sites = [
        # Genre sub-shelf rows.
        r"onOpenShow\s*=\s*\{\s*onOpenShow\(it,\s*genre\)\s*\}",
        r"onOpenArtist\s*=\s*\{\s*onOpenArtist\(it,\s*genre\)\s*\}",
        # GenreFilteredGrid's own cards (Catalog's single-genre grid).
        r"onClick\s*=\s*\{\s*onOpenShow\(show,\s*genre\)\s*\}",
        r"onClick\s*=\s*\{\s*onOpenArtist\(artist,\s*genre\)\s*\}",
    ]
    for pattern in genre_scoped_call_sites:
        if not re.search(pattern, source):
            fail(f"a genre-row/genre-grid call site does not thread its own genre: {pattern}")

    unscoped_call_sites = [
        # Watchlist quick access is never genre-specific.
        r"item\.show\?\.let\s*\{\s*onOpenShow\(it,\s*null\)\s*\}",
        # Top-level Music/Shows rows list every genre, so they are not one.
        r'onOpenShow\s*=\s*\{\s*onOpenShow\(it,\s*null\)\s*\}',
        r'onOpenArtist\s*=\s*\{\s*onOpenArtist\(it,\s*null\)\s*\}',
    ]
    for pattern in unscoped_call_sites:
        if not re.search(pattern, source):
            fail(f"a top-level/unscoped call site does not explicitly pass null: {pattern}")

    print("ok genre rows thread their genre; top-level rows and watchlist stay unscoped")


def assert_browse_all_shelf_internal_navigation_stays_null() -> None:
    """Clicking an artist/show while already inside an ArtistShelf/ShowShelf
    screen (#369's Browse All) must pass null for the param — that path's
    scope comes from the shelf's own scopedToGenre/title, not this param.
    A stray non-null here would fight the shelf-derived scope or silently
    change precedence."""
    activity = MAIN_ACTIVITY.read_text()
    if not re.search(r"onOpenArtist\(artist,\s*null\)", activity):
        fail("ArtistShelf's internal artist click does not pass null for genreScope")
    if not re.search(r"onOpenShow\(show,\s*null\)", activity):
        fail("ShowShelf's internal show click does not pass null for genreScope")
    print("ok Browse All shelves' internal navigation passes null, deferring to shelf-derived scope")


def assert_scope_precedence_and_immediate_filtering() -> None:
    """openArtistAlbums/openShowSeasons must: (a) let a genre-scoped Browse
    All shelf's own title win over the param — the param only applies to a
    *direct* Catalog open; (b) immediately narrow the artists/shows list at
    open time using the resolved scope, not only on the next catalog delta
    — the bug is visible before any delta ever arrives."""
    model = VIEW_MODEL.read_text()
    artist_open = function_body(model, "openArtistAlbums")
    show_open = function_body(model, "openShowSeasons")

    for name, body, shelf in (
        ("openArtistAlbums", artist_open, "ArtistShelf"),
        ("openShowSeasons", show_open, "ShowShelf"),
    ):
        scope_line = re.search(
            rf"val scope = \(previous as\? UiState\.{shelf}\)\?\.takeIf \{{ it\.scopedToGenre \}}\?\.title\s*\n\s*\?\: genreScope\.takeIf \{{ previous is UiState\.Catalog \}}",
            body,
        )
        if not scope_line:
            fail(
                f"{name} does not resolve scope with shelf-title-first, "
                "Catalog-param-second precedence — a Browse All open could "
                "be overridden by a stray param, or a shelf-internal click "
                "could pick up the wrong genre"
            )
        immediate_filter = re.search(
            r"if \(previous is UiState\.Catalog && scope != null\)",
            body,
        )
        if not immediate_filter:
            fail(
                f"{name} does not immediately narrow its artists/shows list "
                "from the resolved scope at open time; the nested screen "
                "would stay unscoped until a catalog delta happened to arrive"
            )
        if "genreScope = scope" not in body:
            fail(f"{name} does not store the resolved scope on the nested UiState")

    print("ok scope resolution has shelf-first precedence and filters immediately at open time")


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


def assert_open_time_scope_membership() -> None:
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
                "genre card open-scope driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "GenreCardOpenScopeDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "genre card open-scope membership failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_open_signatures_carry_a_genre_slot()
    assert_genre_rows_pass_their_genre_and_others_pass_null()
    assert_browse_all_shelf_internal_navigation_stays_null()
    assert_scope_precedence_and_immediate_filtering()
    assert_open_time_scope_membership()
    print("PASS: Issue #397 genre-row card open-scope UAT")


if __name__ == "__main__":
    main()
