#!/usr/bin/env python3
"""Issue #376 contract: music /art cover 404 must not fire for an existing file.

TV catalog cards request `/art/{entry_key}/cover?v=...&w=320` and
now-playing/detail screens request `/art/{entry_key}/cover` without `w`.
A catalogued cover whose file is still on disk must therefore return 200.
SMB mounts can store a directory or filename under a different Unicode
normalization than an older artwork row, so a raw
`root.join(relative_path).is_file()` miss is not evidence the cover is gone.

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
HTTP_MEDIA = ROOT / "apps/server/src/http_media.rs"
VIEW_MODEL = (
    ROOT
    / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/data/SwarmViewModel.kt"
)
ROKU_ARTWORK = ROOT / "clients/tv-roku/src/source/Artwork.bs"


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


def assert_tv_clients_request_music_cover() -> None:
    source = read(VIEW_MODEL)
    artwork_url = function_body(source, "fun artworkUrl(")
    require(
        'MediaKind.TRACK)"cover"' in artwork_url.replace(" ", ""),
        "Fire TV catalog cards must request kind=cover for tracks",
    )
    require(
        "/art/${entry.entry.entryKey}/$kind" in artwork_url,
        "Fire TV catalog cards must hit /art/{entry_key}/{kind}",
    )
    require(
        "w=320" in artwork_url,
        "Fire TV catalog cards request shelf covers with w=320; that query must still go through the same art() resolver",
    )

    full = function_body(source, "fun fullArtworkUrl(")
    require(
        'MediaKind.TRACK)"cover"' in full.replace(" ", ""),
        "now-playing/detail artwork must request kind=cover for tracks",
    )
    require(
        "w=320" not in full,
        "full cover URL is the no-thumbnail /art/{key}/cover form used on now-playing",
    )

    roku = read(ROKU_ARTWORK)
    require(
        '"/art/"' in roku or "/art/" in roku,
        "Roku artwork helper must request the /art/{entry_key}/{kind} route",
    )


def assert_art_uses_existing_file_resolver() -> None:
    source = read(SERVE)
    art = function_body(source, "async fn art(")
    require(
        "self.roots.resolve_existing(&relative_path)" in art,
        "/art must resolve the catalog artwork path through resolve_existing so an NFC/NFD SMB mismatch does not become a 404",
    )
    require(
        "self.roots.resolve(&relative_path)" not in art,
        "/art must not fall back to a raw join that misses a still-present cover file",
    )
    require(
        "cached_artwork_path(" in art,
        "disk-cache fill must run on the already-resolved existing path, not a second exact join",
    )
    require(
        "thumbnail_path(" in art,
        "TV w=320 thumbnail generation must run on the already-resolved existing path",
    )
    require(
        "return status(404)" in art,
        "/art must still 404 when the existing-file resolver reports no file",
    )
    require(
        "mark_entry_missing" not in art,
        "a missing cover must not hide the music row (issue #73 self-heal is a playback concern)",
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


def assert_http_surface_does_not_join_artwork_paths() -> None:
    source = read(HTTP_MEDIA)
    require(
        '"/art/{entry_key}/{kind}"' in source or "/art/{entry_key}/{kind}" in source,
        "the HTTP media surface must expose GET /art/{entry_key}/{kind}",
    )
    require(
        "resolve_for_network" in source,
        "the HTTP media surface must serve artwork through MediaService, not a second path joiner",
    )


def main() -> None:
    assert_tv_clients_request_music_cover()
    assert_art_uses_existing_file_resolver()
    assert_resolver_rules()
    assert_http_surface_does_not_join_artwork_paths()
    print("PASS: issue #376 music artwork 404 contract")


if __name__ == "__main__":
    main()
