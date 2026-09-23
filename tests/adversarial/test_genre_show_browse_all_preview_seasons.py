#!/usr/bin/env python3
"""Adversarial UAT for Issue #368: genre Show Browse All refresh.

A Shows genre shelf is grouped, then filtered with
CatalogGrouping.previewSeasons(it).isNotEmpty() so extras-only titles never
render as "0 seasons" cards. Opening Browse All from that shelf starts from
that filtered list. A later catalog delta rebuilds the grid via
showsForBrowseAll while the viewer is still on ShowShelf.

The rebuild must apply the same previewSeasons drop. Season 0, null/negative
seasons, and seasons without a numbered episode are extras, not browseable
shows. Genre membership is applied to entries before grouping, so a numbered
season in a different genre cannot keep extras of that show on this page.
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
BROWSE_ALL = APP / "data/BrowseAllShelf.kt"
GROUPING = CORE / "src/main/kotlin/app/swarm/tv/core/catalog/CatalogGrouping.kt"
DRIVER = Path(__file__).with_name("GenreShowBrowseAllPreviewSeasonsDriver.kt")
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


def assert_genre_shelf_drops_empty_preview_groups() -> None:
    catalog = CATALOG.read_text()
    genre_block = re.search(
        r"val showGenreShelves = remember\(filtered, genreFilter\) \{(?P<body>.*?)\n                        \}",
        catalog,
        re.S,
    )
    if not genre_block:
        fail("CatalogScreen is missing showGenreShelves")
    body = genre_block.group("body")
    if "groupEpisodesByShowSeason" not in body:
        fail("genre show shelves are not grouped from matching episodes")
    if "previewSeasons(it).isNotEmpty()" not in body:
        fail("genre show shelves no longer drop groups with empty previewSeasons")

    shelves = function_body(catalog, "topGenreShelves")
    if "it.entry.genres.contains(genre)" not in shelves:
        fail("genre shelves no longer key rows by the genre string shown as the row title")

    grouping = GROUPING.read_text()
    preview = function_body(grouping, "previewSeasons")
    if "(season.season ?: 0) <= 0" not in preview:
        fail("previewSeasons must exclude season 0, null, and negative seasons")
    if "(it.entry.episode ?: 0) > 0" not in preview:
        fail("previewSeasons must require a numbered episode")
    print("ok genre show shelves drop groups without preview seasons")


def assert_refresh_rebuilds_genre_show_shelf_with_preview_filter() -> None:
    helpers = BROWSE_ALL.read_text()
    shows_fn = function_body(helpers, "showsForBrowseAll")
    if "it.entry.genres.contains(title)" not in shows_fn:
        fail("showsForBrowseAll no longer membership-matches on the heading string")
    group_at = shows_fn.find("groupEpisodesByShowSeason")
    preview_at = shows_fn.find("previewSeasons")
    if group_at == -1:
        fail("showsForBrowseAll does not group episodes by show/season")
    if preview_at == -1:
        fail(
            "showsForBrowseAll omits the previewSeasons filter; a catalog delta "
            "can surface extras-only groups under the genre heading"
        )
    if preview_at < group_at:
        fail("showsForBrowseAll applies previewSeasons before grouping")
    if "previewSeasons(it).isNotEmpty()" not in shows_fn:
        fail("showsForBrowseAll does not drop groups with empty previewSeasons")

    model = VIEW_MODEL.read_text()
    start = model.find("private fun replaceEmbeddedCatalog(")
    if start == -1:
        fail("replaceEmbeddedCatalog is missing; a catalog delta cannot rebuild Browse All")
    nxt = model.find("\n    /**", start + 1)
    body = model[start : nxt if nxt != -1 else len(model)]
    marker = "is UiState.ShowShelf ->"
    at = body.find(marker)
    if at == -1:
        fail("replaceEmbeddedCatalog does not rebuild UiState.ShowShelf")
    following = body.find("is UiState.", at + len(marker))
    copy = body[at : following if following != -1 else len(body)]
    if re.search(r"\btitle\s*=", copy):
        fail("a catalog delta overwrites UiState.ShowShelf.title")
    if re.search(r"\bscopedToGenre\s*=", copy):
        fail("a catalog delta rewrites UiState.ShowShelf.scopedToGenre")
    if "if (state.scopedToGenre)" not in copy:
        fail("ShowShelf refresh no longer distinguishes a genre page from the whole kind")
    if "showsForBrowseAll(catalog.entries, state.title)" not in copy:
        fail("genre-scoped ShowShelf refresh does not rebuild through showsForBrowseAll")
    print("ok catalog delta rebuilds genre Show Browse All with previewSeasons")


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


def assert_refresh_membership() -> None:
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
                "genre Show Browse All driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "GenreShowBrowseAllPreviewSeasonsDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "genre Show Browse All previewSeasons refresh failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_genre_shelf_drops_empty_preview_groups()
    assert_refresh_rebuilds_genre_show_shelf_with_preview_filter()
    assert_refresh_membership()
    print("PASS: Issue #368 genre Show Browse All previewSeasons refresh")


if __name__ == "__main__":
    main()
