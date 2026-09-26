#!/usr/bin/env python3
"""Issue #449 adversarial contract: VFR probing has its own slow-share budget.

Expected behavior is derived from the issue and playback invariants, rather
than the implementation:

* A LAN remux is safe only after a VFR result says it is safe.  A metadata
  response arriving after the two-second audio-selection deadline but before
  a reasonable VFR deadline must still be used, otherwise the #442 timestamp
  protection silently disappears on the slow shares it is meant to protect.
* Audio selection is startup-sensitive and retains its short deadline; making
  every probe wait longer would turn this narrowly-scoped fix into a playback
  startup regression.
* A genuinely wedged VFR probe must preserve the historical fail-open remux
  fallback, but it must emit a warning so operators can distinguish a CFR
  decision from a timeout.
* Previews, WAN playback, and incompatible video never enter the LAN remux
  path, so they must not pay for a VFR probe.

There is deliberately no wall-clock sleep here.  A synthetic process delay
would make the test slow and scheduler-dependent; this deterministic contract
checks the production timeout boundary and drives the just-over-audio / within-
VFR timing case as fixed duration data.  It fails against #449's predecessor,
where `has_variable_frame_rate` was wrapped in `AUDIO_PROBE_TIMEOUT`.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
TRANSCODE = ROOT / "crates/swarm-media/src/transcode.rs"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    require(start >= 0, f"missing `{signature}`")
    open_brace = source.find("{", start)
    require(open_brace >= 0, f"`{signature}` has no body")
    depth = 0
    for index in range(open_brace, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    fail(f"unbalanced braces in `{signature}`")
    raise AssertionError("unreachable")


def timeout_seconds(source: str, name: str) -> int:
    match = re.search(
        rf"const\s+{re.escape(name)}\s*:\s*Duration\s*=\s*Duration::from_secs\((\d+)\)",
        source,
    )
    require(match is not None, f"missing explicit `{name}` duration constant")
    return int(match.group(1))


def assert_timeout_boundaries(source: str) -> None:
    audio_seconds = timeout_seconds(source, "AUDIO_PROBE_TIMEOUT")
    vfr_seconds = timeout_seconds(source, "VFR_PROBE_TIMEOUT")
    require(audio_seconds == 2, "audio probing must retain its 2-second startup deadline")
    require(vfr_seconds == 10, "VFR probing must use the issue #449 10-second slow-share deadline")
    require(vfr_seconds > audio_seconds, "VFR deadline must be independently longer than audio")

    # A 3s metadata response is the boundary that regressed: it is late for
    # audio selection, yet early enough to preserve timestamp safety.
    slow_share_response_seconds = 3
    require(
        audio_seconds < slow_share_response_seconds < vfr_seconds,
        "fixture must model a VFR result that would fail under the old shared timeout "
        "but complete under the dedicated VFR timeout",
    )
    print(
        "ok a 3s slow-share VFR result is rejected by the audio budget but accepted "
        "by the dedicated 10s VFR budget"
    )


def assert_plan_wires_the_vfr_deadline(source: str) -> None:
    plan = function_body(source, "pub async fn plan(")
    eligibility_at = plan.find("let remux_eligible =")
    probe_at = plan.find("crate::probe::has_variable_frame_rate")
    remux_at = plan.find("if remux_eligible && !variable_frame_rate")
    require(eligibility_at >= 0, "plan must define the LAN remux eligibility boundary")
    require(probe_at > eligibility_at, "VFR probe must be considered only after remux eligibility")
    require(remux_at > probe_at, "remux decision must wait for VFR probe result")

    vfr_block = plan[eligibility_at:remux_at]
    require(
        "let variable_frame_rate = if remux_eligible" in vfr_block,
        "VFR probe must be skipped for previews, WAN, and incompatible video",
    )
    timeout_at = vfr_block.find("tokio::time::timeout(")
    probe_at = vfr_block.find("crate::probe::has_variable_frame_rate")
    require(timeout_at >= 0 and probe_at > timeout_at, "VFR probe must be bounded by tokio timeout")
    wrapped_probe = vfr_block[timeout_at:probe_at]
    require(
        "VFR_PROBE_TIMEOUT" in wrapped_probe,
        "has_variable_frame_rate must use VFR_PROBE_TIMEOUT, never AUDIO_PROBE_TIMEOUT",
    )
    require(
        "AUDIO_PROBE_TIMEOUT" not in wrapped_probe,
        "the audio timeout must not be reused for VFR probing",
    )
    require(
        "if remux_eligible && !variable_frame_rate" in plan,
        "only a completed true VFR result may veto the normal eligible remux path",
    )
    print("ok only LAN remux candidates probe VFR, using the dedicated deadline before remux")


def assert_timeout_fallback_is_visible_and_fail_open(source: str) -> None:
    plan = function_body(source, "pub async fn plan(")
    timeout_match = re.search(
        r"Err\(_\)\s*=>\s*\{(?P<body>.*?)\n\s*}\n\s*}\n\s*}\s*else",
        plan,
        flags=re.DOTALL,
    )
    require(timeout_match is not None, "VFR timeout must have an explicit fallback branch")
    fallback = timeout_match.group("body")
    require(
        '"variable-frame-rate probe timed out; falling back to remux"' in fallback,
        "a VFR timeout must warn operators instead of silently failing open",
    )
    require("timeout_secs = VFR_PROBE_TIMEOUT.as_secs()" in fallback, "warning must report the VFR deadline")
    require(re.search(r"\bfalse\b", fallback) is not None, "VFR timeout must fail open as not-VFR")

    direct_audio = function_body(source, "async fn direct_play_audio_is_viable(")
    audio_probe_at = direct_audio.find("crate::probe::list_audio_streams")
    require(audio_probe_at >= 0, "direct audio selection must retain its audio probe")
    audio_timeout_at = direct_audio.rfind("tokio::time::timeout(", 0, audio_probe_at)
    require(audio_timeout_at >= 0, "audio probe must remain timeout-bounded")
    require(
        "AUDIO_PROBE_TIMEOUT" in direct_audio[audio_timeout_at:audio_probe_at],
        "audio selection must continue using AUDIO_PROBE_TIMEOUT rather than inheriting VFR latency",
    )
    print("ok VFR timeout warns and fails open; audio selection remains on its short deadline")


def main() -> None:
    require(TRANSCODE.is_file(), f"missing production source: {TRANSCODE}")
    source = TRANSCODE.read_text()
    assert_timeout_boundaries(source)
    assert_plan_wires_the_vfr_deadline(source)
    assert_timeout_fallback_is_visible_and_fail_open(source)
    print("PASS: Issue #449 VFR probe timeout contract")


if __name__ == "__main__":
    main()
