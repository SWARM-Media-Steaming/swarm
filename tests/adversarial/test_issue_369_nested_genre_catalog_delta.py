#!/usr/bin/env python3
"""Adversarial UAT for Issue #369: nested artist/show screens stay genre-scoped.

Reproduction: Music genre Browse All -> open an artist (or Shows genre Browse
All -> open a show), then a catalog delta arrives. The nested screen must
rebuild the same genre subset the originating Browse All page uses. Grouping
the whole kind would add albums or episodes that were never on that page.

Unscoped Music/Shows Browse All (and a plain catalog open) still regroup the
whole kind. A user genre spelled "Music" or "Shows" is still that genre —
heading text is not the discriminator.
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
BROWSE_ALL = APP / "data/BrowseAllShelf.kt"
ACTIVITY = APP / "MainActivity.kt"
DRIVER = Path(__file__).with_name("NestedGenreCatalogDeltaDriver.kt")
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
    nxt = source.find("\n    fun ", start + 1)
    nxt2 = source.find("\n    private fun ", start + 1)
    nxt3 = source.find("\n    internal fun ", start + 1)
    cuts = [i for i in (nxt, nxt2, nxt3) if i != -1]
    end = min(cuts) if cuts else len(source)
    return source[start:end]


def replace_embedded_catalog(source: str) -> str:
    start = source.find("private fun replaceEmbeddedCatalog(")
    if start == -1:
        fail("replaceEmbeddedCatalog is missing; a catalog delta cannot rebuild nested screens")
    nxt = source.find("\n    /**", start + 1)
    return source[start : nxt if nxt != -1 else len(source)]


def state_branch(body: str, state: str) -> str:
    marker = f"is UiState.{state} ->"
    at = body.find(marker)
    if at == -1:
        fail(f"replaceEmbeddedCatalog does not rebuild UiState.{state}")
    following = body.find("is UiState.", at + len(marker))
    return body[at : following if following != -1 else len(body)]


def assert_nested_open_inherits_genre_scope_only_from_genre_shelf() -> None:
    """Opening from a genre Browse All must carry that genre into the nested screen.

    Opening from the top-level Music/Shows grid, or from Catalog, must not
    treat the heading string as a genre — "Music" and "Shows" are valid
    user genre names.
    """
    model = VIEW_MODEL.read_text()
    artist_open = function_body(model, "openArtistAlbums")
    show_open = function_body(model, "openShowSeasons")

    if "is UiState.ArtistShelf" not in artist_open:
        fail("openArtistAlbums no longer opens from a Music/genre Browse All shelf")
    if "is UiState.ShowShelf" not in show_open:
        fail("openShowSeasons no longer opens from a Shows/genre Browse All shelf")

    # Scope is the genre shelf's title only when that shelf is genre-scoped.
    # Reading scopedToGenre on the shelf (at open, or later via previous)
    # is the discriminator; the nested states have no title of their own.
    for name, body, shelf in (
        ("openArtistAlbums", artist_open, "ArtistShelf"),
        ("openShowSeasons", show_open, "ShowShelf"),
    ):
        if "scopedToGenre" not in body and "genreScope" not in body:
            fail(
                f"{name} does not record whether it was opened from a genre "
                f"Browse All {shelf}; a later catalog delta cannot stay on that genre"
            )
        if f"UiState.{shelf}" not in body:
            fail(f"{name} does not inherit from UiState.{shelf}")
        # Unscoped Music/Shows Browse All must remain whole-kind. Tying scope
        # to the heading string would filter a top-level Music grid to the
        # "Music" genre.
        if re.search(r"title\s*==\s*BROWSE_ALL_(MUSIC|SHOWS)_TITLE", body):
            fail(f"{name} infers genre scope from the Music/Shows heading text")
        if "BROWSE_ALL_MUSIC_TITLE" in body or "BROWSE_ALL_SHOWS_TITLE" in body:
            fail(f"{name} uses the top-level heading constants as a genre key")
    print("ok nested open inherits genre scope only from a genre-scoped shelf")


def assert_catalog_delta_rebuilds_nested_screens_with_inherited_scope() -> None:
    model = VIEW_MODEL.read_text()
    body = replace_embedded_catalog(model)

    artist = state_branch(body, "ArtistAlbums")
    show = state_branch(body, "ShowSeasons")

    if "artistsForBrowseAll(catalog.entries" not in artist:
        fail(
            "genre-scoped ArtistAlbums catalog delta does not rebuild through "
            "artistsForBrowseAll; albums outside the originating genre can appear"
        )
    if "CatalogGrouping.groupTracksByArtistAlbum(catalog.entries)" not in artist:
        fail("unscoped ArtistAlbums catalog delta lost whole-kind regrouping")
    if "showsForBrowseAll(catalog.entries" not in show:
        fail(
            "genre-scoped ShowSeasons catalog delta does not rebuild through "
            "showsForBrowseAll; episodes outside the originating genre can appear"
        )
    if "CatalogGrouping.groupEpisodesByShowSeason(catalog.entries)" not in show:
        fail("unscoped ShowSeasons catalog delta lost whole-kind regrouping")

    for name, copy in (("ArtistAlbums", artist), ("ShowSeasons", show)):
        # Scope must be the inherited genre (stored, or the previous genre
        # shelf), never a rewrite of the nested screen into the whole kind.
        uses_stored_scope = "state.genreScope" in copy
        uses_previous_shelf = "scopedToGenre" in copy and "previous" in copy
        if not uses_stored_scope and not uses_previous_shelf:
            fail(
                f"{name} catalog delta has no genre discriminator; it cannot "
                "tell a Jazz nested artist from a top-level Music nested artist"
            )
        if re.search(rf"\bgenreScope\s*=", copy):
            fail(f"a catalog delta rewrites UiState.{name}.genreScope")
        if "replaceEmbeddedCatalog(state.previous, catalog)" not in copy:
            fail(
                f"{name} catalog delta does not rebuild the Browse All page "
                "underneath; Back would return to a stale or widened shelf"
            )
        # Nested screens have no title; using one would not compile, but a
        # mistaken state.title on a copied shelf helper would pick "Music".
        if re.search(r"artistsForBrowseAll\(catalog\.entries,\s*state\.title\)", copy):
            fail(f"{name} treats a nested-screen title as the genre key")
        if re.search(r"showsForBrowseAll\(catalog\.entries,\s*state\.title\)", copy):
            fail(f"{name} treats a nested-screen title as the genre key")

    # Playback from the nested screen must walk the same rebuild so Back
    # (and a delta while playing) cannot widen the artist/show underneath.
    player = state_branch(body, "Player")
    if "replaceEmbeddedCatalog(state.previous, catalog)" not in player:
        fail("Player catalog delta does not rebuild the nested artist/show underneath")
    preparing = state_branch(body, "PreparingPlayback")
    if "replaceEmbeddedCatalog(state.previous, catalog)" not in preparing:
        fail("PreparingPlayback catalog delta does not rebuild the nested artist/show underneath")
    print("ok catalog delta rebuilds nested artist/show with inherited genre scope")


def assert_album_and_season_screens_render_rebuilt_group() -> None:
    activity = ACTIVITY.read_text()
    albums = activity.find("is UiState.ArtistAlbums")
    seasons = activity.find("is UiState.ShowSeasons")
    if albums == -1 or seasons == -1:
        fail("MainActivity no longer routes ArtistAlbums/ShowSeasons")
    album_block = activity[albums : activity.find("is UiState.", albums + 1)]
    season_block = activity[seasons : activity.find("is UiState.", seasons + 1)]
    if "state.artist" not in album_block:
        fail("AlbumScreen is not given the rebuilt ArtistAlbums artist after a catalog delta")
    if "state.show" not in season_block:
        fail("SeasonScreen is not given the rebuilt ShowSeasons show after a catalog delta")
    print("ok nested screens render the rebuilt artist/show group")


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


def assert_nested_delta_membership() -> None:
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
                "nested genre catalog-delta driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "NestedGenreCatalogDeltaDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "nested genre catalog-delta membership failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_nested_open_inherits_genre_scope_only_from_genre_shelf()
    assert_catalog_delta_rebuilds_nested_screens_with_inherited_scope()
    assert_album_and_season_screens_render_rebuilt_group()
    assert_nested_delta_membership()
    print("PASS: Issue #369 nested artist/show genre catalog-delta UAT")


if __name__ == "__main__":
    main()
