#!/usr/bin/env python3
"""Adversarial coverage for issue #466: adjustable Audio Boost on the Fire TV
player.

Expected behavior is derived from the issue and Android audio-effect
invariants, then checked against the Kotlin sources (no device or kotlinc
needed, so it is deterministic):

- the supported levels are a bounded, strictly increasing list that starts
  with Off, and the gain cap can never silently clamp a level the UI offers
  (otherwise the label would promise more gain than is applied);
- the persisted value is validated on read AND write, so a corrupt or
  out-of-range preference can never produce an unbounded gain;
- the old unconditional, error-swallowing fixed +10 dB enhancer is gone;
- the effect is rebuilt for every audio-session change, the previous
  instance is released first, and the listener/effect are torn down on
  dispose (no leaked AudioEffect per episode handoff or level change);
- "active" is only reported after the effect is verified to be enabled and
  controlled: AudioEffect.setEnabled reports failure through a status code
  and hasControl() rather than an exception, so a try/catch alone can show
  "Active" for an effect that is not applied;
- the status reflects the real output path (passthrough/bitstream cannot be
  boosted), not just a static caveat appended to "Active".
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
PLAYER = (APP / "ui/screens/PlayerScreen.kt").read_text()
STORE = (APP / "data/AndroidAudioBoostStore.kt").read_text()

failures: list[str] = []


def check(cond: bool, message: str) -> None:
    if not cond:
        failures.append(message)


def attach_block() -> str:
    start = PLAYER.find("DisposableEffect(player, audioBoostLevel)")
    if start < 0:
        failures.append("audio boost DisposableEffect keyed on player+level not found")
        return ""
    end = PLAYER.find("onDispose", start)
    end = PLAYER.find("\n    }\n", end)
    return PLAYER[start:end]


def levels() -> list[int]:
    m = re.search(r"LEVELS\s*=\s*listOf\(([^)]*)\)", STORE)
    check(m is not None, "AndroidAudioBoostStore.LEVELS not found")
    return [int(x) for x in m.group(1).split(",")] if m else []


def test_levels_bounded_and_consistent() -> None:
    lv = levels()
    check(len(lv) >= 2 and lv[0] == 0, f"LEVELS must start with Off(0): {lv}")
    check(lv == sorted(set(lv)), f"LEVELS must be strictly increasing: {lv}")
    check(all(x >= 0 for x in lv), f"LEVELS must be non-negative: {lv}")
    cap = re.search(r"AUDIO_BOOST_MAX_MILLIBELS\s*=\s*([\d_]+)", PLAYER)
    check(cap is not None, "AUDIO_BOOST_MAX_MILLIBELS missing")
    if cap and lv:
        cap_mb = int(cap.group(1).replace("_", ""))
        check(max(lv) * 1000 <= cap_mb,
              f"top level {max(lv)} dB exceeds the gain cap {cap_mb} mB; the UI label would lie")
        check(cap_mb <= 15_000,
              f"gain cap {cap_mb} mB exceeds the +15 dB distortion threshold noted in the old code")
    check(re.search(r"levelDb\s*\*\s*1000", PLAYER) is not None,
          "gain must be dB*1000 millibels")


def test_store_validates_both_directions() -> None:
    get = re.search(r"fun get\(\)[^\n]*", STORE)
    check(get is not None and "in LEVELS" in get.group(0) and "?: 0" in get.group(0),
          "get() must fall back to Off for values outside LEVELS")
    setter = re.search(r"fun set\(.*?\n    }\n", STORE, re.S)
    check(setter is not None and "in LEVELS" in setter.group(0),
          "set() must normalise values outside LEVELS")
    check("applicationContext" in STORE,
          "store must use applicationContext (no Activity leak / survives recreation)")
    check("MODE_PRIVATE" in STORE, "preferences must be private")
    for p in APP.rglob("*.kt"):
        t = p.read_text()
        if "swarm_audio_boost" in t and p.name != "AndroidAudioBoostStore.kt":
            failures.append(f"{p.name} touches the audio boost prefs file directly")


def test_old_fixed_enhancer_removed() -> None:
    check("AUDIO_BOOST_MILLIBELS" not in PLAYER.replace("AUDIO_BOOST_MAX_MILLIBELS", ""),
          "old fixed AUDIO_BOOST_MILLIBELS still referenced")
    check("loudnessEnhancer" not in PLAYER, "stale loudnessEnhancer reference remains")
    check(len(re.findall(r"LoudnessEnhancer\(", PLAYER)) == 1,
          "exactly one LoudnessEnhancer construction site is expected")
    check(re.search(r"runCatching\s*\{\s*LoudnessEnhancer", PLAYER) is None,
          "enhancer construction must not silently swallow failures")


def test_effect_lifecycle() -> None:
    block = attach_block()
    if not block:
        return
    check("onAudioSessionIdChanged" in block, "must react to audio session changes")
    check("addListener" in block and "removeListener" in block,
          "session listener must be added and removed")
    ond = block[block.find("onDispose"):]
    check("release()" in ond, "enhancer must be released on dispose")
    check("removeListener" in ond, "listener must be removed on dispose")
    attach = block[block.find("fun attach"):]
    first_release = attach.find("release()")
    construct = attach.find("LoudnessEnhancer(")
    check(0 <= first_release < construct,
          "previous enhancer must be released before a new one is created")
    check("enhancer = null" in attach[: construct if construct > 0 else 0],
          "released enhancer reference must be cleared")
    # failure path must also not leak a half-constructed effect and must log
    catch = attach[attach.find("catch"):]
    check("release()" in catch, "failed activation must release the effect")
    check("Log.w" in catch and "error" in catch, "failure must log the exception")
    check("AUDIO_BOOST_UNAVAILABLE_STATUS" in catch, "failure must surface to the viewer")
    check("attach(player.audioSessionId)" in block, "must attach for the current session immediately")


def test_active_requires_verification() -> None:
    block = attach_block()
    if not block:
        return
    try_body = block[block.find("try {"): block.find("catch")]
    verified = re.search(r"\bhasControl\(\)|\.enabled\b(?!\s*=)|\bgetEnabled\(\)|\bsetEnabled\([^)]*\)\s*(!=|==)", try_body)
    # `enabled = true` alone is an assignment, not a verification.
    check(verified is not None,
          "after enabling, the code never reads back enabled/hasControl(); a non-throwing "
          "AudioEffect failure (status code / another app holding control) would still show 'Active'")


def test_status_reflects_output_path() -> None:
    sig = re.search(r"fun audioBoostStatusFor\(([^)]*)\)", PLAYER)
    params = sig.group(1).count(":") if sig else 0
    path_aware = re.search(
        r"sampleMimeType|onAudioTrackInitialized|AudioTrackConfig|isPassthrough|passthrough\w*\s*[=:(]|"
        r"AudioCapabilities|onAudioSinkError|audioFormat|currentTracks[^\n]*audio",
        PLAYER, re.I)
    check(params > 2 or path_aware is not None,
          "status never inspects the audio output path; passthrough/bitstream playback is "
          "reported 'Active' with only a static caveat, contrary to 'clearly communicate when "
          "the current output path cannot apply the boost'")


def test_ui_reachable_by_remote() -> None:
    m = re.search(r"private fun AudioBoostPicker.*?\n}\n", PLAYER, re.S)
    check(m is not None, "AudioBoostPicker missing")
    if m:
        body = m.group(0)
        check("SelectableChip" in body and "onClick" in body,
              "levels must be D-pad selectable chips")
        check("AndroidAudioBoostStore.LEVELS" in body, "picker must offer every supported level")
        check('"Off"' in body, "an explicit Off option is required")
    check("AudioBoostPicker(audioBoostLevel, audioBoostStatus" in PLAYER,
          "picker must be wired into the pause overlay")
    check(re.search(r"audioBoostStore\.set\(level\)", PLAYER) is not None,
          "selection must be persisted")
    check(re.search(r"mutableStateOf\(audioBoostStore\.get\(\)\)", PLAYER) is not None,
          "initial level must come from the persisted value")


def main() -> None:
    for fn in (test_levels_bounded_and_consistent, test_store_validates_both_directions,
               test_old_fixed_enhancer_removed, test_effect_lifecycle,
               test_active_requires_verification, test_status_reflects_output_path,
               test_ui_reachable_by_remote):
        fn()
    if failures:
        for f in failures:
            print(f"FAIL: {f}", file=sys.stderr)
        sys.exit(1)
    print("issue 466 audio boost adversarial checks passed")


if __name__ == "__main__":
    main()
