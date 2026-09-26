#!/usr/bin/env python3
"""Adversarial coverage for issue #443: skip-ahead bursts falsely report
"Server has gone offline".

Root cause (from the field report): on a direct-play MKV, mashing/holding
the skip button fired one real `Player.seekTo` per key-repeat tick. Each
seekTo tears down and restarts Media3's progressive extractor read; landing
that restart mid-EBML-element corrupts the parse
(ERROR_CODE_PARSING_CONTAINER_MALFORMED), and the resulting burst of
transport IOExceptions on the abandoned loads is what falsely trips the
outage banner. The server never went offline.

The fix accumulates a burst of skip presses into one pending target and
commits a single real seekTo only after the burst goes quiet
(SEEK_COALESCE_QUIET_MS). This suite does not re-run the happy-path
first-press/last-press assertions already covered by
clients/tv-android/app/src/test/kotlin/.../PlayerSeekTest.kt. It instead
checks invariants that a plausible-looking but subtly wrong implementation
could still violate:

- every repeatable skip input path (D-pad held via ACTION_DOWN, and a
  remote's key-up SEEK_FORWARD/SEEK_BACK) is routed through the coalescer,
  not left calling Player.seekTo/seekForward/seekBack directly;
- the pending target and its generation counter are scoped per playback
  session, so a stale burst from the tail of one episode cannot leak into
  the next;
- the commit effect clears the pending target before firing the real seek,
  so it cannot double-fire or replay a stale target;
- the quiet window is neither zero (which would reintroduce the bug) nor so
  long it reads as broken;
- the pure accumulation function itself nets a mixed-direction burst
  correctly and does not let a clamped-at-zero skip-back leave a phantom
  negative debt for the next skip-forward to pay off.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PLAYER_SCREEN = (
    ROOT
    / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui/screens/PlayerScreen.kt"
)
DRIVER = Path(__file__).with_name("Issue443SeekCoalescingDriver.kt")


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)


def assert_every_repeatable_skip_path_is_coalesced() -> None:
    source = PLAYER_SCREEN.read_text()

    action_down_start = source.find("if (event.action == KeyEvent.ACTION_DOWN)")
    if action_down_start == -1:
        fail("could not find the ACTION_DOWN surface-action dispatch")
    action_down_end = source.find("RemotePlaybackAction.PAUSE -> {", action_down_start)
    action_down_block = source[action_down_start:action_down_end]

    if "RemotePlaybackAction.SEEK_FORWARD ->" not in action_down_block:
        fail("ACTION_DOWN dispatch no longer handles SEEK_FORWARD")
    if "RemotePlaybackAction.SEEK_BACK ->" not in action_down_block:
        fail("ACTION_DOWN dispatch no longer handles SEEK_BACK")
    if "coalescedSeekTargetMs(" not in action_down_block:
        fail(
            "ACTION_DOWN (D-pad-held) skip no longer routes through "
            "coalescedSeekTargetMs — reissuing Player.seekTo per key-repeat "
            "tick is exactly what caused #443"
        )
    if "controllerPlayer.seekTo(" in action_down_block:
        fail(
            "ACTION_DOWN skip still calls controllerPlayer.seekTo directly; "
            "a held button will fire one real seek per repeat tick again"
        )

    remote_key_up_start = source.find("RemotePlaybackAction.PLAY -> player.play()")
    if remote_key_up_start == -1:
        fail("could not find the key-up RemotePlaybackAction dispatch")
    remote_key_up_end = source.find("RemotePlaybackAction.SHOW_CONTROLS ->", remote_key_up_start)
    remote_key_up_block = source[remote_key_up_start:remote_key_up_end]

    if "coalescedSeekTargetMs(" not in remote_key_up_block:
        fail(
            "key-up remote SEEK_FORWARD/SEEK_BACK no longer routes through "
            "coalescedSeekTargetMs"
        )
    if "controllerPlayer.seekForward()" in remote_key_up_block or (
        "controllerPlayer.seekBack()" in remote_key_up_block
    ):
        fail(
            "key-up remote skip still calls controllerPlayer.seekForward()/"
            "seekBack() directly, bypassing the coalescer entirely for that "
            "input path — a remote that reports repeats as key-up bursts "
            "would still corrupt the MKV parse"
        )
    print("ok both repeatable skip input paths route through the coalescer")


def assert_pending_state_is_session_scoped() -> None:
    source = PLAYER_SCREEN.read_text()
    for declaration in (
        "var pendingSeekTargetMs by remember(sessionId) { mutableStateOf<Long?>(null) }",
        "var seekRequestGeneration by remember(sessionId) { mutableStateOf(0L) }",
    ):
        if declaration not in source:
            fail(
                f"missing session-scoped declaration: {declaration!r} — without "
                "remember(sessionId), a coalesced target from the tail of one "
                "episode could survive into the next episode's session and "
                "fire a bogus seek on the new video"
            )
    print("ok pending seek state is scoped per playback session, not global")


def assert_commit_effect_clears_before_seeking() -> None:
    source = PLAYER_SCREEN.read_text()
    effect_start = source.find(
        "LaunchedEffect(controllerPlayer, sessionId, seekRequestGeneration) {"
    )
    if effect_start == -1:
        fail(
            "missing the commit LaunchedEffect keyed on "
            "(controllerPlayer, sessionId, seekRequestGeneration) — without "
            "the generation key in its recomposition scope, a new press "
            "cannot cancel a not-yet-fired commit from an earlier press"
        )
    effect_end = source.find("\n    }", effect_start)
    effect_body = source[effect_start:effect_end]

    clear_at = effect_body.find("pendingSeekTargetMs = null")
    seek_at = effect_body.find("controllerPlayer.seekTo(target)")
    if clear_at == -1 or seek_at == -1:
        fail("commit effect no longer clears pendingSeekTargetMs and fires the real seekTo")
    if clear_at > seek_at:
        fail(
            "commit effect fires the real seekTo before clearing "
            "pendingSeekTargetMs — a recomposition racing the seek could "
            "read back the just-committed target and double-fire it"
        )
    if "delay(SEEK_COALESCE_QUIET_MS)" not in effect_body:
        fail("commit effect no longer waits out the quiet window before committing")
    print("ok commit effect clears the pending target before firing the single real seek")


def assert_quiet_window_is_sane() -> None:
    source = PLAYER_SCREEN.read_text()
    marker = "internal const val SEEK_COALESCE_QUIET_MS = "
    at = source.find(marker)
    if at == -1:
        fail("SEEK_COALESCE_QUIET_MS constant is missing")
    rest = source[at + len(marker) :]
    digits = ""
    for ch in rest:
        if ch.isdigit():
            digits += ch
        else:
            break
    if not digits:
        fail("could not parse SEEK_COALESCE_QUIET_MS value")
    value_ms = int(digits)
    if value_ms <= 0:
        fail(
            f"SEEK_COALESCE_QUIET_MS is {value_ms}ms — a zero (or negative) "
            "quiet window commits a real seekTo on every press again, "
            "reintroducing #443"
        )
    if value_ms > 2000:
        fail(
            f"SEEK_COALESCE_QUIET_MS is {value_ms}ms — that reads as broken "
            "for a single isolated skip press, which the fix's own doc "
            "comment says must still feel instant"
        )
    print(f"ok SEEK_COALESCE_QUIET_MS ({value_ms}ms) is a sane quiet window")


def jars_named(name: str) -> list[str]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return [
        str(p)
        for p in sorted(cache.glob(f"**/{name}-*.jar"))
        if "sources" not in p.name and "javadoc" not in p.name
    ]


def kotlin_compiler() -> list[str]:
    embedded = jars_named("kotlin-compiler-embeddable")
    if not embedded:
        fail("kotlin compiler is absent from Gradle cache")
    extras: list[str] = []
    for name in (
        "kotlin-stdlib",
        "kotlin-reflect",
        "kotlin-script-runtime",
        "kotlinx-coroutines-core-jvm",
        "annotations",
        "trove4j",
    ):
        extras.extend(jars_named(name)[:3])
    return [
        "java",
        "-cp",
        ":".join([embedded[-1], *extras]),
        "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
    ]


def extract_coalesced_seek_target_fn() -> str:
    """Pulls coalescedSeekTargetMs verbatim out of the real source, so the
    compiled driver runs the actual fix rather than a hand-copied stand-in
    that could silently drift from it."""
    source = PLAYER_SCREEN.read_text()
    start = source.find("internal fun coalescedSeekTargetMs(")
    if start == -1:
        fail("coalescedSeekTargetMs is missing from PlayerScreen.kt")
    end = source.find("\n\n", start)
    if end == -1:
        fail("could not find the end of coalescedSeekTargetMs")
    snippet = source[start:end]
    if "coerceAtLeast(0L)" not in snippet:
        fail("extracted coalescedSeekTargetMs no longer clamps at zero")
    return snippet


def assert_pure_accumulation_boundaries() -> None:
    fn_source = extract_coalesced_seek_target_fn()
    with tempfile.TemporaryDirectory() as temp:
        src_dir = Path(temp) / "src"
        src_dir.mkdir()
        fn_file = src_dir / "CoalescedSeekTargetMs.kt"
        fn_file.write_text(fn_source + "\n")
        out = Path(temp) / "classes"
        out.mkdir()
        stdlib_classpath = ":".join(jars_named("kotlin-stdlib"))
        compile_result = subprocess.run(
            kotlin_compiler()
            + [
                "-no-stdlib",
                "-no-reflect",
                "-cp",
                stdlib_classpath,
                "-d",
                str(out),
                str(fn_file),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            fail(
                "seek-coalescing driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        stdlib = jars_named("kotlin-stdlib")
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{':'.join(stdlib)}", "Issue443SeekCoalescingDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "seek-coalescing accumulation boundaries failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_every_repeatable_skip_path_is_coalesced()
    assert_pending_state_is_session_scoped()
    assert_commit_effect_clears_before_seeking()
    assert_quiet_window_is_sane()
    assert_pure_accumulation_boundaries()
    print("PASS: Issue #443 skip-seek coalescing adversarial UAT")


if __name__ == "__main__":
    main()
