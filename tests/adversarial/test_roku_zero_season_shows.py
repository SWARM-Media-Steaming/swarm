#!/usr/bin/env python3
"""Adversarial boundary tests for #360: Roku Shows() 0-season filter.

Complements tests/roku_catalog_grouping.py (the delivery's own test) with
cases that stress the boundary of IsPreviewEpisode/HasPreviewSeason rather
than re-asserting its happy path: multiple season-0 episodes, casing merges
where the filter-relevant entry isn't the one that sets the group's key,
partial-numbering within an otherwise real season, and a large synthetic
catalog checked group-by-group instead of by a couple of hand-picked shows.

There is no BrightScript runtime in this environment (see
tests/roku_catalog_grouping.py's docstring), so this file source-checks the
predicate functions in CatalogGrouping.bs and cross-checks a faithful Python
port of them against those boundary cases.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GROUPING = ROOT / "clients/tv-roku/src/source/CatalogGrouping.bs"


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def extract_function(src: str, name: str) -> str:
    match = re.search(
        rf"function {re.escape(name)}\b.*?^    end function",
        src,
        re.M | re.S,
    )
    if not match:
        fail(f"CatalogGrouping.bs is missing function {name}")
    return match.group(0)


def assert_source_contract() -> None:
    text = GROUPING.read_text()
    # The filter must reject on <= 0, not == 0 -- a negative season/episode
    # (malformed scrape data) must not slip through as "preview".
    preview_ep = extract_function(text, "IsPreviewEpisode")
    if "<= 0" not in preview_ep:
        fail("IsPreviewEpisode must reject non-positive season/episode with <= 0, not == 0")
    # HasPreviewSeason must scan *every* episode in the group, not just the
    # first one that set the group's title casing.
    has_preview = extract_function(text, "HasPreviewSeason")
    if "for each e in episodes" not in has_preview:
        fail("HasPreviewSeason must iterate every episode in the group")


# --- Faithful Python port of the boundary-relevant predicates ---

def is_preview_episode(e: dict) -> bool:
    season = e.get("season")
    episode = e.get("episode")
    if season is None or season <= 0:
        return False
    if episode is None or episode <= 0:
        return False
    return True


def has_preview_season(episodes: list[dict]) -> bool:
    return any(is_preview_episode(e) for e in episodes)


def shows(entries: list[dict]) -> list[dict]:
    by_title: dict[str, dict] = {}
    order: list[str] = []
    for e in entries:
        if e.get("kind") != "episode" or e.get("show_title") is None:
            continue
        key = e["show_title"].lower()
        if key not in by_title:
            by_title[key] = {"title": e["show_title"], "episodes": []}
            order.append(key)
        by_title[key]["episodes"].append(e)
    return [by_title[k] for k in order if has_preview_season(by_title[k]["episodes"])]


def ep(fp: str, show: str | None, season, episode) -> dict:
    return {"kind": "episode", "show_title": show, "season": season, "episode": episode, "fingerprint": fp}


def check(name: str, got, expected) -> None:
    if got != expected:
        fail(f"{name}: got={got!r} expected={expected!r}")
    print(f"ok {name}")


def run_boundary_cases() -> None:
    # Multiple season-0 episodes only -- still no real season, must be hidden.
    check(
        "multiple-season-zero-episodes-hidden",
        [g["title"] for g in shows([
            ep("s0e1", "Extras Only", 0, 1),
            ep("s0e2", "Extras Only", 0, 2),
            ep("s0e3", "Extras Only", 0, 3),
        ])],
        [],
    )

    # Negative season/episode (malformed scrape) must not count as preview.
    check(
        "negative-season-hidden",
        [g["title"] for g in shows([ep("neg", "Bad Scrape", -1, 1)])],
        [],
    )
    check(
        "negative-episode-hidden",
        [g["title"] for g in shows([ep("neg2", "Bad Scrape 2", 1, -3)])],
        [],
    )

    # Casing merge: the entry that sets the group's display title (first
    # seen) is the extras-only one; a later, differently-cased entry
    # carries the real season. The merge must still detect the preview
    # season regardless of which entry set the casing.
    merged = shows([
        ep("special", "the office", 0, 1),
        ep("real", "The Office", 1, 1),
    ])
    check("casing-merge-still-detects-preview", [g["title"] for g in merged], ["the office"])
    if len(merged[0]["episodes"]) != 2:
        fail(f"casing-merge must retain both entries: {merged}")
    print("ok casing-merge-retains-both-entries")

    # Reverse order: real season first, extras-only differently-cased entry
    # second -- must not accidentally get dropped by re-evaluating only the
    # last-seen entry.
    merged2 = shows([
        ep("real", "The Office", 1, 1),
        ep("special", "THE OFFICE", 0, 1),
    ])
    check("casing-merge-reverse-order", [g["title"] for g in merged2], ["The Office"])

    # Partial numbering within a real season: some episodes in season 1
    # lack episode numbers, but at least one is numbered -- show must
    # still appear (HasPreviewSeason only needs one qualifying episode).
    check(
        "partial-numbering-in-real-season-kept",
        [g["title"] for g in shows([
            ep("e-null", "Partial", 1, None),
            ep("e-1", "Partial", 1, 1),
            ep("e-0", "Partial", 1, 0),
        ])],
        ["Partial"],
    )

    # Large synthetic catalog: N shows with only season 0 / null, M shows
    # with a real season -- verify exact set survives, not just "some
    # survive" (guards against an off-by-one in the order/dedup logic).
    entries: list[dict] = []
    expected_survivors = []
    for i in range(25):
        title = f"Extras Show {i}"
        entries.append(ep(f"ex{i}-s0", title, 0, 1))
        entries.append(ep(f"ex{i}-null", title, None, None))
    for i in range(25):
        title = f"Real Show {i}"
        entries.append(ep(f"real{i}-s0", title, 0, 1))
        entries.append(ep(f"real{i}-s1", title, 1, 1))
        expected_survivors.append(title)
    got = [g["title"] for g in shows(entries)]
    check("large-catalog-exact-survivor-set", got, expected_survivors)


def main() -> None:
    assert_source_contract()
    run_boundary_cases()
    print("PASS: Roku Shows() 0-season filter boundary cases")


if __name__ == "__main__":
    main()
