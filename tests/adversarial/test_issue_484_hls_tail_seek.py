#!/usr/bin/env python3
"""Adversarial coverage for issue #484: fast-forwarding to the post-credit
tail freezes playback and makes the media server disconnect and reconnect.

Issue text (verbatim, the requirement this suite ties to): "When watching
movies, like marvel movies, and you fast forward towards the end to the post
credit scenes the movie freezes, the media server always disconnects, then it
reconnects, then it starts streaming again. This needs to be fixed."

Root cause and fix: FullDurationPlayer.seekTo compared a tail seek against the
generated HLS window and started a new session, even when the window already
ran to the movie end. The fix serves such seeks locally via
localHlsTailSeekMs/hlsWindowReachesMediaEnd. This suite:

- compiles the real extracted functions with the Gradle-cached Kotlin compiler
  and drives boundary cases (see Issue484HlsTailSeekDriver.kt);
- checks that the local-tail branch in seekTo runs before the restart branch,
  so a reordering cannot silently bring the disconnect back.

It does not replace clients/tv-android/app/src/test PlayerSeekTest, which
covers the happy paths. Real Fire TV playback is not exercised here.
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
DRIVER = Path(__file__).with_name("Issue484HlsTailSeekDriver.kt")

FUNCTION_MARKERS = (
    "internal fun shouldRestartHlsPlaybackForSeek(",
    "internal const val HLS_WINDOW_END_TOLERANCE_MS",
    "internal fun hlsWindowReachesMediaEnd(",
    "internal fun localHlsTailSeekMs(",
)


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)


def assert_local_tail_branch_precedes_restart() -> None:
    source = PLAYER_SCREEN.read_text()
    seek_start = source.find("override fun seekTo(mediaItemIndex: Int, positionMs: Long) {")
    if seek_start == -1:
        fail("FullDurationPlayer.seekTo(mediaItemIndex, positionMs) is missing")
    seek_end = source.find("\n    override fun seekForward()", seek_start)
    body = source[seek_start:seek_end]

    local_at = body.find("localHlsTailSeekMs(")
    restart_at = body.find("if (restartOutsideAvailableWindow && outsideAvailableWindow)")
    if local_at == -1:
        fail(
            "seekTo no longer consults localHlsTailSeekMs — tail seeks inside a "
            "window that already reaches the movie end would restart the session (#484)"
        )
    if restart_at == -1:
        fail("seekTo restart branch is missing or changed shape; re-check against #484")
    if local_at > restart_at:
        fail(
            "the local tail branch sits after the restart branch in seekTo — the "
            "restart would win and the disconnect/reconnect of #484 returns"
        )
    print("ok seekTo serves reachable tail seeks locally before the restart branch")


def extract_function_sources() -> str:
    source = PLAYER_SCREEN.read_text()
    chunks: list[str] = []
    for marker in FUNCTION_MARKERS:
        start = source.find(marker)
        if start == -1:
            fail(f"{marker.strip()} is missing from PlayerScreen.kt")
        end_candidates = [
            index
            for index in (
                source.find("\n\n", start),
                source.find("\n/**", start),
            )
            if index != -1
        ]
        if not end_candidates:
            fail(f"could not find the end of {marker.strip()}")
        chunks.append(source[start : min(end_candidates)])
    joined = "\n\n".join(chunks)
    if "positionOffsetMs + availableDurationMs >= fullDurationMs - HLS_WINDOW_END_TOLERANCE_MS" not in joined:
        fail("extracted hlsWindowReachesMediaEnd no longer compares the window end to the movie end")
    if "coerceAtMost(availableDurationMs)" not in joined:
        fail("extracted localHlsTailSeekMs no longer clamps to the generated window")
    return joined


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


def run_driver_on_real_functions() -> None:
    functions = extract_function_sources()
    with tempfile.TemporaryDirectory() as temp:
        temp_path = Path(temp)
        src_dir = temp_path / "src"
        src_dir.mkdir()
        # Media3's C.TIME_UNSET is Long.MIN_VALUE + 1; a minimal stand-in keeps
        # the extracted functions compiling without the Android dependency.
        stub = src_dir / "MediaCStub.kt"
        stub.write_text(
            "package androidx.media3.common\n\n"
            "object C {\n    const val TIME_UNSET: Long = Long.MIN_VALUE + 1\n}\n"
        )
        fn_file = src_dir / "HlsTailSeekFunctions.kt"
        fn_file.write_text(
            "package app.swarm.tv.app.ui.screens\n\n"
            "import androidx.media3.common.C\n\n"
            + functions
            + "\n"
        )
        out = temp_path / "classes"
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
                str(stub),
                str(fn_file),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            fail(
                "HLS tail-seek driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            [
                "java",
                "-cp",
                ":".join([str(out), stdlib_classpath]),
                "app.swarm.tv.app.ui.screens.Issue484HlsTailSeekDriverKt",
            ],
            capture_output=True,
            text=True,
            timeout=300,
        )
        if run_result.returncode:
            fail(
                "HLS tail-seek boundary checks failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_local_tail_branch_precedes_restart()
    run_driver_on_real_functions()
    print("ok issue #484 HLS tail-seek adversarial checks passed")


if __name__ == "__main__":
    main()
