#!/usr/bin/env python3
"""Issue #466 output-path adversarial check.

The issue requires the UI to say whether the current output path can be
boosted. A selected compressed surround codec is not itself proof of
passthrough: Media3 may decode that same track to PCM when the sink/device
cannot accept bitstream output. Conversely, a sink's configured output mode
is the relevant evidence for whether a PCM LoudnessEnhancer can affect it.

This deterministic source contract prevents treating codec identity as an
output-path detector. It intentionally does not prescribe a particular
Media3 callback/API, only that output capability/configuration is consulted.
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
    # A list of MIME types and selected-track checks only describe the input
    # track. They cannot distinguish bitstream passthrough from decoded PCM.
    detector = re.search(
        r"private\s+fun\s+hasSelectedPassthroughAudio\s*\([^)]*\).*?\n}\s*",
        source,
        re.S,
    )
    if detector:
        body = detector.group(0)
        if "sampleMimeType" in body and not re.search(
            r"(AudioCapabilities|AudioSink|AudioTrackConfig|outputMode|isPassthrough|passthroughEnabled)",
            body,
            re.I,
        ):
            fail(
                "passthrough status is inferred from selected track MIME type; "
                "the same AC3/E-AC3 track can be decoded to PCM, so this can "
                "incorrectly report Audio Boost unavailable on a boostable path"
            )

    # More generally, status wiring must depend on an observed/configured
    # output path rather than just track metadata or a permanently false flag.
    path_evidence = re.search(
        r"AudioCapabilities|AudioSink|AudioTrackConfig|onAudioSinkError|"
        r"isPassthrough|passthroughEnabled|outputMode|audioOutputPath",
        source,
        re.I,
    )
    status_logic = re.search(r"audioBoostStatusFor\([^)]*\).*?\n}", source, re.S)
    status_has_path = bool(status_logic and re.search(
        r"passthrough|output|sink|capabilit", status_logic.group(0), re.I
    ))
    if not path_evidence or not status_has_path:
        fail(
            "Audio Boost availability is not based on active output-path or "
            "sink capability evidence; selected codec metadata alone cannot "
            "establish that boost is inapplicable"
        )

    print("PASS: Audio Boost availability distinguishes sink output from track codec")


if __name__ == "__main__":
    main()
