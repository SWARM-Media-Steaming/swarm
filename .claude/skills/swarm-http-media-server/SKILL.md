---
name: swarm-http-media-server
description: Use when adding or changing anything in apps/server/src/http_media.rs — the always-on, plain-HTTP(S) pairing and media-playback surface for clients that can't speak the QUIC peer transport (built as groundwork for a future Roku client, see GitHub issue #54's pinned implementation-plan comment for the full research/critique history). Covers its two-tier auth model, why it has its own TCP port instead of sharing bind's, why its crypto/rate-limit helpers are local rather than shared, the stream_body cancel-safety contract it depends on, and how to add a new route. For the dashboard's pairing UI see media-server-dashboard-ui; for the QUIC-native transport it deliberately doesn't touch see swarm-media's serve.rs.
---

# The plain-HTTP(S) pairing + media-playback surface

SWARM's real transport is a custom QUIC protocol authorized by mTLS
cert-pinning (`crates/swarm-p2p`) — that's what the Fire TV client and
every existing peer speak. `apps/server/src/http_media.rs` exists
because a device that structurally *can't* do QUIC (Roku's BrightScript
has no QUIC implementation and no realistic path to build one) still
needs a way to pair and play. It is a second, independent HTTP surface
bolted onto `ServerCore`, not a reimplementation or a replacement of the
QUIC path — read `crates/swarm-media/src/serve.rs`'s module doc comment
too before touching either.

## Two credential models on one router, and why neither is optional

`/pair/begin` and `/pair/poll` are **unauthenticated by definition** — a
device that hasn't paired yet has no credential to present. That's
compensated for, not ignored:

- `require_lan` (built on `swarm_media::serve::is_lan_ip`, now `pub` for
  exactly this reuse) rejects anything not on a private/link-local/
  loopback address.
- `reject_cross_site` rejects any request carrying `Sec-Fetch-Site` or
  `Sec-Fetch-Mode` headers at all — real device HTTP clients
  (BrightScript's `roUrlTransfer`, `curl`, etc.) never send these; only
  a browser's `fetch()`/XHR does. This exists because `is_lan_ip` alone
  can't distinguish a real pairing device from a malicious webpage
  running in a browser that happens to be on the same LAN — the
  browser's *own* IP is genuinely local, so the LAN check passes either
  way. `lan.rs`'s raw NDJSON-over-TCP pairing protocol never needed this
  because no browser can speak it; real HTTP can, so this surface does.
- `AllocationLimiter` (a local copy of `apps/stun-server/src/security.rs`'s
  struct of the same name, same fixed-window per-IP shape) rate-limits
  `/pair/begin` — `lan.rs`'s only cap is a global 32-pending-activations
  ceiling, fine when its protocol wasn't browser-reachable, not fine now
  that this one is.

Everything else (`/play`, `/stream/.../media`, `/media/{key}`,
`/hls/...`) requires `Authorization: Bearer <token>`, checked via
`require_bearer` against a per-device hash lookup in `state_db`'s
`http_media_device` table. Model any change to this check on
`apps/stun-server/src/authn.rs::require_device`'s shape (hash the
presented token, look up by hash, never store the raw token) —
**not** `apps/server/src/mcp.rs::has_valid_bearer`'s shape (a single
static shared secret compared with `==`). They look superficially
similar and mcp.rs is physically closer to copy from within the same
crate, but it's a different, weaker model with no per-device revocation
— reaching for it here would be a real regression, not a shortcut.

## Its own TCP port — don't try to share `bind`'s

`lan.rs`'s `LanService::start` already binds a raw-TCP listener on
`ServerConfig.bind`'s port number (documented rationale: sharing a
number between one TCP and one UDP listener avoids a second firewall
prompt), and it runs *before* `http_media::start` inside
`ServerCore::start`. A second TCP listener attempting that same port
would only occasionally win that race and otherwise silently fall back
to an OS-assigned ephemeral one — confirmed as a real bug during design
review, not a hypothetical. `http_media_bind` (`ServerConfig`, default
`0.0.0.0:8546`, `SWARM_HTTP_MEDIA_BIND` env override in `gui.rs` — same
pattern as `SWARM_PEER_BIND`, deliberately no Settings/UI field since
this surface has no opt-in toggle to pair one with) is a genuinely
separate port for exactly this reason. If a future change wants to
collapse this back onto one port, it has to either retire `lan.rs`'s
raw-TCP protocol first or hand both listeners a shared, coordinated bind
step — don't just change the config value.

