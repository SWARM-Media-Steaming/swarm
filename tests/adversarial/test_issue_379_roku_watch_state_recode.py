#!/usr/bin/env python3
"""Adversarial boundary tests for #379: Roku watch state show/season/episode
fallback (Registry.bs GetWatchState/SetWatchState, CatalogScreen.bs
resumePositionSecsFor).

Issue #379 (an adversarial finding against #356) requires Roku watch state
to survive a recode that changes a show's episode file fingerprint, the same
guarantee Fire TV's WatchState.kt already provides via `stateFor` (see
clients/tv-android/core/src/main/kotlin/app/swarm/tv/core/watch/WatchState.kt).
The delivered patch adds a showTitle/season/episode identity to
Swarm.Registry's watch-state records and a fallback scan in GetWatchState.

There is no BrightScript runtime in this environment (see
tests/roku_catalog_grouping.py's docstring: `bsc` -- present in
clients/tv-roku/node_modules -- transpiles and type-checks .bs source but
does not execute it; no `brs`/rooibos interpreter is installed here), so
this file source-checks the delivered functions and cross-checks a faithful
Python port against boundary cases that go beyond the delivery's own happy
path: watched episodes must not resume via the fallback, case/whitespace
normalization must round-trip, ties between multiple stale identity matches
must resolve to the most recently updated one, pre-#379 legacy records
(missing the identity keys entirely, not just null) must not spuriously
match or crash, movies must never engage the fallback, and -- the
cross-file invariant tying this delivery to #360/#363 -- CatalogGrouping's
FirstEpisode() must never hand CatalogScreen's resumePositionSecsFor() a
season/episode pair that is invalid for a show that survived the Shows()
preview-season filter, since GetWatchState's fallback path is only reachable
through that call.
"""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REGISTRY = ROOT / "clients/tv-roku/src/source/Registry.bs"
CATALOG_SCREEN = ROOT / "clients/tv-roku/src/components/screens/CatalogScreen.bs"
PLAYER_SCREEN = ROOT / "clients/tv-roku/src/components/screens/PlayerScreen.bs"
MOVIE_DETAIL_SCREEN = ROOT / "clients/tv-roku/src/components/screens/MovieDetailScreen.bs"

_spec = importlib.util.spec_from_file_location(
    "roku_catalog_grouping", ROOT / "tests/roku_catalog_grouping.py"
)
_grouping = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
_spec.loader.exec_module(_grouping)
shows = _grouping.shows
first_episode = _grouping.first_episode
episode = _grouping.episode


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def check(name: str, got, expected) -> None:
    if got != expected:
        fail(f"{name}: got={got!r} expected={expected!r}")
    print(f"ok {name}")


def extract_block(src: str, keyword: str, name: str, end: str) -> str:
    match = re.search(
        rf"^([ \t]*){keyword} {re.escape(name)}\b.*?^\1{end}",
        src,
        re.M | re.S,
    )
    if not match:
        fail(f"missing {keyword} {name}")
    return match.group(0)


# --- source contract -------------------------------------------------------

def assert_source_contract() -> None:
    registry = REGISTRY.read_text()

    get_state = extract_block(registry, "function", "GetWatchState", "end function")
    if "all.DoesExist(fingerprint)" not in get_state:
        fail("GetWatchState must still look up by fingerprint first")
    if "MatchesEpisodeIdentity" not in get_state:
        fail("GetWatchState must fall back to a show/season/episode identity scan")
    if "candidate.updatedAt > best.updatedAt" not in get_state:
        fail("GetWatchState fallback must keep the most recently updated match, not the first")

    matches = extract_block(registry, "function", "MatchesEpisodeIdentity", "end function")
    if "entry.showTitle = invalid or entry.season = invalid or entry.episode = invalid" not in matches:
        fail("MatchesEpisodeIdentity must reject entries missing the identity fields (legacy records)")
    if "LCase(showTitle.Trim())" not in matches:
        fail("MatchesEpisodeIdentity must normalize the queried show title the same way SetWatchState stores it")

    set_state = extract_block(registry, "sub", "SetWatchState", "end sub")
    if "showTitle <> invalid and season <> invalid and episode <> invalid" not in set_state:
        fail("SetWatchState must only persist the identity when all three fields are supplied")

    catalog = CATALOG_SCREEN.read_text()
    if "GetWatchState(entry.fingerprint, entry.show_title, entry.season, entry.episode)" not in catalog:
        fail("CatalogScreen must query GetWatchState with the show's fingerprint and identity")
    resume_fn = extract_block(catalog, "function", "resumePositionSecsFor", "end function")
    if "not saved.watched" not in resume_fn:
        fail("resumePositionSecsFor must not resume a watched episode via the identity fallback")
    if "saved.positionSecs > 0.0" not in resume_fn:
        fail("resumePositionSecsFor must not resume a zero/negative saved position")

    player = PLAYER_SCREEN.read_text()
    if "SetWatchState(m.entry.fingerprint, absolutePosition, durationSecs, m.entry.show_title, m.entry.season, m.entry.episode)" not in player:
        fail("PlayerScreen must snapshot show_title/season/episode alongside every saved position")

    movie_detail = MOVIE_DETAIL_SCREEN.read_text()
    if "GetWatchState(m.entry.fingerprint)" not in movie_detail:
        fail("MovieDetailScreen must not pass an episode identity into GetWatchState for movies")


