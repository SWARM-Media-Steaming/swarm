#!/usr/bin/env python3
"""Issue #357 adversarial follow-up: the early-Resume race.

The shipped suite (test_issue_357_buffering_suppression.py) pins the case
where the viewer presses Resume on a Continue Watching cover *after*
negotiation has already produced a parked session
(`resumeFromPreparingPlayback`'s `prepared != null` branch stamps the clock
directly). It does not exercise the other branch of that same function: a
viewer who presses Resume *before* negotiation has finished.

Derived from the issue text before trusting the implementation: "suppress
the buffering notification after an asset is initially played for 10
seconds". Whichever button press or code path makes that asset actually
start playing, the 10s window must be anchored to *that* moment. If the
early-press branch failed to arrange for the clock to be stamped once
negotiation lands, `activePlaybackSessionStartedAtMs` would still hold
whatever a *previous* session left behind (or 0 on a cold start) — and a
title watched past its own 10s window, then abandoned for a fresh Continue
Watching pick with a fast tap on Resume, would have its brand new initial
buffer immediately toast: the exact original complaint, just reached via
the early-press race instead of the late-press one.

Tracing the source (as of this test's writing):
  - `resumeFromPreparingPlayback`'s early branch (`prepared == null`) sets
    `preparingResumeRequested = true` and does NOT stamp the clock itself.
  - It relies on `playEntry`'s asynchronous negotiation completion to do it:
    `playerState.startPaused = startPaused && !preparingResumeRequested`
    forces `startPaused` false once an early Resume was requested, which
    routes the `when` block away from the "park behind the cover, don't
    stamp" arm and into the `else` arm that both commits `_state.value`
    and stamps `activePlaybackSessionStartedAtMs`.
  - `preparingResumeRequested` is reset to `false` at the top of `playEntry`
    so a *later*, unrelated play does not inherit a stale early-resume flag
    from a prior negotiation.

This file pins that chain structurally (so a refactor that breaks any link
is caught) and drives it end to end in the same executable Kotlin policy
style as the shipped driver, modelling the interleaving explicitly.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VIEW_MODEL = (
    ROOT
    / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/data/SwarmViewModel.kt"
)
DRIVER = Path(__file__).with_name("EarlyResumeRaceDriver.kt")

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


def assert_early_resume_does_not_stamp_before_negotiation_lands() -> None:
    before = len(FAILURES)
    model = read(VIEW_MODEL)
    resume = function_body(model, "fun resumeFromPreparingPlayback(")
    early_branch = resume[resume.find("if (prepared != null)") :]
    else_at = early_branch.find("} else {")
    require(else_at != -1, "resumeFromPreparingPlayback must branch on whether negotiation already produced a parked session")
    early_only = early_branch[else_at:]
    require(
        "preparingResumeRequested = true" in resume[: resume.find("if (prepared != null)")],
        "the early-resume intent flag must be recorded regardless of which branch is taken",
    )
    require(
        "activePlaybackSessionStartedAtMs" not in early_only,
        "the early-press (negotiation still in flight) branch must not stamp the "
        "suppression clock itself — negotiation has not produced a session yet, "
        "so there is nothing to initially play",
    )
    if len(FAILURES) == before:
        print("ok early Resume press does not stamp the clock before a session exists")


def assert_playentry_completion_honors_early_resume_flag() -> None:
    before = len(FAILURES)
    model = read(VIEW_MODEL)
    play_entry = function_body(model, "private fun playEntry(")
    require(
        "preparingResumeRequested = false" in play_entry[: play_entry.find("viewModelScope.launch")],
        "playEntry must clear any stale early-resume flag from a prior negotiation "
        "before starting a new one, or an unrelated later play could inherit it",
    )
    require(
        "startPaused && !preparingResumeRequested" in play_entry,
        "an early Resume press must force the committed player state's startPaused "
        "false once negotiation lands, so it does not get parked a second time "
        "behind the cover without ever stamping the clock",
    )
    else_arm = play_entry[play_entry.rfind("else ->") :]
    require(
        "activePlaybackSessionStartedAtMs = SystemClock.elapsedRealtime()" in else_arm,
        "the fallback arm that commits an early-resumed session to _state.value "
        "must stamp the suppression clock there, since the early-press branch "
        "deliberately did not",
    )
    if len(FAILURES) == before:
        print("ok playEntry's completion stamps the clock for an early-resumed session")


def assert_driver_executes_the_race() -> None:
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
                "early-resume race driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "EarlyResumeRaceDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            abort(
                "early-resume race UAT failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_early_resume_does_not_stamp_before_negotiation_lands()
    assert_playentry_completion_honors_early_resume_flag()
    assert_driver_executes_the_race()
    if FAILURES:
        print(f"FAILED {len(FAILURES)} production checks", file=sys.stderr)
        sys.exit(1)
    print("PASS: Issue #357 early-Resume race does not leak a stale suppression clock")


if __name__ == "__main__":
    main()
