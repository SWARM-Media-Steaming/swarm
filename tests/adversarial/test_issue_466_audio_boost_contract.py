#!/usr/bin/env python3
"""Adversarial contract checks for issue #466 (Fire TV Audio Boost), executed.

The sibling suite test_issue_466_audio_boost.py only greps the Kotlin source.
This suite compiles the real pure mapping and status code out of
PlayerScreen.kt (verbatim, like the issue #443 suite does for seek
coalescing) and runs boundary cases against it:

- the gain mapping for every level the picker offers, plus saturation for
  values at or above the top level (no Int overflow turning a boost into 0 dB);
- "Active" is never reported for a level that applies 0 mB;
- Off, unready-session, and passthrough outputs each get their own status.

The levels are read from AndroidAudioBoostStore.LEVELS so the test follows the
offered picker values rather than a copy of them.

Needs only the Kotlin compiler jars already in the Gradle cache (the same ones
the issue #443 suite uses); no device, emulator, or network access.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
PLAYER_SCREEN = APP / "ui/screens/PlayerScreen.kt"
STORE = APP / "data/AndroidAudioBoostStore.kt"
DRIVER = Path(__file__).with_name("Issue466AudioBoostDriver.kt")


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)


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


def extract_mapping_and_status() -> str:
    source = PLAYER_SCREEN.read_text()
    start = source.find("private const val AUDIO_BOOST_MAX_MILLIBELS")
    # Bound the pure mapping/status declarations using the next stable
    # declaration. Output-path detection intentionally belongs to the sink
    # lifecycle below and must not require a source-codec MIME list here.
    end = source.find("internal const val AUDIO_BOOST_UNAVAILABLE_STATUS")
    if start == -1 or end == -1 or end <= start:
        fail("could not locate audioBoostGainMillibels/audioBoostStatusFor in PlayerScreen.kt")
    block = source[start:end]
    if "audioBoostGainMillibels(" not in block or "audioBoostStatusFor(" not in block:
        fail("extracted audio boost block is missing the gain or status function")
    unavailable = re.search(r"internal const val AUDIO_BOOST_UNAVAILABLE_STATUS = .*", source)
    if unavailable is None:
        fail("AUDIO_BOOST_UNAVAILABLE_STATUS is missing from PlayerScreen.kt")
    # The driver lives in the default package, so file-private declarations
    # are widened to internal for the compiled copy. Behaviour is unchanged.
    widened = (block + "\n" + unavailable.group(0) + "\n").replace(
        "private const val", "internal const val"
    )
    return widened


def extract_levels() -> list[int]:
    m = re.search(r"LEVELS\s*=\s*listOf\(([^)]*)\)", STORE.read_text())
    if m is None:
        fail("AndroidAudioBoostStore.LEVELS not found")
    levels = [int(x) for x in m.group(1).split(",")]
    if not levels:
        fail("AndroidAudioBoostStore.LEVELS is empty")
    return levels


def assert_contract_on_compiled_source() -> None:
    mapping_source = extract_mapping_and_status()
    levels = extract_levels()
    levels_source = (
        "internal val SUPPORTED_LEVELS_DB: List<Int> = listOf("
        + ", ".join(str(x) for x in levels)
        + ")\n"
    )
    with tempfile.TemporaryDirectory() as temp:
        src_dir = Path(temp) / "src"
        src_dir.mkdir()
        mapping_file = src_dir / "AudioBoostMapping.kt"
        mapping_file.write_text(mapping_source)
        levels_file = src_dir / "AudioBoostLevels.kt"
        levels_file.write_text(levels_source)
        out = Path(temp) / "classes"
        out.mkdir()
        stdlib = jars_named("kotlin-stdlib")
        compile_result = subprocess.run(
            kotlin_compiler()
            + [
                "-no-stdlib",
                "-no-reflect",
                "-cp",
                ":".join(stdlib),
                "-d",
                str(out),
                str(mapping_file),
                str(levels_file),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            fail(
                "audio boost driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{':'.join(stdlib)}", "Issue466AudioBoostDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "audio boost contract failed on the compiled source:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_contract_on_compiled_source()
    print("PASS: Issue #466 audio boost contract adversarial checks")


if __name__ == "__main__":
    main()