# --- faithful Python port of the delivered Registry.bs logic ---------------

def norm(title: str) -> str:
    return title.strip().lower()


def matches_episode_identity(entry: dict, show_title: str, season: int, episode_num: int) -> bool:
    e_show = entry.get("showTitle")
    e_season = entry.get("season")
    e_episode = entry.get("episode")
    if e_show is None or e_season is None or e_episode is None:
        return False
    return e_show == norm(show_title) and e_season == season and e_episode == episode_num


def get_watch_state(store: dict, fingerprint: str, show_title=None, season=None, episode_num=None):
    if fingerprint in store:
        return store[fingerprint]
    if show_title is None or season is None or episode_num is None:
        return None
    best = None
    for candidate in store.values():
        if matches_episode_identity(candidate, show_title, season, episode_num):
            if best is None or candidate["updatedAt"] > best["updatedAt"]:
                best = candidate
    return best


def set_watch_state(store: dict, fingerprint: str, position_secs: float, duration_secs: float,
                     updated_at: int, show_title=None, season=None, episode_num=None) -> None:
    watched = duration_secs > 0.0 and (position_secs / duration_secs) >= 0.95
    record = {
        "positionSecs": position_secs,
        "durationSecs": duration_secs,
        "watched": watched,
        "updatedAt": updated_at,
        "showTitle": None,
        "season": None,
        "episode": None,
    }
    if show_title is not None and season is not None and episode_num is not None:
        record["showTitle"] = norm(show_title)
        record["season"] = season
        record["episode"] = episode_num
    store[fingerprint] = record


def resume_position_secs_for(store: dict, entry: dict) -> float:
    saved = get_watch_state(
        store, entry["fingerprint"], entry.get("show_title"), entry.get("season"), entry.get("episode")
    )
    if saved is not None and not saved["watched"] and saved["positionSecs"] > 0.0:
        return saved["positionSecs"]
    return 0.0


# --- boundary cases ----------------------------------------------------------

