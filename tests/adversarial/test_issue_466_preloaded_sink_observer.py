#!/usr/bin/env python3
"""Issue #466 preloaded-episode sink observation boundary check.

Expected behaviour (from the issue: "episode handoffs ... Reattach or recreate
the effect when required", and the UI must reflect whether boost is active):

- Episode handoffs use VideoPlayerPool.preload(), which creates and prepares the
  next ExoPlayer during the countdown with playWhenReady=false. Media3 creates
  the AudioTrack during that preparation, so onAudioTrackInitialized can fire
  before any UI listener exists.
- AnalyticsListener events are only delivered to listeners registered at the
  time they fire. PlayerScreen registers its sink listener only after the
  composition promotes the preloaded player, so the observed encoding stays
  unknown and Audio Boost reports "Unavailable: audio output not ready" until
  the AudioTrack is re-created, even though the decoded PCM output is live.
- Therefore the sink observer must be registered in the pool's player creation
  path before the first prepare() of any player, preloads included.

This is a source-level check because the Compose and ExoPlayer lifecycle cannot
run in the JVM unit harness. Device confirmation of the Media3 preload timing
is still required for issue #466 acceptance.
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PLAYER = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui/screens/PlayerScreen.kt"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def create_player_body(source: str) -> str:
    start = source.find("private fun createPlayer(")
    if start < 0:
        fail("VideoPlayerPool.createPlayer not found; the preload path can no longer be checked")
    end = source.find("\n}\n", start)
    if end < 0:
        fail("could not find the end of VideoPlayerPool.createPlayer")
    return source[start:end]


def main() -> None:
    source = PLAYER.read_text()
    body = create_player_body(source)

    prepare_at = body.find("prepare()")
    if prepare_at < 0:
        fail("createPlayer no longer calls prepare(); preloaded players are not checked by this suite")

    observer_at = body.find("addAnalyticsListener(")
    if observer_at < 0 or observer_at > prepare_at:
        fail(
            "VideoPlayerPool.createPlayer prepares the player without registering a sink observer first. "
            "A preloaded next episode initializes its AudioTrack during the countdown, before PlayerScreen "
            "attaches its listener, so onAudioTrackInitialized is missed. After promotion the sink stays "
            "unknown and Audio Boost reports 'Unavailable: audio output not ready' while decoded PCM plays."
        )

    print("PASS: sink observation is registered before any player, including preloads, is prepared")


if __name__ == "__main__":
    main()
