#!/usr/bin/env python3
"""Adversarial boundaries for issue #353: Browse All shows the selected category.

The issue: opening Browse All for a Movies/Shows/Music (or genre) category
must show that category's name at the top of the destination page, the same
way the catalog shelves label those rows.

This suite does not re-assert the earlier click-to-state path. It checks
invariants that would still let that path "pass" while the page lied:

- Top-level Movies/Shows/Music Browse All is the whole kind; a genre row
  with the same spelling is still that genre (scopedToGenre is the
  discriminator, not the heading text).
- The heading is pinned above the grid so scrolling does not drop the
  selected category.
- The heading uses the catalog shelf treatment (muted, black-weight,
  top-level size) and is a label only (physical Back dismisses).
- Refresh rebuilds from the retained title without rewriting it.
- Grid membership for a named category is that category: extras, other
  kinds, substring genre labels, and unknown names must not widen it.
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
CATALOG = APP / "ui/screens/CatalogScreen.kt"
ACTIVITY = APP / "MainActivity.kt"
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
BROWSE_ALL = APP / "data/BrowseAllShelf.kt"
HEADER = APP / "ui/screens/BrowseAllHeader.kt"
DRIVER = Path(__file__).with_name("BrowseAllCategoryTitleBoundariesDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"
SCREENS = {
    "MovieShelfScreen.kt": "BROWSE_ALL_MOVIES_TITLE",
    "ShowShelfScreen.kt": "BROWSE_ALL_SHOWS_TITLE",
    "ArtistShelfScreen.kt": "BROWSE_ALL_MUSIC_TITLE",
}


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)


def function_body(source: str, name: str) -> str:
    match = re.search(rf"(?:private |internal )?fun (?:<[^>]+>\s*)?{re.escape(name)}\s*\(", source)
    if not match:
        fail(f"missing {name}")
    start = match.start()
    nxt = source.find("\nprivate fun ", start + 1)
    nxt2 = source.find("\n@Composable", start + 1)
    cuts = [i for i in (nxt, nxt2) if i != -1]
    end = min(cuts) if cuts else len(source)
    return source[start:end]


def assert_top_level_is_not_inferred_from_heading() -> None:
    """Clicking Browse All on Movies must not filter to a genre named Movies."""
    catalog = CATALOG.read_text()
    constants = BROWSE_ALL.read_text()
    for const, literal in (
        ("BROWSE_ALL_MOVIES_TITLE", "Movies"),
        ("BROWSE_ALL_SHOWS_TITLE", "Shows"),
        ("BROWSE_ALL_MUSIC_TITLE", "Music"),
    ):
        if f'internal const val {const} = "{literal}"' not in constants:
            fail(f"{const} drifted from the catalog's {literal!r} row label")
        if f'"{literal}"' not in catalog:
            fail(f"CatalogScreen no longer labels the top-level row {literal!r}")

    # Top-level rows pass scopedToGenre=false; genre sub-shelves pass true.
    # Heading text is not the discriminator — a user genre can be "Movies".
    if "onOpenMovieShelf(title, rowMovies, false)" not in catalog:
        fail("top-level Movies Browse All is not marked as the whole kind")
    if "onOpenMovieShelf(title, rowMovies, true)" not in catalog:
        fail("genre Movies Browse All is not marked as genre-scoped")
    if "onOpenShowShelf(title, rowShows, false)" not in catalog:
        fail("top-level Shows Browse All is not marked as the whole kind")
    if "onOpenShowShelf(title, rowShows, true)" not in catalog:
        fail("genre Shows Browse All is not marked as genre-scoped")
    if "onOpenArtistShelf(title, rowArtists, false)" not in catalog:
        fail("top-level Music Browse All is not marked as the whole kind")
    if "onOpenArtistShelf(title, rowArtists, true)" not in catalog:
        fail("genre Music Browse All is not marked as genre-scoped")

    activity = ACTIVITY.read_text()
    for call in (
        "viewModel.openMovieShelf(movies, title, scopedToGenre)",
        "viewModel.openShowShelf(shows, title, scopedToGenre)",
        "viewModel.openArtistShelf(artists, title, scopedToGenre)",
    ):
        if call not in activity:
            fail(f"MainActivity does not forward title+scope as {call}")
    print("ok top-level vs genre scope is not inferred from the heading")


def assert_title_pinned_at_top_and_matches_catalog_header() -> None:
    header = HEADER.read_text()
    catalog = CATALOG.read_text()
    if "SwarmMuted" not in header or "FontWeight.Black" not in header:
        fail("Browse All title is not the muted black-weight catalog shelf treatment")
    top_level = re.search(r"private val TOP_LEVEL_TITLE_SIZE = (\d+)\.sp", catalog)
    browse_size = re.search(r"private val BROWSE_ALL_TITLE_SIZE = (\d+)\.sp", header)
    if not top_level or not browse_size:
        fail("could not compare Browse All title size to catalog Movies/Shows/Music headers")
    if top_level.group(1) != browse_size.group(1):
        fail(
            f"Browse All title is {browse_size.group(1)}.sp; catalog kind headers are "
            f"{top_level.group(1)}.sp"
        )
    if re.search(r"\b(clickable|focusRequester|FocusRequester|onClick)\b", header):
        fail("Browse All title is interactive; the issue asks for a visible category label")
    if re.search(r"""Text\(\s*["']Back["']""", header) or "Button(" in header:
        fail("Browse All header added on-screen Back chrome")

    for screen, default in SCREENS.items():
        text = (APP / "ui/screens" / screen).read_text()
        title_at = text.find("BrowseAllScreenTitle(title)")
        grid_at = text.find("LazyVerticalGrid(")
        empty_at = text.find("No ")
        if title_at == -1:
            fail(f"{screen} does not render the selected category name")
        if grid_at != -1 and title_at > grid_at:
            fail(f"{screen} puts the category name inside/after the grid, so it is not at the top")
        if empty_at != -1 and title_at > empty_at:
            fail(f"{screen} can show the empty-state copy without the category name")
        # Title must not be a grid item — it has to stay put while cards scroll.
        grid_block = text[grid_at : text.find("\n    }", grid_at)] if grid_at != -1 else ""
        if "BrowseAllScreenTitle" in grid_block:
            fail(f"{screen} places the category name inside LazyVerticalGrid (it would scroll away)")
        if "BackHandler(onBack = onBack)" not in text:
            fail(f"{screen} lost physical Back as the way to leave Browse All")
        if re.search(r"""Text\(\s*["']Back["']""", text):
            fail(f"{screen} added an on-screen Back button")
        if f"title: String = {default}" not in text:
            fail(f"{screen} no longer takes the originating category as title")
    print("ok title is pinned at the top with catalog-header treatment")


def assert_refresh_keeps_the_heading() -> None:
    model = VIEW_MODEL.read_text()
    start = model.find("private fun replaceEmbeddedCatalog(")
    if start == -1:
        fail("replaceEmbeddedCatalog is missing; a catalog delta cannot keep the Browse All heading")
    nxt = model.find("\n    /**", start + 1)
    body = model[start : nxt if nxt != -1 else len(model)]
    for state, rebuild in (
        ("MovieShelf", "moviesForBrowseAll(catalog.entries, state.title)"),
        ("ShowShelf", "showsForBrowseAll(catalog.entries, state.title)"),
        ("ArtistShelf", "artistsForBrowseAll(catalog.entries, state.title)"),
    ):
        marker = f"is UiState.{state} ->"
        at = body.find(marker)
        if at == -1:
            fail(f"replaceEmbeddedCatalog does not rebuild UiState.{state}")
        following = body.find("is UiState.", at + len(marker))
        copy = body[at : following if following != -1 else len(body)]
        if re.search(r"\btitle\s*=", copy):
            fail(f"a catalog delta overwrites UiState.{state}.title, dropping the selected category")
        if re.search(r"\bscopedToGenre\s*=", copy):
            fail(f"a catalog delta rewrites UiState.{state}.scopedToGenre")
        if rebuild not in copy:
            fail(f"genre-scoped {state} refresh does not rebuild from the retained title")
        if "if (state.scopedToGenre)" not in copy:
            fail(f"{state} refresh no longer distinguishes a genre page from the whole kind")
    print("ok catalog delta keeps the heading and rebuilds that same category")


def assert_shelf_membership_matches_refresh() -> None:
    """Genre shelves and Browse All refresh must use the same category key."""
    catalog = CATALOG.read_text()
    shelves = function_body(catalog, "topGenreShelves")
    if "it.entry.genres.contains(genre)" not in shelves:
        fail("genre shelves no longer key rows by the genre string shown as the row title")
    helpers = BROWSE_ALL.read_text()
    if helpers.count("it.entry.genres.contains(title)") < 3:
        fail("Browse All refresh helpers do not membership-match on the heading string")
    print("ok genre shelf title is the same key used to rebuild the grid")


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


def assert_membership_boundaries() -> None:
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
                "Browse All boundary driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "BrowseAllCategoryTitleBoundariesDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "Browse All category membership boundaries failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_top_level_is_not_inferred_from_heading()
    assert_title_pinned_at_top_and_matches_catalog_header()
    assert_refresh_keeps_the_heading()
    assert_shelf_membership_matches_refresh()
    assert_membership_boundaries()
    print("PASS: Issue #353 Browse All category-title boundary UAT")


if __name__ == "__main__":
    main()