def run_recode_fallback_cases() -> None:
    # Core issue repro: overnight recode changes the fingerprint; the
    # in-progress episode must still resume via show/season/episode.
    store: dict = {}
    set_watch_state(store, "fp-old", 600.0, 1800.0, 1000, "The Office", 2, 5)
    resumed = get_watch_state(store, "fp-new-after-recode", "The Office", 2, 5)
    check("recode-fallback-finds-progress", resumed["positionSecs"], 600.0)

    # A watched episode must not silently "resume" a completed episode
    # after a recode -- CatalogScreen's own watched/position>0 gate applies.
    store2: dict = {}
    set_watch_state(store2, "fp-old", 1780.0, 1800.0, 1000, "Finished Show", 1, 1)
    entry = {"fingerprint": "fp-new", "show_title": "Finished Show", "season": 1, "episode": 1}
    check("watched-episode-not-resumed-after-recode", resume_position_secs_for(store2, entry), 0.0)

    # Zero saved position (e.g. a state written before any real playback)
    # must not be surfaced as a resumable position either.
    store3: dict = {}
    set_watch_state(store3, "fp-old", 0.0, 1800.0, 1000, "Never Started", 1, 1)
    entry3 = {"fingerprint": "fp-new", "show_title": "Never Started", "season": 1, "episode": 1}
    check("zero-position-not-resumed-after-recode", resume_position_secs_for(store3, entry3), 0.0)

    # Case/whitespace differences between the write-time and read-time show
    # title must not defeat the match (mirrors Fire TV's trim+lowercase).
    store4: dict = {}
    set_watch_state(store4, "fp-old", 300.0, 1200.0, 1000, "  The OFFICE  ", 1, 1)
    resumed4 = get_watch_state(store4, "fp-new", "the office", 1, 1)
    check("case-and-whitespace-normalized", resumed4["positionSecs"] if resumed4 else None, 300.0)

    # Multiple stale entries share the identity (e.g. two prior recodes);
    # the most recently updated one must win, not the first Keys() order.
    store5: dict = {}
    set_watch_state(store5, "fp-oldest", 100.0, 1200.0, 500, "Multi", 1, 1)
    set_watch_state(store5, "fp-older", 900.0, 1200.0, 2000, "Multi", 1, 1)
    set_watch_state(store5, "fp-newest", 450.0, 1200.0, 3000, "Multi", 1, 1)
    resumed5 = get_watch_state(store5, "fp-current", "Multi", 1, 1)
    check("tie-break-picks-most-recently-updated", resumed5["positionSecs"], 450.0)

    # Pre-#379 legacy records have no showTitle/season/episode keys at all
    # (not merely None) -- must not spuriously match and must not crash.
    store6 = {"fp-legacy": {"positionSecs": 42.0, "durationSecs": 1200.0, "watched": False, "updatedAt": 999}}
    resumed6 = get_watch_state(store6, "fp-new", "Legacy Show", 1, 1)
    check("legacy-record-without-identity-keys-does-not-match", resumed6, None)

    # A different show that coincidentally shares season/episode numbers
    # must never match.
    store7: dict = {}
    set_watch_state(store7, "fp-a", 500.0, 1200.0, 1000, "Show A", 1, 1)
    resumed7 = get_watch_state(store7, "fp-b", "Show B", 1, 1)
    check("different-show-same-season-episode-does-not-match", resumed7, None)

    # Movies must never engage the identity fallback (MovieDetailScreen.bs
    # only ever calls GetWatchState with the fingerprint).
    store8: dict = {}
    set_watch_state(store8, "fp-movie-old", 100.0, 6000.0, 1000)
    resumed8 = get_watch_state(store8, "fp-movie-new")
    check("movie-fingerprint-only-lookup-misses-after-recode", resumed8, None)


def run_first_episode_identity_invariant() -> None:
    """Cross-file invariant: any show group that survives CatalogGrouping's
    Shows() preview-season filter (#360/#363) must have FirstEpisode()
    return an episode with a real, non-invalid season/episode -- because
    that pair is exactly what CatalogScreen.resumePositionSecsFor() feeds
    into GetWatchState's identity fallback. If either file regresses this
    independently, the fallback would silently degrade to fingerprint-only
    again (GetWatchState's own invalid-guard swallows it without error).
    """
    scenarios = [
        [episode("s0e1", "Show", 0, 1), episode("s1e1", "Show", 1, 1)],
        [episode("s1e2", "Show", 1, 2), episode("s1e1", "Show", 1, 1), episode("s0e1", "Show", 0, 1)],
        [episode("s2e1", "Show", 2, 1), episode("s1e1", "Show", 1, 1)],
    ]
    for eps in scenarios:
        groups = shows(eps)
        if len(groups) != 1:
            fail(f"expected exactly one surviving show group for {eps}, got {groups}")
        best = first_episode(groups[0]["episodes"])
        if best is None or best.get("season") is None or best.get("episode") is None:
            fail(f"FirstEpisode returned an invalid season/episode for a Shows()-surviving group: {best}")
        if best["season"] <= 0 or best["episode"] <= 0:
            fail(f"FirstEpisode returned a non-preview season/episode for a Shows()-surviving group: {best}")
    print("ok first-episode-always-yields-valid-identity-for-surviving-shows")


def main() -> None:
    assert_source_contract()
    run_recode_fallback_cases()
    run_first_episode_identity_invariant()
    print("PASS: Roku watch state show/season/episode recode-fallback boundary cases (#379)")


if __name__ == "__main__":
    main()