## Local crypto/rate-limit helpers are deliberate, not an oversight

`generate_token`/`token_hash` and `AllocationLimiter` are copied from
`apps/stun-server/src/security.rs`, not imported from a shared crate.
`crates/swarm-core`'s own doc comment scopes it to wire-protocol types
and identity primitives and asks callers to keep it dependency-light;
these helpers don't fit, and `swarm-p2p`/`swarm-media`/`swarm-stun-client`
would all absorb a dependency they don't need. More importantly, there's
no cross-service *wire* compatibility requirement here the way there is
for `PeerRequest` (which must byte-match Kotlin and Rust) — this crate's
tokens are only ever validated against this crate's own table, never the
STUN server's. This mirrors an existing precedent: `is_lan_ip`
(`swarm_media::serve`) and `lan.rs`'s own `is_lan_address` are already
two independently-maintained copies of the same shape of helper. If you
need a third copy of one of these for another crate, that's consistent
with how this codebase already handles small, self-contained, no-I/O
helpers — don't "fix" it by centralizing three call sites into a new
shared crate without a real drift risk driving that.

## `state_db`'s `http_media_device` table: hard-delete, not soft-revoke

Revocation is `DELETE FROM http_media_device WHERE token_hash = ?`
(`remove_http_media_device`), matching `local_peer`'s existing
hard-delete pattern in this same file — **not**
`apps/stun-server`'s separate `devices` table, which soft-revokes via a
`revoked_at` column. Nothing outside this table needs to distinguish
"revoked" from "never paired," so don't add that column without a real
need driving it.

## Every media route funnels through `stream_body` — never hand-roll streaming here

`/play`, `/stream/.../media`, `/media/{key}`, and `/hls/...` all build a
`PeerRequest` from the incoming HTTP request (parse the `Range` header
into `ByteRange::FromTo`/`Suffix` — see `parse_range_header`), call the
already-existing `MediaService::resolve_for_network(&request, is_lan)`
(confirmed to do **zero** auth itself — `require_bearer` above is the
only thing standing between a request and this call), then hand the
resulting `Resolved` to `swarm_media::serve::stream_body` and feed its
output straight into `axum::body::Body::from_stream`. Do not write a new
manual read-loop against `Resolved::body` here. `stream_body` exists
specifically so this surface and QUIC's `handle_stream` share one
implementation of 64 KiB chunking, per-chunk rate limiting, bandwidth
accounting, and — the part that actually matters — releasing the
transcode session via a `Drop`-based guard that fires whether the stream
finishes normally *or* is dropped early (an HTTP client seeking within a
`<video>` element, or just disconnecting, abandons the response stream
mid-read routinely, not exceptionally; QUIC's `handle_stream` never had
to handle that case, this transport does). A hand-rolled loop here would
silently reintroduce the exact session-leak bug that pattern was built
to fix — see `crates/swarm-media/tests/playback.rs`'s
`dropping_a_stream_body_early_still_releases_the_session` test, which
was verified against a real regression (temporarily neutered the `Drop`
impl and confirmed the test fails) before being trusted.

`is_lan_ip` (not a bespoke check) determines the `is_lan` argument to
`resolve_for_network` — this gates whether the shared upload-bandwidth
budget applies at all, so getting it wrong in either direction is a real
cost/availability bug, not cosmetic. The router is started with
`.into_make_service_with_connect_info::<SocketAddr>()` specifically so
handlers can extract the real peer address for this — don't switch to
plain `into_make_service()` (that's `mcp.rs`'s precedent, which never
needs peer IP).

## `/health` is a third, deliberately tiny route group

