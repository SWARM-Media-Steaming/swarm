# Feature-parity inventory

The living tracking artifact for [[swarm-cross-client-parity]]. One row per user-visible
SWARM behavior, status per client. Update this file **in the same change** that alters the
behavior on any client — see [[swarm-cross-client-parity]]'s checklist for the procedure.

Status values: **Complete** (matches Fire TV's behavior exactly) · **Partial** (present but
missing a documented piece) · **Not implemented** · **Deviation** (intentionally different,
reason recorded in the Notes column or the deviation table at the bottom).

Fire TV is the reference column — its rows are "Complete" by definition except where noted
as a deviation candidate itself (rare: only if a Roku/future-platform limitation forces a
Fire TV change too, which should be rare and deliberate).

**Roku status as of this writing**: a first real, compiling vertical slice —
`clients/tv-roku` — covering the core loop (discover → pair → browse → play → resume) end
to end, deliberately scoped down from full parity to ship something genuinely working rather
than a larger surface that's all stubs. Every row below reflects what actually exists in the
code, not the target design. See `clients/tv-roku/README.md` for the build/deploy
instructions and the same gap list in narrative form.

## Core connectivity

| Feature | Fire TV | Roku | Notes |
|---|---|---|---|
| STUN registration / activation code flow | Complete | Not implemented | **Scope decision, not a gap**: Roku v1 is LAN-first by design (see platform-notes/roku.md) — no STUN swarm membership at all yet |
| Join additional swarm by code | Complete | Not implemented | Depends on STUN registration above |
| Leave swarm / switch active swarm | Complete | Not implemented | Depends on STUN registration above |
| LAN discovery (mDNS) | Complete (`NsdManager`) | Partial | `Swarm.Mdns`/`MdnsDiscoveryTask` implemented (hand-rolled DNS-SD over `roDatagramSocket`); the parsing algorithm was independently verified against a hand-built packet, but the socket I/O itself is **unverified on real hardware** — see platform-notes/roku.md's open items |
| LAN pairing | Complete (raw NDJSON, `lan.rs`) | Complete | `PairingTask` implements `/pair/begin`+`/pair/poll` end-to-end, folded into `DashboardScreen` as a modal state (matching Fire TV's `LanPairingOverlay` pattern) rather than a separate top-level screen |
| Remote (off-LAN) server reach | Complete (QUIC hole-punch) | Not implemented | **Gap, tracked**: server now issues TLS (CA + leaf, `http_media_tls_bind`/`:8547`) so a *reachable* address can be secured, but the relay that would let Roku *reach* a server with no direct network path is not built. Roku v1 only ever dials `http://` addresses found via mDNS. |
| Manual server address entry | N/A (Fire TV always has LAN+STUN) | Not implemented | Deferred — needs a Keyboard-overlay component not yet built. Real gap for the "mDNS didn't work" fallback case. |
| Disconnect / reconnect / forget a server | Complete | Partial | "Forget this server" implemented (`DashboardScreen`); there is only ever one saved connection at a time (v1 simplification — Fire TV supports multiple known LAN servers), so reconnect/switch-between-servers doesn't apply yet |
| Dashboard presence refresh (10s poll) | Complete | Not implemented | |
| SWARM service outage vs. server offline messaging | Complete — a failed roster fetch shows "Can't reach the SWARM service" (`Dashboard.serviceUnreachable`), a registered-but-disconnected server is named as offline, and only an actually empty swarm says nobody has joined; servers this TV can see over mDNS stay online through a roster outage | Not implemented | **Gap, tracked with "Remote (off-LAN) server reach"**: Roku never reads the SWARM roster today, so it cannot hit this failure. Whoever builds Roku's SWARM path must show the same three distinct messages (service down / server offline / nobody joined) — one "no servers" line for all three sent people debugging the wrong thing. |
| LAN server status wording, and Browse when a paired server is found | Complete — rows read connected / available (found, no session) / offline / disconnected; Browse connects to a found paired server when there is no session; a user-initiated Connect that fails says so | N/A | Roku has no per-server status labels and no disabled Browse: "Browse Library" only appears once connected, a found server is a "Pair with <name>" button, and failures show in the pairing status text. The three Fire TV problems ("connected" meaning only "found", Browse greyed beside a found paired server, a silent failed Connect) cannot occur. If Roku gains per-server status rows, reuse Fire TV's four states and the two rules in ux-rules.md. |
| Device/app-build testing mode (debug builds) | Complete | Not implemented | Lower priority — dev/QA convenience, not user-facing |

## Catalog & browsing

| Feature | Fire TV | Roku | Notes |
|---|---|---|---|
| Merged multi-server catalog | Complete | Not implemented | Roku v1 talks to exactly one paired server (see "Disconnect/reconnect" above) |
| Movies shelf | Complete (+ genre sub-shelves) | Partial | `CatalogScreen` shows a flat Movies shelf, first 10 entries, no genre sub-shelves/sort-by-rating |
| Shows shelf (grouped) | Complete (+ genre sub-shelves) | Partial | Grouped by title (`Swarm.CatalogGrouping.Shows`), first 10 shows, no genre sub-shelves. Groups with no numbered season (`season > 0`) containing a numbered episode (`episode > 0`) are omitted — same `previewSeasons` rule as Fire TV (#351/#360), so extras-only / season-0-only shows never appear as a show row. Selecting a show **plays its first episode directly** — no season/episode grid yet (real, documented simplification) |
| Music/artist shelf (grouped) | Complete (+ genre sub-shelves) | Partial | Grouped by artist, first 10 artists, no genre sub-shelves. Selecting an artist **plays its first track directly** — no album/track grid yet |
| Continue Watching row (cap 6) | Complete | Not implemented | |
| Watchlist row | Complete | Not implemented | Watchlist *toggle* exists (`MovieDetailScreen`) and persists; there's no row surfacing it yet |
| Netflix-style top bar (search icon, Movies default, Shows, Music) + category-tile first row (genres ranked by asset count, no artwork, no "liked", no "Categories" section label — #352); replaced the filter sidebar in #324 | Complete | Not implemented | |
| Search (submit-only, not live-filter; opened from the top-bar search icon as a popup, spans all kinds) | Complete | Not implemented | Roku has no search UI yet. When it lands, it must group the filtered catalog through `CatalogGrouping.Shows` (already drops extras-only / 0-season groups; Fire TV does the equivalent with `previewSeasons` after search). Do not introduce a second grouping path that skips that filter. |
| "Browse All" grids (alphabetical) | Complete | Not implemented | Shelves are capped at 10 items with no overflow grid |
| Genre-filtered full grid | Complete | Not implemented | |
| Live catalog change-feed (long-poll delta) | Complete | Not implemented | `CatalogTask` does one full `/catalog/manifest` fetch per screen visit, no `.gz`, no `/catalog/changes` delta polling — documented v1 simplification, real bandwidth cost on a large library |
| Catalog cache (offline/warm-start paint) | Complete (files) | Not implemented | Every catalog visit is a fresh network fetch |
| Hover/browse preview playback | Complete | Not implemented | |
| Movie detail screen | Complete | Partial | `MovieDetailScreen`: poster, title, year, genres, Play/Like/Watchlist. No backdrop, cast, overview, or report-a-problem |
| Show → season → episode navigation | Complete | Not implemented | See "plays first episode directly" above |
| Artist → album → track navigation | Complete | Not implemented | See "plays first track directly" above |

## Playback

| Feature | Fire TV | Roku | Notes |
|---|---|---|---|
| Direct-play (Range-served bytes) | Complete | Partial | `PlayerScreen` negotiates and plays direct-mode content via the `Video` node's `PlayStart`; exact `ContentNode`/`Video` field behavior is **unverified on real hardware** |
| HLS adaptive/remux playback | Complete | Partial | Implemented (`streamFormat="hls"`); fMP4/CMAF + `EVENT` playlist compatibility is the single highest flagged risk — validate on real Roku hardware first, see platform-notes/roku.md |
| Resume from saved position | Complete | Complete | Same HLS-offset-vs-direct-`PlayStart` split as Fire TV, same 95% watched threshold, same "watched always restarts at 0" rule |
| Session negotiation discipline (`/stop` on every teardown) | Complete | Complete | `PlaybackTask`'s `stop` operation fires on every exit path (Back, playback finished) in `PlayerScreen.teardown()` |
| Capability profile sent with negotiation | Real device probe, union-with-baseline | Partial | `Swarm.Capability.Baseline()` is always the static baseline — no real `roDeviceInfo` decode probe yet. Safe by construction (under-advertising only costs an unnecessary server transcode) but not yet optimal |
| Pause overlay (metadata, cast, recommendations) | Complete | Not implemented | `PlayerScreen` has OK-to-toggle-pause only, no overlay UI |
| Skip intro / recap / credits markers | Complete | Not implemented | |
| "Up next" continue overlay + countdown autoplay | Complete | Not implemented | On finish, `PlayerScreen` just tears down and pops back |
| Next-episode preload | Complete | Not implemented | |
| Buffering-triggered quality recovery | Complete | Not implemented | |
| Audio/subtitle track selection | Complete | Not implemented | `PlaybackPlan.subtitles` is parsed by `PlaybackTask` but not yet wired into the `Video` node |
| Session-expiry recovery (renegotiate on stale 404) | Complete | Not implemented | |
| Music playback screen (lyrics, shuffle, repeat) | Complete | Not implemented | Music tracks currently play through the same `PlayerScreen` as video, audio-only, no dedicated UI |
| Music minimize to mini-player (Back semantics) | Complete | Not implemented | |
| Music track preload / gapless auto-advance | Complete | Not implemented | |
| Client-error reporting on playback failure | Complete | Not implemented | `/errors/report` is documented in http-client-contract.md but no client call site exists yet |

## Device preferences & state

| Feature | Fire TV | Roku | Notes |
|---|---|---|---|
| Settings: server URL, device name | Complete (editable) | Partial | `SettingsScreen` shows device name/server/address read-only; editing needs a Keyboard-overlay component not yet built |
| Kid Mode (PIN, rules, single-chokepoint filter) | Complete | Not implemented | |
| Watch state persistence (95% threshold) | Complete | Deviation | `Swarm.Registry.SetWatchState`/`GetWatchState`, same 95% threshold and restart-from-0 rule, but **LRU-capped at 500 entries** (registry size limit) vs. Fire TV's unbounded SharedPreferences store |
| Watchlist persistence | Complete | Deviation | `Swarm.Registry` watchlist functions implemented (movie-only key shape so far, `"movie:<fingerprint>"`), **LRU-capped at 300 entries** |
| Likes persistence | Complete | Deviation | `Swarm.Registry.IsLiked`/`SetLiked` + best-effort server round-trip (`LikeTask`), **LRU-capped at 500 entries** |
| Resolved-problem notifications inbox | Complete | Not implemented | `Swarm.Registry` has the LRU-capped storage functions (`LoadNotifications`/`MarkNotificationSeen`/`DismissNotification`) but no fetch/UI wiring yet |
| Client-error reporting (auto + user-initiated) | Complete | Not implemented | |
| Client-error local retry queue (cap ~20) | Complete | Not implemented | |

## Images

| Feature | Fire TV | Roku | Notes |
|---|---|---|---|
| Authenticated artwork fetch | Complete | Complete (Deviation in mechanism) | `ArtworkTask` fetches via `roUrlTransfer` (bearer header) into `cachefs:/artwork/`, `PosterTile` reads the local file — `Poster` has no HTTP-header support, so this bridge is required, not optional |
| Artwork TTL cache (30 days, refresh-on-hit-only) | Complete | Deviation | v1 is unconditional "reuse if the cachefs file already exists," no TTL/expiry bookkeeping — `cachefs:/` is already OS-evictable, so this degrades gracefully but never proactively refreshes stale art |
| Artwork retry + fallback (artist photo → album cover) | Complete | Not implemented | A failed fetch just leaves the placeholder showing, no retry/backoff |
| Shelf-scroll artwork prefetch (bounded pool) | Complete (`PREFETCH_AHEAD = 4`) | Not implemented | Each visible `PosterTile` fetches its own artwork independently, naturally bounded by how many tiles exist (10/shelf) rather than an explicit 4-concurrent pool |
| Branded placeholder art (always drawn under real art) | Complete | Partial | `PosterTile` shows a flat colored rectangle placeholder, not the branded mascot/movie-camera art (no ported image assets yet) |

## UX chrome

| Feature | Fire TV | Roku | Notes |
|---|---|---|---|
| Shared palette / button interaction language | Complete | Complete | `Theme.bs` ports every `Swarm*` hex value exactly; `SwarmButton` implements the white/cyan/gold sequence |
| Toast/notification host (3 severities, cap 4) | Complete | Complete | `ToastHost`/`ToastRow`, same durations (4/5/7s), same cap, same auto-dismiss-and-remove-self behavior; visual polish (slide/fade animation) not yet ported |
| Loading indicator + themed message rotation | Complete (50-message rotation + GIF) | Partial | `LoadingScreen` shows one static message, no rotation, no animated asset |
| Exit-confirm modal | Complete | Not implemented | |
| Keep-awake during active playback only | Complete | Not implemented | No explicit keep-awake call yet (worth checking whether the `Video` node's own playing state already covers this on Roku before adding one) |
| Two-tier catalog Back (scroll to top bar, then exit) | Complete | N/A | No top bar/search yet in this pass |
| Two-tier player Back (video pause-then-exit; music minimize) | Complete | Deviation (simplified) | Roku v1's Back always tears down + exits immediately (no pause-first tier, no music-minimize) |

## Server/protocol surface consumed by the client

| Feature | Fire TV transport | Roku transport | Notes |
|---|---|---|---|
| Catalog/artwork/playback API | QUIC (`PeerRequest`) via loopback proxy | Plain HTTP to `http_media.rs` (`:8546`) | Same payload shapes both ways — see http-client-contract.md. Roku's TLS listener (`:8547`) is implemented server-side and the client persists `httpCaPem` from pairing, but v1's `Swarm.Http` calls all still default to the plain-HTTP path — the pinned-TLS call path (`GetPinned`/`PostJsonPinned`) exists but is not yet the one actually used end-to-end |
| Pairing | Raw NDJSON (`lan.rs`) or STUN activation | `/pair/begin` + `/pair/poll` | Different protocols, same product outcome — fully implemented on Roku |
| Capability profile | `MediaCodecList`/`Display` probe | Static baseline only | See "Capability profile sent with negotiation" above |

---

## Recorded intentional platform-specific deviations

A deviation is only "intentional" once it has a row here with a reason. An
undocumented difference discovered later during a parity review must either become a bug
(fix it) or get promoted to a documented deviation (record why) — it can never stay silent.

| Behavior | Fire TV | Roku | Why |
|---|---|---|---|
| Remote/off-LAN server transport | QUIC hole-punch | Not yet built (planned: TLS-passthrough relay) | No QUIC in BrightScript; a relay is the only way to preserve the "server never sees plaintext" promise for a device with no other path to a server (see plan/issue #54) |
| LAN pairing protocol | Raw NDJSON over TCP (`lan.rs`) | HTTP JSON (`/pair/begin`/`/pair/poll`) | Roku has no client cert to present; the HTTP flow mints a bearer token instead |
| Server discovery mechanism | `NsdManager` (OS mDNS) | Hand-rolled DNS-SD over `roDatagramSocket` | No mDNS API on Roku |
| Durable per-item state (watch/likes/watchlist/notifications) | Unbounded (Room/SharedPreferences) | LRU-capped (registry ~32KB/section; 500/500/300/100 entry caps respectively) | Roku registry has a hard size ceiling; Fire TV's storage does not |
| Media→player bridge | `PeerLoopbackProxy` (local HTTP↔QUIC translation) | None — `Video` node consumes server HTTP(S) URLs directly | Roku's player already speaks HTTP; the bridge only exists to work around QUIC |
| Client identity | Long-lived self-signed keypair, cert fingerprint pinned at registration | `roDeviceInfo.GetChannelClientId()` (device-level) + server-issued opaque bearer token (per pairing) | Roku's transport has no mTLS peer-identity concept to anchor a cert to |
| Transport controls UI | Media3 native `PlayerView` controller | Bare `Video` node, OK toggles play/pause | No built-in controller equivalent used yet; also avoids porting Fire TV's own #154-class "native controller keeps focus after hiding" bug class by construction |
| Artwork delivery to the image widget | Coil loads authenticated URL directly | Task fetches → `cachefs:/` → `Poster` reads local file | `Poster` has no HTTP header support |
| Show/artist selection in the catalog shelf | Opens season/episode or album/track navigation | Plays the first episode/track directly | v1 scope cut to prove the negotiate→play→resume loop end to end before building the intermediate browsing screens; tracked as **Not implemented** (season/episode and album/track navigation), not a permanent deviation |

## How to update this file

1. Any change to a feature/behavior on any client: update that client's status cell in the
   same change.
2. Marking something a **Deviation** requires adding (or already having) a row in the
   deviation table above with a real reason — "ran out of time" is not a deviation reason,
   it's a **Not implemented** status plus a follow-up issue.
3. See [[swarm-cross-client-parity]] for the full procedure and the rule this file exists
   to serve.
