#!/usr/bin/env python3
"""Issue #357: suppress the buffering toaster for 10s after initial play.

Expected behavior, derived from the issue before trusting the implementation:

1. Starting a movie, show episode, or music track must not show the
   "Buffering" toaster while that asset's stream first fills.
2. Notification logic begins 10 seconds after the asset is initially
   played — meaning when the viewer actually starts playback, not when a
   session is merely negotiated behind a Continue Watching cover.
3. After those 10 seconds, genuine mid-playback rebuffers still toast.
4. Playing the next episode/track is initially playing a new asset, so
   that load gets its own 10s window. A black PlaybackLoading handoff
   must not inherit the previous session's clock (otherwise a title that
   has already been playing for 10s+ immediately toasts on the next
   asset's first fill — the original complaint).
5. The existing 3s brief-stall delay on PlayerScreen remains; this issue
   adds a 10s initial-play window, it does not replace that filter.
6. Other toasts (quality reduced, errors) are not this notification.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
ACTIVITY = APP / "MainActivity.kt"
PLAYER = APP / "ui/screens/PlayerScreen.kt"
MUSIC = APP / "ui/screens/MusicPlayerScreen.kt"
DRIVER = Path(__file__).with_name("BufferingSuppressionDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"

SUPPRESSION_MS = 10_000
BRIEF_STALL_MS = 3_000


FAILURES: list[str] = []


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    FAILURES.append(message)


def abort(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def read(path: Path) -> str:
    if not path.is_file():
        abort(f"missing {path}")
    return path.read_text()


def match_braces(source: str, open_at: int, opener: str = "{", closer: str = "}") -> int:
    depth = 0
    for index, char in enumerate(source[open_at:], open_at):
        if char == opener:
            depth += 1
        elif char == closer:
            depth -= 1
            if depth == 0:
                return index
    abort(f"unbalanced {opener}{closer} at {open_at}")
    raise AssertionError


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start == -1:
        abort(f"missing `{signature}`")
    brace = source.find("{", start)
    if brace == -1:
        abort(f"`{signature}` has no body")
    return source[start : match_braces(source, brace) + 1]


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def jars_named(name: str) -> list[str]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return [
        str(path)
        for path in sorted(cache.glob(f"**/{name}-*.jar"))
        if "sources" not in path.name and "javadoc" not in path.name
    ]


def kotlin_compiler() -> list[str]:
    embedded = jars_named("kotlin-compiler-embeddable")
    if not embedded:
        abort("kotlin compiler is absent from the Gradle cache")
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


def stdlib_classpath() -> str:
    jars: list[str] = []
    for name in ("kotlin-stdlib", "kotlin-stdlib-jdk8", "kotlin-stdlib-jdk7", "annotations"):
        jars.extend(jars_named(name))
    if not jars:
        abort("kotlin stdlib is absent from the Gradle cache")
    return ":".join(dict.fromkeys(jars))


def slice_until_launch(play_entry: str) -> str:
    launch = play_entry.find("viewModelScope.launch")
    if launch == -1:
        abort("playEntry must launch negotiation asynchronously")
    return play_entry[:launch]


def assignments_of(source: str, name: str) -> list[int]:
    needle = f"{name} ="
    found: list[int] = []
    start = 0
    while True:
        index = source.find(needle, start)
        if index == -1:
            return found
        found.append(index)
        start = index + len(needle)


def assert_suppression_window_is_ten_seconds() -> None:
    before = len(FAILURES)
    model = read(VIEW_MODEL)
    require(
        "INITIAL_BUFFERING_NOTIFICATION_SUPPRESSION_MS = 10_000L" in model
        or "INITIAL_BUFFERING_NOTIFICATION_SUPPRESSION_MS = 10000L" in model,
        "the initial-play suppression window must be exactly 10 seconds",
    )
    report = function_body(model, "fun reportPlaybackBuffering(")
    require(
        "INITIAL_BUFFERING_NOTIFICATION_SUPPRESSION_MS" in report,
        "reportPlaybackBuffering must apply the 10s initial-play window",
    )
    require(
        "sinceSessionStart < INITIAL_BUFFERING_NOTIFICATION_SUPPRESSION_MS" in report
        or "elapsed < INITIAL_BUFFERING_NOTIFICATION_SUPPRESSION_MS" in report,
        "reports strictly inside the 10s window must return before notify; "
        "elapsed == 10s is when notification logic begins",
    )
    require(
        'notify("Buffering", ClientNotificationKind.WARNING)' in report,
        "the toaster this issue suppresses is the Buffering WARNING toast",
    )
    require(
        "_state.value !is UiState.Player" in report,
        "buffering toasts belong on the playback surface, not browse/dashboard",
    )
    quality = function_body(model, "fun reportPlaybackQualityReduced(")
    require(
        "INITIAL_BUFFERING_NOTIFICATION_SUPPRESSION_MS" not in quality,
        "the quality-reduced toaster is a different notification and must not "
        "be silently swallowed by the buffering suppression window",
    )
    if len(FAILURES) == before:
        print("ok reportPlaybackBuffering gates Buffering on a 10s window")


def assert_clock_starts_when_playback_is_shown() -> None:
    before = len(FAILURES)
    model = read(VIEW_MODEL)
    play_entry = function_body(model, "private fun playEntry(")
    require(
        "activePlaybackSessionStartedAtMs = SystemClock.elapsedRealtime()" in play_entry,
        "playEntry must stamp the suppression clock from elapsedRealtime",
    )

    require(
        "playerState.startPaused && preparingCover != null" in play_entry
        or "startPaused && preparingCover != null" in play_entry,
        "Continue Watching still parks a prepared session behind the cover",
    )

    park_at = play_entry.find("preparingCover.copy(prepared")
    require(park_at != -1, "playEntry must park the prepared Continue Watching session")
    resume = function_body(model, "fun resumeFromPreparingPlayback(")
    require(
        "prepared?.copy(startPaused = false)" in resume or "startPaused = false" in resume,
        "Resume must commit the parked Player session",
    )
    clock_before_park = any(
        site < park_at for site in assignments_of(play_entry, "activePlaybackSessionStartedAtMs")
    )
    resume_stamps = "activePlaybackSessionStartedAtMs = SystemClock.elapsedRealtime()" in resume
    require(
        (not clock_before_park) or resume_stamps,
        "Continue Watching must start the 10s window when the viewer presses "
        "Resume (or otherwise when Player is actually shown), not when the "
        "session is only parked behind the cover — otherwise a long read of "
        "the synopsis makes the initial buffer toast immediately",
    )

    play_next = function_body(model, "fun playNext(")
    require(
        "activePlaybackSessionStartedAtMs = SystemClock.elapsedRealtime()" in play_next,
        "promoting a preloaded next episode/track is initially playing that asset",
    )
    if len(FAILURES) == before:
        print("ok suppression clock starts when the asset is actually played")


def assert_replacement_load_does_not_inherit_prior_clock() -> None:
    before = len(FAILURES)
    model = read(VIEW_MODEL)
    play_entry = function_body(model, "private fun playEntry(")
    sync = slice_until_launch(play_entry)
    require(
        "UiState.PlaybackLoading" in sync,
        "session replacement still uses PlaybackLoading during the initial fill",
    )

    report = function_body(model, "fun reportPlaybackBuffering(")
    loading_can_toast = "UiState.PlaybackLoading" in report
    # Accept either policy: keep PlaybackLoading silent entirely, or treat
    # entering it as a fresh session start so the 10s window applies to this
    # asset's fill instead of the session being torn down.
    if loading_can_toast:
        require(
            "activePlaybackSessionStartedAtMs = SystemClock.elapsedRealtime()" in sync,
            "PlaybackLoading immediately reports buffering (MainActivity). If "
            "that report can toast, the 10s clock must be stamped when the "
            "replacement load begins — a previous episode that has already "
            "played for 10s+ must not make the next asset's first fill toast",
        )
    if len(FAILURES) == before:
        print("ok replacement initial load does not inherit the previous session clock")


def assert_player_and_loading_surfaces_report_through_the_gate() -> None:
    before = len(FAILURES)
    player = read(PLAYER)
    require(
        "BUFFERING_NOTIFICATION_DELAY_MS = 3_000L" in player
        or "BUFFERING_NOTIFICATION_DELAY_MS = 3000L" in player,
        "PlayerScreen must keep the 3s brief-stall delay; #357 adds a 10s "
        "initial-play window in front of that logic",
    )
    require(
        BRIEF_STALL_MS != SUPPRESSION_MS,
        "the 3s stall filter and the 10s initial-play window are distinct",
    )
    effect = player[player.find("LaunchedEffect(sessionId, isLoading)") :]
    require(
        "delay(BUFFERING_NOTIFICATION_DELAY_MS)" in effect[:500],
        "the buffering toast effect must still wait out brief stalls",
    )
    require(
        "onPlaybackBuffering()" in effect[:600],
        "PlayerScreen must still report buffering through the ViewModel gate",
    )

    activity = read(ACTIVITY)
    require(
        "onPlaybackBuffering = viewModel::reportPlaybackBuffering" in activity,
        "the shared toast surface must be SwarmViewModel.reportPlaybackBuffering",
    )
    loading = function_body(activity, "is UiState.PlaybackLoading ->")
    require(
        "onPlaybackBuffering()" in loading,
        "PlaybackLoading still reports through the same buffering gate",
    )

    music = read(MUSIC)
    require(
        "onPlaybackBuffering" not in music,
        "MusicPlayerScreen has no separate buffering toaster; music must share "
        "the ViewModel gate (full player / PlaybackLoading), not a second path",
    )
    if len(FAILURES) == before:
        print("ok both playback surfaces report buffering through the 10s gate")


def assert_fresh_play_uses_preparing_cover() -> None:
    before = len(FAILURES)
    play_entry = function_body(read(VIEW_MODEL), "private fun playEntry(")
    sync = slice_until_launch(play_entry)
    require(
        "UiState.PreparingPlayback(" in sync,
        "a fresh play must cover the catalog with PreparingPlayback rather than "
        "a Buffering toast while the first session is negotiated",
    )
    report = function_body(read(VIEW_MODEL), "fun reportPlaybackBuffering(")
    require(
        "PreparingPlayback" not in report,
        "PreparingPlayback is not a buffering-toast state; the cover replaces "
        "the need for a toaster during that wait",
    )
    if len(FAILURES) == before:
        print("ok fresh play waits on the cover, not a Buffering toast")


def assert_driver_executes_issue_policy() -> None:
    classpath = stdlib_classpath()
    with tempfile.TemporaryDirectory() as temp:
        out = Path(temp) / "classes"
        out.mkdir()
        compile_result = subprocess.run(
            kotlin_compiler()
            + [
                "-no-stdlib",
                "-no-reflect",
                "-cp",
                classpath,
                "-d",
                str(out),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            abort(
                "buffering suppression driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "BufferingSuppressionDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            abort(
                "buffering suppression UAT failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_suppression_window_is_ten_seconds()
    assert_clock_starts_when_playback_is_shown()
    assert_replacement_load_does_not_inherit_prior_clock()
    assert_player_and_loading_surfaces_report_through_the_gate()
    assert_fresh_play_uses_preparing_cover()
    assert_driver_executes_issue_policy()
    if FAILURES:
        print(f"FAILED {len(FAILURES)} production checks", file=sys.stderr)
        sys.exit(1)
    print("PASS: Issue #357 initial-play buffering toaster suppression UAT")


if __name__ == "__main__":
    main()
