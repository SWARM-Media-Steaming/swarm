#!/usr/bin/env python3
"""Adversarial UAT for Issue #497 (pause-screen dropdowns must display multiple values):

Issue #497 requirement (verbatim):
  "On this screen audio boost has multiple buttons. Same with audio language and subtitles. I want each of these to be replaced with a drop-down that is used to view multiple values instead of having multiple buttons."

Verifies with Compose's real `ColumnMeasurePolicy`, `ButtonDefaults.ContentPadding`,
and `TwoDimensionalFocusSearchKt` on a 1080p Fire TV viewport (`1920x1080` at `xhdpi`
`density = 2.0`, `960.dp x 540.dp`) that opening Audio Boost, Audio language, or
Subtitles while "More like this" recommendations are shown lays out every option
row with non-clipped height in a multi-value viewport and allows sequential D-pad
Down traversal across the options without collapsing options to 0px or jumping out
onto "More like this".
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
UI_DIR = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui"
PLAYER_SCREEN = UI_DIR / "screens/PlayerScreen.kt"
PAUSE_DROPDOWN_OPTIONS = UI_DIR / "screens/PauseDropdownOptions.kt"
DRIVER = Path(__file__).with_name("Issue497PauseDropdownLayoutDriver.kt")


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)


def match_delim(source: str, open_at: int, opener: str = "{", closer: str = "}") -> int:
    depth = 0
    for idx in range(open_at, len(source)):
        ch = source[idx]
        if ch == opener:
            depth += 1
        elif ch == closer:
            depth -= 1
            if depth == 0:
                return idx
    fail(f"unbalanced {opener}{closer} starting at {open_at}")
    raise AssertionError


def find_function_body(sources: dict[Path, str], name_pattern: str) -> tuple[Path, str] | None:
    regex = re.compile(name_pattern)
    for path, src in sources.items():
        m = regex.search(src)
        if not m:
            continue
        paren = src.find("(", m.start())
        if paren == -1:
            continue
        after_params = match_delim(src, paren, "(", ")") + 1
        brace = src.find("{", after_params)
        if brace == -1:
            continue
        return path, src[m.start() : match_delim(src, brace, "{", "}") + 1]
    return None


def jars_named(prefix: str, group_glob: str = "**") -> list[str]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    pattern = f"**/{prefix}-*.jar" if group_glob == "**" else f"{group_glob}/**/{prefix}-*.jar"
    return [
        str(p)
        for p in sorted(cache.glob(pattern))
        if "sources" not in p.name and "javadoc" not in p.name
    ]


def aars_under(coord: str) -> list[Path]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return sorted(cache.glob(f"{coord}/**/*.aar"))


def kotlin_compiler() -> list[str]:
    embedded = jars_named("kotlin-compiler-embeddable")
    if not embedded:
        fail("kotlin-compiler-embeddable is absent from Gradle cache")
    extras: list[str] = []
    for name in (
        "kotlin-stdlib",
        "kotlin-reflect",
        "kotlin-script-runtime",
        "kotlinx-coroutines-core-jvm",
        "trove4j",
    ):
        extras.extend(jars_named(name)[:2])
    extras.extend(jars_named("annotations", "org.jetbrains/annotations")[:2])
    return [
        "java",
        "-cp",
        ":".join(dict.fromkeys([embedded[-1], *extras])),
        "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
    ]


def extract_pause_layout_flags(sources: dict[Path, str]) -> list[str]:
    pause_overlay_found = find_function_body(sources, r"\bfun\s+PauseOverlay\s*\(")
    if not pause_overlay_found:
        fail("could not find `fun PauseOverlay(` under clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui")
    _, pause_overlay = pause_overlay_found

    pause_dropdown_found = find_function_body(sources, r"\bfun\s+(?:<\w+>\s*)?PauseDropdown\s*\(")
    if not pause_dropdown_found:
        fail("could not find `fun PauseDropdown(` under clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui")
    _, pause_dropdown = pause_dropdown_found

    # Verify all three controls use PauseDropdown rather than FlowRow chip groups.
    for picker_fn in ("AudioBoostPicker", "TrackPickerBlock"):
        found = find_function_body(sources, rf"\bfun\s+{picker_fn}\s*\(")
        if not found:
            fail(f"missing `fun {picker_fn}(`")
        if "FlowRow(" in found[1]:
            fail(f"`{picker_fn}` still uses `FlowRow(` instead of a dropdown")
        if "PauseDropdown(" not in found[1]:
            fail(f"`{picker_fn}` does not delegate to `PauseDropdown(`")

    # Outer padding on PauseOverlay's main Column: `.padding(horizontal = 44.dp, vertical = 28.dp)`
    outer_h_pad = 44.0
    outer_v_pad = 28.0
    pad_match = re.search(
        r"\.padding\(\s*horizontal\s*=\s*(\d+(?:\.\d+)?)\.dp\s*,\s*vertical\s*=\s*(\d+(?:\.\d+)?)\.dp\s*\)",
        pause_overlay,
    )
    if pad_match:
        outer_h_pad = float(pad_match.group(1))
        outer_v_pad = float(pad_match.group(2))

    # Locate the right-hand picker container inside `Row(... weight(1f) ...)` if still inline there.
    top_row_idx = pause_overlay.find("Row(")
    more_idx = pause_overlay.find('"More like this"')
    top_row_block = pause_overlay[top_row_idx:more_idx] if (top_row_idx != -1 and more_idx != -1) else pause_overlay

    boost_call_idx = top_row_block.find("AudioBoostPicker(")
    right_col_header = ""
    right_col_block = ""
    if boost_call_idx != -1:
        # Walk back to the enclosing Column/LazyColumn/Box around AudioBoostPicker.
        enclosing = list(re.finditer(r"\b(?:LazyColumn|Column|Box)\s*\(", top_row_block[:boost_call_idx]))
        if enclosing:
            last_enc = enclosing[-1]
            paren_end = match_delim(top_row_block, last_enc.end() - 1, "(", ")")
            right_col_header = top_row_block[last_enc.start() : paren_end + 1]
            brace_at = top_row_block.find("{", paren_end)
            if brace_at != -1:
                right_col_block = top_row_block[last_enc.start() : match_delim(top_row_block, brace_at, "{", "}") + 1]

    right_width = 285.0
    w_match = re.search(r"\.width\(\s*(\d+(?:\.\d+)?)\.dp\s*\)", right_col_header)
    if w_match:
        right_width = float(w_match.group(1))

    picker_spacing = 18.0
    sp_matches = re.findall(r"Spacer\(\s*Modifier\.height\(\s*(\d+(?:\.\d+)?)\.dp\s*\)\s*\)", right_col_block)
    if sp_matches:
        picker_spacing = float(sp_matches[0])

    # Check if PauseOverlay's outer Column, top Row, or right-hand picker Column scrolls vertically.
    outer_col_header = pause_overlay[: top_row_idx if top_row_idx != -1 else len(pause_overlay)]
    right_column_scrollable = (
        "verticalScroll(" in outer_col_header
        or "LazyColumn(" in outer_col_header
        or "verticalScroll(" in right_col_header
        or right_col_header.startswith("LazyColumn")
    )

    # Also check if right_col_block delegates to a helper composable that scrolls.
    for helper_call in re.findall(r"\b([A-Z]\w+)\s*\(", right_col_block):
        if helper_call in ("Spacer", "Text", "Column", "Row", "Box", "AudioBoostPicker", "TrackPickerBlock"):
            continue
        helper_found = find_function_body(sources, rf"\bfun\s+{re.escape(helper_call)}\s*\(")
        if helper_found and ("verticalScroll(" in helper_found[1] or "LazyColumn(" in helper_found[1]):
            right_column_scrollable = True

    # Check if PauseDropdown renders its expanded options in a Popup / Dialog / DropdownMenu
    # or if PauseOverlay renders an expanded picker overlay outside top_row_block.
    after_more_block = pause_overlay[more_idx:] if more_idx != -1 else ""
    dropdown_uses_popup = any(
        token in pause_dropdown
        for token in ("Popup(", "Dialog(", "DropdownMenu(", "ModalBottomSheet(")
    ) or any(
        token in after_more_block
        for token in ("PauseDropdown", "PAUSE_SUBTITLE_OPTION_PREFIX", "PAUSE_AUDIO_OPTION_PREFIX", "Popup(")
    )

    # Check if PauseDropdown's expanded options list itself scrolls vertically and/or sets a max height.
    exp_idx = pause_dropdown.find("if (expanded)")
    exp_block = pause_dropdown[exp_idx:] if exp_idx != -1 else pause_dropdown
    dropdown_options_scrollable = "verticalScroll(" in exp_block or "LazyColumn(" in exp_block

    dropdown_max_h = "none"
    list_container = re.search(r"\b(?:LazyColumn|Column)\s*\(", exp_block)
    if list_container:
        lc_end = match_delim(exp_block, list_container.end() - 1, "(", ")")
        lc_header = exp_block[list_container.start() : lc_end + 1]
        max_h_match = re.search(r"\.(?:heightIn\([^)]*max\s*=\s*|height\(\s*)(\d+(?:\.\d+)?)\.dp", lc_header)
        if max_h_match:
            dropdown_max_h = max_h_match.group(1)

    opt_sp_match = re.search(r"Arrangement\.spacedBy\(\s*(\d+(?:\.\d+)?)\.dp\s*\)", exp_block)
    option_spacing = float(opt_sp_match.group(1)) if opt_sp_match else 4.0

    # Check if PauseOverlay hides collapsed sibling pickers when one dropdown is expanded.
    hides_siblings = bool(
        right_col_block
        and re.search(r"\bif\s*\([^)]*expanded", right_col_block, re.IGNORECASE)
    )

    # Extract "More like this" block spacers and LazyRow height.
    more_top_sp = 12.0
    more_hdr_sp = 8.0
    more_row_h = 186.0
    if more_idx != -1:
        rec_idx = pause_overlay.rfind("if (recommendations.isNotEmpty())", 0, more_idx + 1)
        more_section = pause_overlay[rec_idx if rec_idx != -1 else more_idx :]
        before_title = more_section[: more_section.find('"More like this"')]
        after_title = more_section[more_section.find('"More like this"') :]
        m1 = re.findall(r"Spacer\(\s*Modifier\.height\(\s*(\d+(?:\.\d+)?)\.dp\s*\)\s*\)", before_title)
        if m1:
            more_top_sp = float(m1[-1])
        lazy_row_at = after_title.find("LazyRow(")
        if lazy_row_at != -1:
            between = after_title[:lazy_row_at]
            m2 = re.findall(r"Spacer\(\s*Modifier\.height\(\s*(\d+(?:\.\d+)?)\.dp\s*\)\s*\)", between)
            if m2:
                more_hdr_sp = float(m2[0])
            lr_end = match_delim(after_title, lazy_row_at + len("LazyRow"), "(", ")")
            lr_header = after_title[lazy_row_at : lr_end + 1]
            mh = re.search(r"\.height\(\s*(\d+(?:\.\d+)?)\.dp\s*\)", lr_header)
            if mh:
                more_row_h = float(mh.group(1))

    return [
        f"--outer-horizontal-padding-dp={outer_h_pad}",
        f"--outer-vertical-padding-dp={outer_v_pad}",
        f"--right-column-width-dp={right_width}",
        f"--picker-spacing-dp={picker_spacing}",
        f"--option-spacing-dp={option_spacing}",
        f"--more-like-this-top-spacer-dp={more_top_sp}",
        f"--more-like-this-header-spacer-dp={more_hdr_sp}",
        f"--more-like-this-row-height-dp={more_row_h}",
        f"--right-column-scrollable={'true' if right_column_scrollable else 'false'}",
        f"--dropdown-uses-popup-or-overlay={'true' if dropdown_uses_popup else 'false'}",
        f"--dropdown-options-scrollable={'true' if dropdown_options_scrollable else 'false'}",
        f"--dropdown-options-max-height-dp={dropdown_max_h}",
        f"--hides-sibling-pickers-when-expanded={'true' if hides_siblings else 'false'}",
    ]


def main() -> None:
    for required in (PLAYER_SCREEN, PAUSE_DROPDOWN_OPTIONS, DRIVER):
        if not required.is_file():
            fail(f"missing required file: {required}")

    ui_sources = {p: p.read_text() for p in sorted(UI_DIR.rglob("*.kt"))}
    driver_flags = extract_pause_layout_flags(ui_sources)

    required_aars = [
        "androidx.compose.runtime/runtime-android",
        "androidx.compose.ui/ui-android",
        "androidx.compose.ui/ui-geometry-android",
        "androidx.compose.ui/ui-unit-android",
        "androidx.compose.ui/ui-graphics-android",
        "androidx.compose.foundation/foundation-android",
        "androidx.compose.foundation/foundation-layout-android",
        "androidx.tv/tv-material",
    ]

    with tempfile.TemporaryDirectory() as temp:
        temp_path = Path(temp)
        cp_jars: list[str] = []
        for idx, coord in enumerate(required_aars):
            found = aars_under(coord)
            if not found:
                fail(f"missing AAR in Gradle cache: {coord}")
            sub = temp_path / f"aar_{idx}"
            sub.mkdir()
            with zipfile.ZipFile(found[-1]) as zf:
                zf.extract("classes.jar", sub)
            cp_jars.append(str(sub / "classes.jar"))

        cp_jars.extend(jars_named("annotations", "org.jetbrains/annotations")[:2])
        for jar_name in ("kotlin-stdlib", "collection-jvm"):
            cp_jars.extend(jars_named(jar_name)[:2])
        classpath = ":".join(dict.fromkeys(cp_jars))

        out_dir = temp_path / "classes"
        out_dir.mkdir()
        env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}

        compile_res = subprocess.run(
            kotlin_compiler()
            + [
                "-no-stdlib",
                "-no-reflect",
                "-cp",
                classpath,
                "-d",
                str(out_dir),
                str(PAUSE_DROPDOWN_OPTIONS),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
            env=env,
        )
        if compile_res.returncode:
            fail(f"Kotlin driver compilation failed:\n{compile_res.stdout}\n{compile_res.stderr}")

        run_res = subprocess.run(
            [
                "java",
                "-cp",
                f"{out_dir}:{classpath}",
                "app.swarm.tv.app.ui.screens.Issue497PauseDropdownLayoutDriverKt",
                *driver_flags,
            ],
            capture_output=True,
            text=True,
            timeout=120,
            env=env,
        )
        if run_res.stdout.strip():
            print(run_res.stdout.strip())
        if run_res.returncode or "ALL_OK" not in run_res.stdout:
            fail(
                "Pause-screen dropdown multi-value layout / D-pad visibility check failed:\n"
                f"flags={' '.join(driver_flags)}\n"
                f"{run_res.stderr.strip()}"
            )

    print("PASS: Issue #497 pause-screen dropdowns display and traverse multiple values on 1080p TV")


if __name__ == "__main__":
    main()
