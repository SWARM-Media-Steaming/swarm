#!/usr/bin/env python3
"""Lifecycle UAT for #398's nested genre playback boundary.

The issue's boundary is not limited to calculating a first successor.  A
Player state can be promoted from a preload or rebuilt for next/previous;
each transition must retain the original ArtistAlbums/ShowSeasons state, or a
subsequent queue calculation silently loses the genre scope.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
VIEW_MODEL = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/data/SwarmViewModel.kt"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def body(source: str, signature: str, until: str) -> str:
    start = source.find(signature)
    if start < 0:
        fail(f"missing lifecycle operation {signature}")
    end = source.find(until, start + len(signature))
    if end < 0:
        fail(f"could not delimit lifecycle operation {signature}")
    return source[start:end]


def require(fragment: str, source: str, message: str) -> None:
    if fragment not in source:
        fail(message)


def assert_nested_state_carries_scope(source: str) -> None:
    artist = body(source, "data class ArtistAlbums(", "    /** Movies:")
    show = body(source, "data class ShowSeasons(", "    data class Player(")
    player = body(source, "data class Player(", "/** The [UiState.Catalog]")
    require("val genreScope: String? = null", artist, "ArtistAlbums cannot retain its origin genre")
    require("val genreScope: String? = null", show, "ShowSeasons cannot retain its origin genre")
    require("val previous: UiState", player, "Player narrows previous state and would discard nested genre scope")


def assert_all_player_reentries_keep_previous(source: str) -> None:
    # The fallback advance path and Previous renegotiate a Player state.  Both
    # must receive the full nested screen, not its embedded Catalog.
    next_body = body(source, "    fun playNext()", "    /**\n     * ExoPlayer auto-advanced")
    require("previousScreen = current.previous", next_body, "playNext drops the nested screen before renegotiating")

    previous_body = body(source, "    fun playPrevious()", "    /**\n     * \"Minimize to tray\"")
    require("previousScreen = current.previous", previous_body, "playPrevious drops the nested screen before renegotiating")

    # Preloaded advancement does not call playEntry.  Its promotion must pass
    # the same state to toPlayerState, otherwise later skip/shuffle is wide.
    if not re.search(r"preloaded\.toPlayerState\(current\.previous,\s*musicQueueId\s*=\s*current\.musicQueueId\)", next_body):
        fail("preloaded advance does not preserve current.previous")


def assert_queue_reads_the_preserved_scope(source: str) -> None:
    helper = body(source, "private fun UiState.playbackGenreScope(", "/**\n * Screens")
    require("is UiState.ArtistAlbums -> genreScope.takeIf { kind == MediaKind.TRACK }", helper,
            "track queue does not read ArtistAlbums.genreScope")
    require("is UiState.ShowSeasons -> genreScope.takeIf { kind == MediaKind.EPISODE }", helper,
            "episode queue does not read ShowSeasons.genreScope")

    queue = body(source, "private fun playbackQueueEntries(", "/**\n * Screens")
    require("entriesForGenreScope(entries, previousScreen.playbackGenreScope(kind))", queue,
            "queue membership is not narrowed by the preserved previous screen")

    # A mixed-kind nested screen must never cause the unrelated kind to be
    # filtered: no scope is the intentional whole-catalog fallback.
    if "else -> null" not in helper:
        fail("non-matching media kinds lack the unscoped fallback")


def main() -> None:
    source = VIEW_MODEL.read_text(encoding="utf-8")
    assert_nested_state_carries_scope(source)
    assert_all_player_reentries_keep_previous(source)
    assert_queue_reads_the_preserved_scope(source)
    print("PASS: #398 preserves nested genre scope through preload, next, and previous player transitions")


if __name__ == "__main__":
    main()
