#!/usr/bin/env python3
"""Issue #406: desktop artwork commands must not exact-join catalog paths.

Expected behavior is derived from the desktop command boundary in the issue:
an artwork row may retain NFC while an SMB listing exposes an NFD component.
The Media-tab command must return the bytes of that still-present cover, and
clearing the scrape must best-effort delete that same resolved file.  A plain
exact join is insufficient on a normalization-sensitive SMB mount.

The real mocked-Tauri UAT is run below as well.  Some local filesystems treat
NFC and NFD as the same name, so source-level assertions deliberately guard
the resolver choice rather than letting that host property mask a regression.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
GUI = ROOT / "apps/server/src/gui.rs"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        fail(f"missing command `{signature}`")
    open_brace = source.find("{", start)
    if open_brace < 0:
        fail(f"command `{signature}` has no body")
    depth = 0
    for index, character in enumerate(source[open_brace:], open_brace):
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    fail(f"command `{signature}` has unbalanced braces")
    raise AssertionError


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def assert_desktop_read_contract(source: str) -> None:
    command = body(source, "async fn get_artwork_bytes")
    require(
        "core.media_roots.resolve_existing(&relative_path)" in command,
        "get_artwork_bytes must resolve a catalog artwork path through resolve_existing",
    )
    require(
        "core.media_roots.resolve(&relative_path)" not in command,
        "get_artwork_bytes must not use an exact-only artwork path join",
    )
    require(
        "Ok(bytes) => Ok(Some(bytes))" in command,
        "an existing resolved cover must be returned as bytes",
    )
    require(
        "Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None)" in command,
        "a genuinely absent cover must remain a non-error None result",
    )
    require(
        "Err(e) => Err(e.to_string())" in command,
        "non-NotFound artwork read failures must remain visible to the desktop caller",
    )
    require(
        "ArtworkKind::parse(&kind)" in command
        and "unknown artwork kind" in command,
        "a malformed artwork kind must remain a command error rather than selecting a path",
    )


def assert_desktop_clear_contract(source: str) -> None:
    command = body(source, "async fn clear_scraped_metadata")
    require(
        "core.media_roots.resolve_existing(&relative_path)" in command,
        "clear_scraped_metadata must resolve NFC/NFD artwork before deletion",
    )
    require(
        "core.media_roots.resolve(&relative_path)" not in command,
        "clear_scraped_metadata must not leave an NFD SMB cover orphaned via exact join",
    )
    require(
        "let _ = tokio::fs::remove_file(&path).await;" in command,
        "artwork deletion must remain best-effort for absent or flaky network files",
    )


def run_mocked_tauri_uat() -> None:
    command = [
        "cargo",
        "test",
        "--package",
        "swarm-server",
        "--bin",
        "swarm-server-app",
        "artwork_commands_handle_unicode_normalized_smb_paths",
        "--",
        "--nocapture",
    ]
    result = subprocess.run(command, cwd=ROOT, check=False)
    require(
        result.returncode == 0,
        "the mocked-Tauri UAT must read and then delete the NFC/NFD SMB artwork fixture",
    )


def main() -> None:
    source = GUI.read_text(encoding="utf-8") if GUI.is_file() else fail(f"missing {GUI}")
    assert_desktop_read_contract(source)
    assert_desktop_clear_contract(source)
    run_mocked_tauri_uat()
    print("PASS: issue #406 desktop artwork Unicode-path contract")


if __name__ == "__main__":
    main()
