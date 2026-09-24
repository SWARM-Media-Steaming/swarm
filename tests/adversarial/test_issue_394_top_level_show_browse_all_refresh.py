#!/usr/bin/env python3
"""Adversarial UAT for Issue #394: top-level (un-scoped) Shows Browse All refresh.

CatalogScreen's own `val shows` grid (the un-scoped top-level Shows row,
`scopedToGenre = false`) groups episodes into shows and drops any group whose
`CatalogGrouping.previewSeasons` is empty, so extras-only titles never render
as "0 seasons" cards. `openShowShelf()`/`openShowSeasons()` build the same
un-scoped list when first opening that page, and `replaceEmbeddedCatalog`
rebuilds it from a fresh catalog while the viewer stays on the page.

The reported bug (found while testing #368, which only fixed the *genre*
Show Browse All path via `showsForBrowseAll`): the top-level branch used
`CatalogGrouping.groupEpisodesByShowSeason` directly on catalog-delta
rebuilds, with no `previewSeasons` drop, so a catalog delta arriving while a
viewer is on the un-scoped ShowShelf/ShowSeasons page could resurface a
season-0/extras-only group as a browsable "0 seasons" card.

This suite asserts every un-scoped call site — CatalogScreen's initial grid,
openShowShelf's default, openShowSeasons's Catalog branch, and every
`replaceEmbeddedCatalog` branch's `else` (non-genre) arm — routes through the
same `CatalogGrouping.browsableShows` helper, and that the helper's own
domain logic (season 0, null/negative season, unnumbered/negative episode,
canonical scrape merges, catalog-delta transitions) matches the drop the
genre path already established in #368.
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
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
GROUPING = CORE / "src/main/kotlin/app/swarm/tv/core/catalog/CatalogGrouping.kt"
DRIVER = Path(__file__).with_name("TopLevelShowBrowseAllRefreshDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"


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
    nxt2 = source.find("\ninternal fun ", start + 1)
    nxt3 = source.find("\n@Composable", start + 1)
    nxt4 = source.find("\n    fun ", start + 1)
    cuts = [i for i in (nxt, nxt2, nxt3, nxt4) if i != -1]
    end = min(cuts) if cuts else len(source)
    return source[start:end]


def assert_browsable_shows_helper_is_the_composition() -> None:
    grouping = GROUPING.read_text()
    fn = function_body(grouping, "browsableShows")
    if "groupEpisodesByShowSeason" not in fn:
        fail("browsableShows no longer groups episodes by show/season")
    if "previewSeasons(it).isNotEmpty()" not in fn:
        fail("browsableShows no longer drops groups with empty previewSeasons")
    preview = function_body(grouping, "previewSeasons")
    if "(season.season ?: 0) <= 0" not in preview:
        fail("previewSeasons must exclude season 0, null, and negative seasons")
    if "(it.entry.episode ?: 0) > 0" not in preview:
        fail("previewSeasons must require a numbered episode")
    print("ok browsableShows composes groupEpisodesByShowSeason + previewSeasons")


def assert_catalog_screen_top_level_grid_uses_browsable_shows() -> None:
    catalog = CATALOG.read_text()
    match = re.search(
        r"val shows = remember\(filtered\) \{(?P<body>.*?)\n\s*\}",
        catalog,
        re.S,
    )
    if not match:
        fail("CatalogScreen is missing the top-level `val shows` grid")
    body = match.group("body")
    if "CatalogGrouping.browsableShows(filtered)" not in body:
        fail(
            "CatalogScreen's top-level Shows grid no longer routes through "
            "CatalogGrouping.browsableShows; it can drift from the refresh path again"
        )
    print("ok CatalogScreen top-level Shows grid uses CatalogGrouping.browsableShows")


def assert_view_model_unscoped_branches_use_browsable_shows() -> None:
    model = VIEW_MODEL.read_text()

    start = model.find("private fun replaceEmbeddedCatalog(")
    if start == -1:
        fail("replaceEmbeddedCatalog is missing; a catalog delta cannot rebuild Browse All")
    nxt = model.find("\n    /**", start + 1)
    replace_body = model[start : nxt if nxt != -1 else len(model)]

    marker = "is UiState.ShowShelf ->"
    at = replace_body.find(marker)
    if at == -1:
        fail("replaceEmbeddedCatalog does not rebuild UiState.ShowShelf")
    following = replace_body.find("is UiState.", at + len(marker))
    show_shelf_branch = replace_body[at : following if following != -1 else len(replace_body)]
    if "if (state.scopedToGenre)" not in show_shelf_branch:
        fail("ShowShelf refresh no longer distinguishes a genre page from the top-level page")
    if "CatalogGrouping.browsableShows(catalog.entries)" not in show_shelf_branch:
        fail(
            "the top-level (else) branch of ShowShelf's catalog-delta rebuild no longer "
            "uses CatalogGrouping.browsableShows — this is the exact #394 regression"
        )
    if "groupEpisodesByShowSeason(catalog.entries)" in show_shelf_branch.split(
        "CatalogGrouping.browsableShows(catalog.entries)"
    )[0]:
        fail("ShowShelf's top-level branch still calls the unfiltered grouping directly")

    marker = "is UiState.ShowSeasons ->"
    at = replace_body.find(marker)
    if at == -1:
        fail("replaceEmbeddedCatalog does not rebuild UiState.ShowSeasons")
    following = replace_body.find("is UiState.", at + len(marker))
    show_seasons_branch = replace_body[at : following if following != -1 else len(replace_body)]
    if "?: CatalogGrouping.browsableShows(catalog.entries)" not in show_seasons_branch:
        fail(
            "the top-level (genreScope == null) branch of ShowSeasons' catalog-delta "
            "rebuild no longer uses CatalogGrouping.browsableShows"
        )
    print("ok replaceEmbeddedCatalog's ShowShelf/ShowSeasons top-level branches use browsableShows")

    open_show_shelf = function_body(model, "openShowShelf")
    if "else CatalogGrouping.browsableShows(current.entries)" not in open_show_shelf:
        fail("openShowShelf's top-level (scopedToGenre = false) default no longer uses browsableShows")

    open_show_seasons = function_body(model, "openShowSeasons")
    if "is UiState.Catalog -> previous to CatalogGrouping.browsableShows(previous.entries)" not in open_show_seasons:
        fail("openShowSeasons' UiState.Catalog branch no longer uses browsableShows")
    print("ok openShowShelf/openShowSeasons top-level defaults use browsableShows")


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


def assert_browsable_shows_domain_membership() -> None:
    classpath = core_classpath()
    with tempfile.TemporaryDirectory() as temp:
        out = Path(temp) / "classes"
        out.mkdir()
        compile_result = subprocess.run(
            kotlin_compiler() + ["-no-stdlib", "-no-reflect", "-cp", classpath, "-d", str(out), str(DRIVER)],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            fail(
                "top-level Show Browse All driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "TopLevelShowBrowseAllRefreshDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "top-level Show Browse All previewSeasons refresh failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_browsable_shows_helper_is_the_composition()
    assert_catalog_screen_top_level_grid_uses_browsable_shows()
    assert_view_model_unscoped_branches_use_browsable_shows()
    assert_browsable_shows_domain_membership()
    print("PASS: Issue #394 top-level Show Browse All previewSeasons refresh")


if __name__ == "__main__":
    main()
