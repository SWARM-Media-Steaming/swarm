#!/usr/bin/env python3
"""Issue #355 contract: music /play 404 must not fire for an existing file.

The TV client reports `server could not prepare playback (404)` whenever
`/play/{entry_key}` is not HTTP 200. A catalogued track whose file is still
on disk must therefore negotiate 200. SMB mounts can store a directory or
filename under a different Unicode normalization than an older catalog row,
so a raw `root.join(relative_path).is_file()` miss is not evidence the file
is gone.

APFS is normalization-insensitive, so an on-disk NFC/NFD pair cannot prove
the fallback. This contract checks the production call sites and the
resolver's documented rules so a revert to exact-join-only cannot hide
behind the host filesystem.
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SERVE = ROOT / "crates/swarm-media/src/serve.rs"
ROOTS = ROOT / "crates/swarm-media/src/roots.rs"
CATALOG_SESSION = (
    ROOT
    / "clients/tv-android/core/src/main/kotlin/app/swarm/tv/core/catalog/CatalogSession.kt"
)
HTTP_MEDIA = ROOT / "apps/server/src/http_media.rs"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def read(path: Path) -> str:
    if not path.is_file():
        fail(f"missing {path}")
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
    fail(f"unbalanced {opener}{closer} at {open_at}")
    raise AssertionError


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start == -1:
        fail(f"missing `{signature}`")
    brace = source.find("{", start)
    if brace == -1:
        fail(f"`{signature}` has no body")
    end = match_braces(source, brace)
    return source[start : end + 1]


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def assert_client_surfaces_play_status() -> None:
    source = read(CATALOG_SESSION)
    require(
        "server could not prepare playback" in source,
        "CatalogSession must still surface the issue #355 playback-prep error text",
    )
    require(
        "response.status" in source and "/play/" in source,
        "the user-visible error must include the /play HTTP status (404 in this issue)",
    )


def assert_play_and_bytes_use_existing_file_resolver() -> None:
    source = read(SERVE)
    play = function_body(source, "async fn play(")
    require(
        "self.roots.resolve_existing(&entry.relative_path)" in play,
        "/play must resolve the catalog path through resolve_existing so an NFC/NFD SMB mismatch does not become a 404",
    )
    require(
        "self.roots.resolve(&entry.relative_path)" not in play,
        "/play must not fall back to a raw join that misses a still-present music file",
    )
    require(
        "mark_entry_missing" in play and "return status(404)" in play,
        "/play must still 404-and-self-heal only after the existing-file resolver reports no file",
    )

    media_entry = function_body(source, "async fn media_entry(")
    require(
        "self.roots.resolve_existing(&entry.relative_path)" in media_entry,
        "byte serving must use the same existing-file resolver as /play",
    )
    require(
        "self.roots.resolve(&entry.relative_path)" not in media_entry,
        "byte serving must not bypass resolve_existing with a raw join",
    )

    session_media = function_body(source, "async fn session_media(")
    require(
        "self.media_entry(" in session_media,
        "direct-play /stream/{id}/media must go through media_entry so unicode-fixed paths keep serving bytes",
    )


def assert_resolver_rules() -> None:
    source = read(ROOTS)
    body = function_body(source, "pub fn resolve_existing(")
    require(
        "exact.is_file()" in body,
        "exact filesystem spelling must win before any normalization fallback",
    )
    require(
        ".nfc()" in body,
        "the fallback must compare Unicode-normalized names (NFC), which is what distinguishes Café from Café",
    )
    require(
        "read_dir" in body,
        "the fallback must inspect the real directory listing an SMB mount returns",
    )
    require(
        "matches.next().is_some()" in body,
        "two NFC-equivalent directory entries must not be silently chosen",
    )
    require(
        "Component::Normal" in body,
        "non-normal path components (`.`, `..`) must abort the walk instead of escaping the media root",
    )

    shared = function_body(source, "impl SharedRootResolver")
    require(
        "pub fn resolve_existing(" in shared,
        "MediaService's SharedRootResolver must expose resolve_existing",
    )


def assert_http_surface_does_not_join_catalog_paths() -> None:
    source = read(HTTP_MEDIA)
    require(
        "resolve_for_network" in source,
        "the HTTP media surface must negotiate playback through MediaService, not a second path joiner",
    )
    require(
        "relative_path" not in source,
        "http_media.rs must not reconstruct catalog filesystem paths itself",
    )


def main() -> None:
    assert_client_surfaces_play_status()
    assert_play_and_bytes_use_existing_file_resolver()
    assert_resolver_rules()
    assert_http_surface_does_not_join_catalog_paths()
    print("PASS: issue #355 music playback 404 contract")


if __name__ == "__main__":
    main()