`GET /health` answers "is this server up, and is it visible through the SWARM
service?" for anything monitoring it from the LAN: `{"ok": true, "swarm_link":
{"state", "failing_since", "attempts", "signaling"}}`. `state` is
`not_linked` (a LAN-only install — healthy), `connecting`, `connected` or
`unreachable` (a service is configured or saved but not answering; LAN clients
are unaffected, SWARM-paired TVs show the server offline).

It is unauthenticated, so it gets the same `require_lan` + `reject_cross_site`
layers as `/pair/*` and reports only `SwarmLinkStatus::public()` — never the
service address or error text, which can name internal hosts. Do not add
fields to it without checking that. The full status (address, last error) is
the `get_swarm_link_status` Tauri command, owner-only. Where the state comes
from and why it exists: `apps/server/src/link.rs` and `ServerCore`'s link
supervisor.

## Adding a new route

1. Decide which of the two `Router`s it belongs to (unauthenticated +
   LAN-gated `pairing_routes`, or bearer-gated `media_routes`) — don't
   add a third middleware tier without a real reason.
2. If it serves media bytes: build a `PeerRequest`, call
   `resolve_for_network`, respond via `stream_body` exactly as the
   existing media routes do.
3. If it's pairing/device-management shaped: extend `PairingState` or
   `HttpMediaService` following the existing `begin`/`poll`/`approve`
   split — `begin`/`poll` are network-facing (called from route
   handlers), `approve` is owner-only (called only from a trusted Tauri
   command in `gui.rs`, never from a route handler).
4. Add a `ServerCore` delegate method if the dashboard needs to reach it
   (see `approve_http_media_pairing`/`http_media_devices`/
   `revoke_http_media_device` for the pattern), then a matching Tauri
   command in `gui.rs` and dashboard UI in `swarm.js`/`index.html` — see
   `media-server-dashboard-ui`'s "One code box, several pairing paths
   tried in sequence" section for how approval UX composes across
   multiple pairing flows without a new input box per flow.
5. Update the end-to-end test in `apps/server/tests/http_media.rs`
   (pairs a fake device over real HTTP, negotiates playback, range-fetches
   real bytes with `reqwest`) rather than only adding a unit test — the
   unit tests in `http_media.rs`'s own `#[cfg(test)] mod tests` cover the
   pairing state machine and helpers in isolation, but wiring mistakes
   (route registration, middleware ordering, `ConnectInfo` availability)
   only surface over a real TCP connection.

## Route parity with the QUIC dispatch is complete — what's still different

Every path `resolve_for_network`'s QUIC-side dispatch recognizes
(`crates/swarm-media/src/serve.rs`) now has an HTTP route pointed at it:
`/play`, `/stream/{id}/media`, `/media/{entry_key}`, `/hls/{id}/{*rest}`,
`/catalog/thumbprint`, `/catalog/manifest[.gz]`, `/art/{entry_key}/{kind}`,
`/stop/{id}`, `/subtitles/{entry_key}/{filename}`, `/errors/report`,
`/likes/toggle`. A paired HTTP-only client can pair, browse, play,
caption, release a session early, report an error, and like/unlike —
the same feature set a QUIC peer has, not a subset. `/pair/*` itself has
no QUIC equivalent at all (a QUIC-capable peer authenticates by
presenting a cert, never needs to "pair" over the wire the way an
HTTP-only device does).

Two non-obvious fixes were needed to get full parity, both easy to miss
if you're only looking at the byte-serving routes as a template:
`media_get` must forward the full path **and query string**
(`OriginalUri::path_and_query`, not `.path()` alone) since an artwork
thumbnail-width request rides in the query (`?w=320`, parsed back out of
the same `PeerRequest.path` field by `swarm-media`'s
`artwork_thumbnail_width`); and the `If-None-Match` request header /
`ETag` response header must round-trip through
`PeerRequest.if_none_match`/`PeerResponseHeader.etag` for artwork's `304`
caching path to ever trigger. See
`browse_catalog_and_fetch_artwork_over_real_http` in
`apps/server/tests/http_media.rs` for the real-HTTP proof of both, and
`hls_master_and_nested_rendition_playlist_serve_over_real_http` for why
`/hls` specifically needed axum's `{*rest}` catch-all rather than a
fixed-depth `{rendition}/{file}` pattern.

Still true: no TLS (plain HTTP only, matching `mcp.rs`'s own precedent),
and no graceful shutdown for the listener (matches every other listener
in this app — QUIC's `accept_loop`, `lan.rs`'s TCP accept loop, `mcp.rs`
— none of which have one either).
