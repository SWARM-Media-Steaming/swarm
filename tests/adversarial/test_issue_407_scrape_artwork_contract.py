#!/usr/bin/env python3
"""Issue #407 source-level contract for normalization-aware scrape paths.

The accompanying Rust UAT drives the full public album scrape. On APFS,
however, NFC/NFD names can be treated as identical by the filesystem, so the
old exact-join implementation may appear to pass that runtime scenario. This
contract ensures the three scrape operations retain their required resolver
boundaries on every host.
"""

from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parents[2]
ARTWORK = ROOT / "crates/swarm-media/src/scrape/artwork.rs"
RUNNER = ROOT / "crates/swarm-media/src/scrape/runner.rs"
ROOTS = ROOT / "crates/swarm-media/src/roots.rs"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        fail(f"missing `{signature}`")
    open_brace = source.find("{", start)
    if open_brace < 0:
        fail(f"missing body for `{signature}`")
    depth = 0
    for index, character in enumerate(source[open_brace:], open_brace):
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    fail(f"unbalanced body for `{signature}`")
    raise AssertionError


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def main() -> None:
    artwork = ARTWORK.read_text()
    runner = RUNNER.read_text()
    roots = ROOTS.read_text()

    exists = function_body(artwork, "pub async fn exists(")
    require(
        "roots.resolve_existing(relative_path)" in exists,
        "non-force artwork existence must resolve the catalog path as an existing file, not raw-join it",
    )
    require(
        ".join(rest" not in exists,
        "artwork existence must not reconstruct an exact catalog path after normalized resolution",
    )

    save = function_body(artwork, "pub async fn save_artwork(")
    require(
        "roots.resolve_existing(relative_path)" in save,
        "artwork writes must derive their parent from the resolved source file so imports stay beside the real album directory",
    )

    local_import = function_body(runner, "async fn import_local_album_cover(")
    require(
        "roots.resolve_existing_dir(relative_parent)" in local_import,
        "local-cover import must enumerate a normalization-resolved directory rather than exact-joining the catalog parent",
    )

    resolve_dir = function_body(roots, "pub fn resolve_existing_dir(")
    require(
        "resolve_existing_matching(relative_path, Path::is_dir)" in resolve_dir,
        "directory resolution must require a directory; file-only resolution cannot enumerate an album folder",
    )
    matching = function_body(roots, "fn resolve_existing_matching(")
    require(
        ".nfc()" in matching and "matches.next().is_some()" in matching,
        "normalization fallback must compare NFC forms and reject ambiguous on-disk matches",
    )
    print("PASS: issue #407 scrape artwork normalization contract")


if __name__ == "__main__":
    main()
