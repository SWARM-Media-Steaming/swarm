#!/usr/bin/env python3
"""Issue #466 sink lifecycle boundary checks.

The UI may report Audio Boost active only after the active sink is observed as
linear PCM. Before the first sink configuration, or after the old sink is
released while a replacement is being created, output mode is unknown. Treating
unknown as PCM temporarily claims a boost which may not be applied (notably on
passthrough transitions).
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
    effect_start = source.find("DisposableEffect(player, audioBoostLevel)")
    if effect_start < 0:
        fail("session and level keyed Audio Boost lifecycle not found")
    effect_end = source.find("\n    // Flush the resume position", effect_start)
    block = source[effect_start:effect_end]

    # Until onAudioTrackInitialized supplies a concrete encoding, null means
    # unknown. A nullable let plus `== true` maps null to false and therefore
    # falls through into LoudnessEnhancer creation/Active status.
    attach = block[block.find("fun attach(sessionId: Int)"):]
    if re.search(r"sinkEncoding\?\.let\s*\{[^}]*!Util\.isEncodingLinearPcm\(it\)[^}]*\}\s*==\s*true", attach):
        fail("unknown sink encoding is treated as decoded PCM and can be reported Active")
    if not re.search(r"sinkEncoding\s*==\s*null|sinkEncoding\?\.let[^\n]*\?:\s*true|when\s*\(sinkEncoding\)", attach):
        fail("attach() does not explicitly classify an unknown sink as unavailable")

    released = re.search(r"override fun onAudioTrackReleased\([^)]*\)\s*\{(.*?)\n\s*}", block, re.S)
    if not released:
        fail("sink release callback is missing")
    body = released.group(1)
    if "sinkEncoding = null" not in body:
        fail("sink release must invalidate the old output encoding")
    if "attach(" in body:
        fail("sink release reattaches against unknown encoding before a replacement sink is initialized")
    print("PASS: unknown/released sink states cannot claim Audio Boost is active")


if __name__ == "__main__":
    main()
