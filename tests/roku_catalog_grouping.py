#!/usr/bin/env python3
"""Roku CatalogGrouping.Shows / FirstEpisode contracts.

Issue #360: Fire TV search/browse hides groups with empty previewSeasons
(season 0, null season, episode <= 0). Roku grouping lives in
clients/tv-roku/src/source/CatalogGrouping.bs.

Issue #363: FirstEpisode must not pick a season-0 special over the real
S1E1 premiere when selecting a show row (v1 plays first episode directly).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GROUPING = ROOT / "clients/tv-roku/src/source/CatalogGrouping.bs"
CATALOG_SCREEN = ROOT / "clients/tv-roku/src/components/screens/CatalogScreen.bs"


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
    shows = extract_function(text, "Shows")
    if "HasPreviewSeason(group.episodes)" not in shows:
        fail("Shows() does not drop groups with HasPreviewSeason(group.episodes)")
    if re.search(r"result\.Push\(byTitle\[key\]\)", shows):
        fail("Shows() still pushes every grouped title without a preview-season check")

    preview_ep = extract_function(text, "IsPreviewEpisode")
    if "e.season = invalid or e.season <= 0" not in preview_ep:
        fail("IsPreviewEpisode must exclude season 0 and unnumbered seasons")
    if "e.episode = invalid or e.episode <= 0" not in preview_ep:
        fail("IsPreviewEpisode must require a numbered episode (> 0)")

    has_preview = extract_function(text, "HasPreviewSeason")
    if "IsPreviewEpisode(e)" not in has_preview:
        fail("HasPreviewSeason must scan episodes with IsPreviewEpisode")

    extract_function(text, "PreviewSeasons")

    first_ep = extract_function(text, "FirstEpisode")
    if "IsPreviewEpisode(e)" not in first_ep:
        fail("FirstEpisode must prefer IsPreviewEpisode (season > 0, episode > 0) over season 0")
    if "isExtra" not in first_ep:
        fail("FirstEpisode must still skip extra_type bonus files")

    catalog = CATALOG_SCREEN.read_text()
    if "Swarm.CatalogGrouping.Shows(entries)" not in catalog:
        fail("CatalogScreen no longer builds the shows shelf from CatalogGrouping.Shows")
    if "Swarm.CatalogGrouping.FirstEpisode" not in catalog:
        fail("CatalogScreen no longer plays a show via CatalogGrouping.FirstEpisode")


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


def preview_seasons(episodes: list[dict]) -> list[int]:
    by_season: dict[int, list[dict]] = {}
    order: list[int] = []
    for e in episodes:
        season = e.get("season")
        episode = e.get("episode")
        if season is None or season <= 0:
            continue
        if episode is None or episode <= 0:
            continue
        if season not in by_season:
            by_season[season] = []
            order.append(season)
        by_season[season].append(e)
    return [s for s in order if by_season[s]]


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


def matches_search(entry: dict, query: str) -> bool:
    q = query.strip().lower()
    if not q:
        return True
    fields = [
        entry.get("scraped_title"),
        entry.get("title"),
        entry.get("artist"),
        entry.get("album"),
        entry.get("show_title"),
    ]
    return any(f is not None and q in str(f).lower() for f in fields)


def search_show_titles(entries: list[dict], query: str) -> list[str]:
    return [g["title"] for g in shows([e for e in entries if matches_search(e, query)])]


def first_episode(episodes: list[dict]) -> dict | None:
    """Mirror CatalogGrouping.FirstEpisode: extras never win; preview
    episodes (season > 0 and episode > 0) beat season-0 specials; then
    lowest (season, episode) with unnumbered last.
    """
    def season_key(e: dict) -> int:
        s = e.get("season")
        return 999999 if s is None else s

    def episode_key(e: dict) -> int:
        n = e.get("episode")
        return 999999 if n is None else n

    def is_extra(e: dict) -> bool:
        extra = e.get("extra_type")
        return extra is not None and extra != ""

    eligible = [e for e in episodes if not is_extra(e)]
    preview = [e for e in eligible if is_preview_episode(e)]
    pool = preview if preview else eligible
    if not pool:
        return None
    return min(pool, key=lambda e: (season_key(e), episode_key(e)))


def episode(fp: str, show: str | None, season: int | None, ep: int | None, title: str | None = None) -> dict:
    return {
        "kind": "episode",
        "title": title or fp,
        "show_title": show,
        "season": season,
        "episode": ep,
        "fingerprint": fp,
    }


def check(name: str, got, expected) -> None:
    if got != expected:
        fail(f"{name}: got={got!r} expected={expected!r}")
    print(f"ok {name}")


def run_grouping_cases() -> None:
    check(
        "specials-only-hidden",
        [g["title"] for g in shows([
            episode("s0e1", "Dexter", 0, 1, "Christmas Special"),
            episode("feat", "Dexter", 2, None, "Making Of"),
            episode("loose", "Dexter", None, 1, "Interview"),
            episode("ep0", "Dexter", 1, 0, "Pilot Recap"),
        ])],
        [],
    )
    check(
        "mixed-real-season-kept",
        [g["title"] for g in shows([
            episode("s0e1", "Dexter", 0, 1, "Christmas Special"),
            episode("s1e1", "Dexter", 1, 1, "Dexter"),
            episode("feat", "Dexter", 2, None, "Making Of"),
        ])],
        ["Dexter"],
    )
    mixed = shows([
        episode("s0e1", "Dexter", 0, 1, "Christmas Special"),
        episode("s1e1", "Dexter", 1, 1, "Dexter"),
        episode("feat", "Dexter", 2, None, "Making Of"),
    ])
    if len(mixed) != 1 or [e["fingerprint"] for e in mixed[0]["episodes"]] != ["s0e1", "s1e1", "feat"]:
        fail(f"mixed group must retain extras: {mixed}")
    print("ok mixed-retains-extras")

    check(
        "only-shows-with-real-seasons",
        [g["title"] for g in shows([
            episode("ghost-special", "Ghost Show", 0, 1, "needle special"),
            episode("real", "Real Show", 1, 1, "needle episode"),
        ])],
        ["Real Show"],
    )
    check(
        "season-zero-alone-hidden",
        [g["title"] for g in shows([episode("s0e1", "Lost", 0, 1)])],
        [],
    )
    check(
        "unnumbered-episodes-in-s1-hidden",
        [g["title"] for g in shows([
            episode("e0", "Wire", 1, 0),
            episode("enull", "Wire", 1, None),
            episode("eneg", "Wire", 1, -1),
        ])],
        [],
    )
    two = shows([
        episode("s1e1", "Wire", 1, 1),
        episode("s2e1", "Wire", 2, 1),
    ])
    n = len(preview_seasons(two[0]["episodes"]))
    if n != 2:
        fail(f"two-seasons count={n}")
    print("ok two-seasons")

    check("empty", [g["title"] for g in shows([])], [])

    library = [
        episode("s1e1", "Dexter", 1, 1, "Pilot"),
        episode("s0e1", "Dexter", 0, 1, "Christmas Special"),
        episode("feat", "Dexter", 2, None, "Making Of"),
        episode("ghost", "Ghost Show", 0, 1, "Holiday Special"),
        episode("ghost-feat", "Ghost Show", None, None, "Behind the Scenes"),
        episode("wire", "The Wire", 1, 1, "The Target"),
        episode("wire-s2", "The Wire", 2, 1, "Ebb Tide"),
        {"kind": "movie", "title": "Making Movies", "show_title": None, "season": None, "episode": None},
    ]
    check("query-extra-title-hides", search_show_titles(library, "Making"), [])
    check("query-special-title-hides-ghost", search_show_titles(library, "Holiday"), [])
    check("query-show-name-extras-only", search_show_titles(library, "Ghost"), [])
    check("query-real-episode", search_show_titles(library, "Pilot"), ["Dexter"])
    check("query-show-name-with-seasons", search_show_titles(library, "dexter"), ["Dexter"])
    check("query-trim-case", search_show_titles(library, "  The Wire  "), ["The Wire"])
    check("movie-not-a-show", search_show_titles(library, "Making Movies"), [])

    wire = shows([e for e in library if matches_search(e, "wire")])
    labels = [f"{len(preview_seasons(g['episodes']))} season" + ("" if len(preview_seasons(g["episodes"])) == 1 else "s") for g in wire]
    if labels != ["2 seasons"]:
        fail(f"wire-label: {labels}")
    print("ok wire-label")
    dexter = shows([e for e in library if matches_search(e, "dexter")])
    dexter_labels = [f"{len(preview_seasons(g['episodes']))} season" + ("" if len(preview_seasons(g["episodes"])) == 1 else "s") for g in dexter]
    if dexter_labels != ["1 season"] or any(l.startswith("0 ") for l in dexter_labels):
        fail(f"dexter-label: {dexter_labels}")
    print("ok dexter-label")


def run_first_episode_cases() -> None:
    special = episode("s0e1", "Show", 0, 1, "Christmas Special")
    premiere = episode("s1e1", "Show", 1, 1, "Pilot")
    s1e2 = episode("s1e2", "Show", 1, 2, "Second")
    extra = dict(episode("feat", "Show", 0, 1, "Making Of"), extra_type="featurette")

    check(
        "first-episode-skips-season-0-special",
        first_episode([special, premiere])["fingerprint"],
        "s1e1",
    )
    check(
        "first-episode-issue-363-repro",
        first_episode([
            {"season": 0, "episode": 1, "extra_type": None, "fingerprint": "special"},
            {"season": 1, "episode": 1, "extra_type": None, "fingerprint": "premiere"},
        ])["fingerprint"],
        "premiere",
    )
    check(
        "first-episode-lowest-numbered-season",
        first_episode([s1e2, premiere, special])["fingerprint"],
        "s1e1",
    )
    check(
        "first-episode-skips-extra-type",
        first_episode([extra, premiere])["fingerprint"],
        "s1e1",
    )
    check(
        "first-episode-specials-only-fallback",
        first_episode([special])["fingerprint"],
        "s0e1",
    )
    if first_episode([extra]) is not None:
        fail("first-episode-extras-only must return None")
    print("ok first-episode-extras-only")


def main() -> None:
    assert_source_contract()
    run_grouping_cases()
    run_first_episode_cases()
    print("PASS: Roku Shows() hides 0-season show groups; FirstEpisode skips season 0")


if __name__ == "__main__":
    main()
