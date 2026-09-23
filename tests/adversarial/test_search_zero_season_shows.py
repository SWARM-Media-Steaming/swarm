#!/usr/bin/env python3
"""Issue #351: search must not surface TV show cards with 0 seasons.

Amazon TV (tv-android) labels show cards with CatalogGrouping.previewSeasons
size. Specials (season 0), null seasons, and unnumbered/zero episodes are
not preview seasons, so a search that only matches those files would render
"0 seasons". Those groups must be omitted from search (and the search-fed
genre shelves). Shows that still have a numbered season with a numbered
episode must remain.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CATALOG_SCREEN = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui/screens/CatalogScreen.kt"
GROUPING = ROOT / "clients/tv-android/core/src/main/kotlin/app/swarm/tv/core/catalog/CatalogGrouping.kt"
DRIVER = Path(__file__).resolve().parent / "SearchZeroSeasonDriver.kt"
GRADLEW = ROOT / "clients/tv-android/gradlew"


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def assert_catalog_screen_hides_empty_preview_groups() -> None:
    text = CATALOG_SCREEN.read_text()
    # Search-derived show shelf: group then drop groups with no preview seasons.
    shows_block = re.search(
        r"val shows = remember\(filtered\) \{(?P<body>.*?)\n                        \}",
        text,
        re.S,
    )
    if not shows_block:
        fail("CatalogScreen is missing the search/browse `val shows` grouping block")
    body = shows_block.group("body")
    if "groupEpisodesByShowSeason(filtered)" not in body:
        fail("search show cards are not grouped from the filtered catalog")
    if "previewSeasons(it).isNotEmpty()" not in body:
        fail(
            "search show cards do not drop groups with empty previewSeasons "
            "(those render as '0 seasons')"
        )

    genre_block = re.search(
        r"val showGenreShelves = remember\(filtered, genreFilter\) \{(?P<body>.*?)\n                        \}",
        text,
        re.S,
    )
    if not genre_block:
        fail("CatalogScreen is missing showGenreShelves")
    gbody = genre_block.group("body")
    if "previewSeasons(it).isNotEmpty()" not in gbody:
        fail("genre show shelves still surface 0-season groups during search")

    # Card subtitle uses previewSeasons size — empty list is the "0 seasons" label.
    if "CatalogGrouping.previewSeasons(it).size" not in text and "previewSeasons(show).size" not in text:
        fail("show cards no longer derive season count from previewSeasons")

    # The shelf subtitle is "<n> season(s)" from that count. A visible card
    # with n==0 is exactly the reported "0 seasons" bug.
    if 'subtitle = "${realSeasonCounts[index]} season"' not in text and "realSeasonCounts[index]" not in text:
        fail("ShowShelfRow no longer labels cards from preview season counts")

    # Search match fields: a query can hit showTitle/title without hitting a
    # numbered episode, which is how 0-season groups appear after grouping.
    if "e.showTitle" not in text or "appliedSearchQuery" not in text:
        fail("catalog search no longer matches showTitle / applied query")


def assert_preview_seasons_contract_in_grouping() -> None:
    text = GROUPING.read_text()
    if "fun previewSeasons" not in text:
        fail("CatalogGrouping.previewSeasons is missing; season counts cannot be derived")
    # Domain: season 0 / null and episode <= 0 are not preview seasons.
    if "(season.season ?: 0) <= 0" not in text:
        fail("previewSeasons must exclude season 0 and unnumbered seasons")
    if "(it.entry.episode ?: 0) > 0" not in text:
        fail("previewSeasons must require a numbered episode")


def compile_classpath() -> str:
    cmd = [
        str(GRADLEW),
        "-p",
        str(ROOT / "clients/tv-android"),
        ":core:compileKotlin",
        "-q",
    ]
    subprocess.run(cmd, check=True, cwd=ROOT)
    classes = ROOT / "clients/tv-android/core/build/classes/kotlin/main"
    if not classes.is_dir():
        fail(f"kotlin classes missing at {classes}")
    jars: list[str] = [str(classes)]
    needed = [
        "kotlin-stdlib",
        "kotlin-stdlib-jdk8",
        "kotlin-stdlib-jdk7",
        "kotlinx-serialization-core-jvm",
        "kotlinx-serialization-json-jvm",
        "kotlinx-coroutines-core-jvm",
        "annotations",
    ]
    for name in needed:
        jars.extend(_jars_named(name))
    # de-dupe preserve order
    seen: set[str] = set()
    out: list[str] = []
    for j in jars:
        if j not in seen:
            seen.add(j)
            out.append(j)
    return ":".join(out)


def _jars_named(name: str) -> list[str]:
    cache = Path.home() / ".gradle" / "caches" / "modules-2" / "files-2.1"
    found = sorted(cache.glob(f"**/{name}-*.jar"))
    return [str(p) for p in found if "sources" not in p.name and "javadoc" not in p.name]


def find_k2jvm() -> list[str]:
    embed = _jars_named("kotlin-compiler-embeddable")
    if not embed:
        fail("kotlin-compiler-embeddable jar not in gradle cache")
    extras = []
    for name in (
        "kotlin-stdlib",
        "kotlin-reflect",
        "kotlin-script-runtime",
        "kotlinx-coroutines-core-jvm",
        "annotations",
        "trove4j",
    ):
        extras.extend(_jars_named(name)[:3])
    cp = ":".join([embed[-1], *extras])
    return ["java", "-cp", cp, "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler"]


def run_grouping_driver() -> None:
    cp = compile_classpath()
    compiler = find_k2jvm()
    with tempfile.TemporaryDirectory() as td:
        out = Path(td) / "out"
        out.mkdir()
        compile = compiler + [
            "-no-stdlib",
            "-no-reflect",
            "-cp",
            cp,
            "-d",
            str(out),
            str(DRIVER),
        ]
        r = subprocess.run(compile, capture_output=True, text=True)
        if r.returncode != 0:
            fail(f"driver compile failed:\n{r.stdout}\n{r.stderr}")
        run = subprocess.run(
            ["java", "-cp", f"{out}:{cp}", "SearchZeroSeasonDriverKt"],
            capture_output=True,
            text=True,
        )
        if r.returncode != 0 or run.returncode != 0 or "ALL_OK" not in run.stdout:
            fail(f"driver run failed:\n{run.stdout}\n{run.stderr}")
        print(run.stdout.strip())


def main() -> None:
    assert_preview_seasons_contract_in_grouping()
    assert_catalog_screen_hides_empty_preview_groups()
    run_grouping_driver()
    print("PASS: search hides 0-season show cards")


if __name__ == "__main__":
    main()
