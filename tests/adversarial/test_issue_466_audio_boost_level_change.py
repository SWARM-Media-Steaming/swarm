#!/usr/bin/env python3
"""Issue #466 level-change lifecycle boundary check.

Expected behaviour (from the issue and Media3 sink semantics):

- Changing Audio Boost from the pause overlay must not drop the knowledge of
  the output path that the player already observed. Media3 reports the sink
  configuration once, through onAudioTrackInitialized, and does not repeat it
  while the same AudioTrack keeps playing. Pausing and changing the level
  does not recreate that track.
- So after a level change on a decoded PCM sink, the boost must re-attach to
  the current session and report "Active", not "Unavailable: audio output not
  ready" until the next sink re-creation.

The observed encoding must therefore live outside the effect keyed on
audioBoostLevel. A variable declared inside that effect is reset on every
level change. This check reads the source because the Compose effect cannot
run in the JVM unit harness.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PLAYER = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui/screens/PlayerScreen.kt"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    source = PLAYER.read_text()

    sink_write = re.search(r"onAudioTrackInitialized[\s\S]*?\b(\w+)\s*=\s*config\.encoding", source)
    if sink_write is None:
        fail("onAudioTrackInitialized no longer records config.encoding; the observed output path is unknown")
    sink_var = sink_write.group(1)

    declaration = re.search(rf"\b(?:var|val)\s+{re.escape(sink_var)}\b", source)
    if declaration is None:
        fail(f"observed sink state '{sink_var}' has no declaration")

    level_effect = re.search(r"DisposableEffect\(\s*player\s*,\s*audioBoostLevel\s*\)", source)
    if level_effect is None:
        # The effect is no longer keyed on the level, so a level change cannot
        # reset the observed sink state through it.
        print("PASS: observed sink state is not reset by Audio Boost level changes")
        return

    if declaration.start() > level_effect.start():
        fail(
            f"'{sink_var}' is declared inside the DisposableEffect keyed on audioBoostLevel, so every "
            "level change from the pause overlay resets it to unknown. Media3 does not re-fire "
            "onAudioTrackInitialized for the same live AudioTrack, so Audio Boost stays "
            "'Unavailable: audio output not ready' and no LoudnessEnhancer is attached until the "
            "sink is re-created."
        )

    print("PASS: observed sink state survives Audio Boost level changes")


if __name__ == "__main__":
    main()
