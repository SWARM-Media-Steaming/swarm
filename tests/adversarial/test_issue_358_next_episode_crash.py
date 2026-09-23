#!/usr/bin/env python3
"""Issue #358: App crash when next episode is clicked.

Expected behavior, derived from the issue and session/player invariants
before trusting the implementation:

1. The Next control on an episode (Forensic Files S4E3 → S4E4 in the
   report) must not take down the process. `VideoPlayerPool.activate`
   runs inside Compose `remember` with no try/catch, so it must not throw
   when the active session id is still set but the ExoPlayer is gone.
2. After that failure, opening the successor from the catalog must still
   load a stream. S4E5 working while S4E4 does not is not acceptable as a
   permanent split: both are ordinary next-episode targets.
3. Next-episode preload releases the finished episode before negotiating
   the successor so a one-slot server can still admit it. Promotion of a
   prepared successor must not go through a throwing player-pool path.
4. Gapless music preload is allowed to keep two claimed sessions; the
   episode path is not an excuse to break that.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
PLAYER = APP / "ui/screens/PlayerScreen.kt"
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
ACTIVITY = APP / "MainActivity.kt"
DRIVER = Path(__file__).with_name("NextEpisodeCrashDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"

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


def core_classpath() -> str:
    cmd = [
        str(GRADLEW),
        "-p",
        str(ROOT / "clients/tv-android"),
        ":core:compileKotlin",
        "-q",
    ]
    subprocess.run(cmd, check=True, cwd=ROOT)
    classes = ROOT / "clients/tv-android/core/build/classes/kotlin/main"
    if not classes.is_dir():
        abort(f"kotlin classes missing at {classes}")
    needed = [
        "kotlin-stdlib",
        "kotlin-stdlib-jdk8",
        "kotlin-stdlib-jdk7",
        "kotlinx-serialization-core-jvm",
        "kotlinx-serialization-json-jvm",
        "kotlinx-coroutines-core-jvm",
        "annotations",
    ]
    jars = [str(classes)]
    for name in needed:
        jars.extend(jars_named(name))
    seen: set[str] = set()
    out: list[str] = []
    for jar in jars:
        if jar not in seen:
            seen.add(jar)
            out.append(jar)
    return ":".join(out)


def kotlin_compiler() -> list[str]:
    embed = jars_named("kotlin-compiler-embeddable")
    if not embed:
        abort("kotlin-compiler-embeddable jar not in gradle cache")
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
        ":".join([embed[-1], *extras]),
        "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
    ]


def assert_activate_cannot_crash_the_composition() -> None:
    player = read(PLAYER)
    activate = function_body(player, "fun activate(")
    require(
        "checkNotNull(activePlayer)" not in activate,
        "VideoPlayerPool.activate must not checkNotNull the active player — that throws on the Compose remember path and hard-crashes the app on next-episode",
    )
    require(
        "activeSessionId == config.sessionId" in activate,
        "activate must still reuse a live player for the same session id (preload promotion)",
    )
    require(
        "activePlayer?.let { return it }" in activate,
        "same-session reuse must be null-safe so a lost ExoPlayer recreates instead of crashing",
    )
    require(
        "remember(playerPool, sessionId)" in player and "playerPool.activate(" in player,
        "the video surface must obtain its player from pool.activate inside remember(sessionId)",
    )
    require(
        "DisposableEffect(player)" in player,
        "the active player must still be released on composition dispose",
    )
    print("ok activate cannot throw on a lost same-session player")


def assert_next_episode_flow_stops_then_plays() -> None:
    view_model = read(VIEW_MODEL)
    preload = function_body(view_model, "fun preloadNextEpisode(")
    require(
        "releasePlaybackSessionNow" in preload,
        "preloadNextEpisode must release the finished episode before negotiating the successor",
    )
    require(
        "preparePlayback(" in preload,
        "preloadNextEpisode must negotiate the successor through preparePlayback",
    )
    release_at = preload.find("releasePlaybackSessionNow")
    prepare_at = preload.find("preparePlayback(")
    require(
        release_at != -1 and prepare_at != -1 and release_at < prepare_at,
        "the finished episode must be released before the next episode is negotiated (one-slot servers)",
    )

    play_next = function_body(view_model, "fun playNext()")
    require(
        "preloadedNext" in play_next,
        "playNext must promote a prepared next episode when one exists",
    )
    require(
        "checkNotNull" not in play_next and "!!" not in play_next,
        "playNext must not force-unwrap the successor — a missing preload is a normal playEntry path",
    )

    play_entry = function_body(view_model, "private fun playEntry(")
    require(
        "replaceSession" in play_entry and "releasePlaybackSessionNow" in play_entry,
        "clicking Next without a preload must /stop the current episode before /play of the successor",
    )

    activity = read(ACTIVITY)
    require(
        "onContinue = onPlayNext" in activity,
        "the player Next/Continue control must invoke playNext",
    )
    require(
        "onPreloadNextEpisode = viewModel::preloadNextEpisode" in activity
        or "onPreloadNext = { viewModel.preloadNextEpisode" in activity,
        "ENDED/credits must still kick preloadNextEpisode",
    )
    print("ok next-episode releases then plays and never force-unwraps")


def assert_music_preload_keeps_current_session() -> None:
    view_model = read(VIEW_MODEL)
    preload_track = function_body(view_model, "fun preloadNextTrack(")
    require(
        "releasePlaybackSessionNow" not in preload_track.split("preparePlayback(")[0],
        "preloadNextTrack must not /stop the playing track before negotiating the successor",
    )
    print("ok music gapless preload does not stop the current track first")


def assert_driver_executes() -> None:
    classpath = core_classpath()
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
                "next-episode crash driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "NextEpisodeCrashDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            abort(
                "next-episode crash UAT failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_activate_cannot_crash_the_composition()
    assert_next_episode_flow_stops_then_plays()
    assert_music_preload_keeps_current_session()
    assert_driver_executes()
    if FAILURES:
        sys.exit(1)
    print("PASS: Issue #358 next-episode crash UAT")


if __name__ == "__main__":
    main()
