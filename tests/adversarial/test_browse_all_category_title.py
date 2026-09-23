#!/usr/bin/env python3
"""Adversarial UAT contract for Issue #353: Browse All retains its category.

The issue requires the destination page to display the category clicked in a
Movies, Shows, or Music shelf.  This suite checks the complete production data
path (row click -> state -> screen) plus a refresh boundary with category names
that collide with the three top-level heading strings.  Genre names are catalog
data, so they are not reserved words and must not change the selected subset.
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
DRIVER = Path(__file__).with_name("BrowseAllCategoryTitleDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def function_body(source: str, name: str) -> str:
    start = source.find(f"private fun {name}(")
    if start == -1:
        fail(f"CatalogScreen is missing {name}")
    next_function = source.find("\nprivate fun ", start + 1)
    return source[start : next_function if next_function != -1 else len(source)]


def assert_clicked_label_reaches_each_grid() -> None:
    catalog = CATALOG.read_text()
    for row, callback in (
        ("MovieRow", "onOpenShelf(title, movies)"),
        ("ShowShelfRow", "onOpenShowShelf(title, shows)"),
        ("ArtistShelfRow", "onOpenArtistShelf(title, artists)"),
    ):
        if callback not in function_body(catalog, row):
            fail(f"{row}'s Browse All tile does not pass its row title to the destination")

    activity = ACTIVITY.read_text()
    for screen, state_title in (
        ("MovieShelfScreen(", "title = state.title"),
        ("ShowShelfScreen(", "title = state.title"),
        ("ArtistShelfScreen(", "title = state.title"),
    ):
        block_start = activity.find(screen)
        block = activity[block_start : activity.find("\n            }", block_start)]
        if block_start == -1 or state_title not in block:
            fail(f"{screen} is not given the title retained in UI state")

    model = VIEW_MODEL.read_text()
    for state in ("ArtistShelf", "MovieShelf", "ShowShelf"):
        if not re.search(rf"data class {state}\([^\n]*val title: String", model):
            fail(f"UiState.{state} does not retain the clicked category title")
    print("ok row-click-to-state-to-grid title path")


def assert_all_grids_render_the_label_even_when_empty() -> None:
    header = HEADER.read_text()
    if not re.search(r"Text\(\s*title,", header):
        fail("Browse All header does not render the supplied title verbatim")
    if "UatTestTags.BROWSE_ALL_TITLE" not in header:
        fail("Browse All header lacks a stable UAT semantics tag")

    for screen in ("MovieShelfScreen.kt", "ShowShelfScreen.kt", "ArtistShelfScreen.kt"):
        text = (APP / "ui/screens" / screen).read_text()
        heading = text.find("BrowseAllScreenTitle(title)")
        # There are earlier focus guards using isEmpty(); only the content
        # branch after the heading decides whether the empty-state copy or
        # the grid is displayed.
        empty = text.find(".isEmpty())", heading)
        if heading == -1:
            fail(f"{screen} does not render the Browse All category heading")
        if empty == -1 or heading > empty:
            fail(f"{screen} hides the category heading when a catalog refresh leaves no entries")
    print("ok all three grids render title before empty state")


def jars_named(name: str) -> list[str]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return [str(p) for p in sorted(cache.glob(f"**/{name}-*.jar")) if "sources" not in p.name and "javadoc" not in p.name]


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


def assert_refresh_keeps_clicked_category() -> None:
    # Compile the production BrowseAllShelf source in the same Kotlin module
    # as the driver. This executes its actual refresh filtering, not a Python
    # reimplementation of it.
    classpath = core_classpath()
    with tempfile.TemporaryDirectory() as temp:
        out = Path(temp) / "classes"
        out.mkdir()
        compile_result = subprocess.run(
            kotlin_compiler() + ["-no-stdlib", "-no-reflect", "-cp", classpath, "-d", str(out), str(BROWSE_ALL), str(DRIVER)],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            fail(f"Browse All driver compilation failed:\n{compile_result.stdout}\n{compile_result.stderr}")
        run_result = subprocess.run(["java", "-cp", f"{out}:{classpath}", "BrowseAllCategoryTitleDriverKt"], capture_output=True, text=True)
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(f"clicked-category refresh invariant failed:\n{run_result.stdout}\n{run_result.stderr}")
        print(run_result.stdout.strip())


def main() -> None:
    assert_clicked_label_reaches_each_grid()
    assert_all_grids_render_the_label_even_when_empty()
    assert_refresh_keeps_clicked_category()
    print("PASS: Issue #353 Browse All category-title UAT contract")


if __name__ == "__main__":
    main()
