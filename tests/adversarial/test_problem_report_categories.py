#!/usr/bin/env python3
"""Issue #354: Report-a-problem popup categories reach the media server.

When "Report a problem" is selected, the viewer must be offered exactly:

    Playback Video, Playback Audio, Artwork, Content, Language, Subtitle

Those labels are the triage context the media server receives. This suite
compiles and executes the production enum rather than re-listing the strings
in Python.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CATEGORY = (
    ROOT
    / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/data/ProblemReportCategory.kt"
)
DRIVER = Path(__file__).with_name("ProblemReportCategoryDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"
ISSUE_LABELS = [
    "Playback Video",
    "Playback Audio",
    "Artwork",
    "Content",
    "Language",
    "Subtitle",
]


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


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
        fail("kotlin compiler is absent from the Gradle cache")
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
        fail("kotlin stdlib is absent from the Gradle cache")
    return ":".join(dict.fromkeys(jars))


def assert_production_enum_declares_issue_labels() -> None:
    if not CATEGORY.is_file():
        fail(f"missing production category source: {CATEGORY}")
    text = CATEGORY.read_text()
    for label in ISSUE_LABELS:
        if f'("{label}")' not in text:
            fail(f"ProblemReportCategory is missing the issue-required label {label!r}")
    print("ok production enum declares the six issue labels")


def assert_driver_executes_production_enum() -> None:
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
                str(CATEGORY),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            fail(
                "ProblemReportCategory driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "ProblemReportCategoryDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "category/server-message invariant failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_production_enum_declares_issue_labels()
    assert_driver_executes_production_enum()
    print("PASS: Issue #354 problem-report category UAT contract")


if __name__ == "__main__":
    main()
