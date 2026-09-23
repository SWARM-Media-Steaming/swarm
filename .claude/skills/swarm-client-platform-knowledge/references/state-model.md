# State, navigation, and identity model

Ported from `clients/tv-android/app/.../data/SwarmViewModel.kt` — the largest and most
load-bearing file in the Fire TV client. Every client platform must reproduce these rules;
only the *mechanism* (a SceneGraph observable node vs. a Kotlin `StateFlow`) is
platform-specific.

## "Screens are states" — no navigation library

There is no nav graph / router / back-stack library. A single sealed state type (Fire TV's
`UiState`) has one variant per screen; each variant that can be navigated *into* carries a
`previous: UiState` reference (or equivalent) pointing at what to return to. Pressing the
platform Back key is (almost) always "swap to `previous`", not a framework pop. Every
screen renders as a pure function of the current state variant plus a set of callback/intent
functions — screens never hold their own copy of server data or mutate global state
directly.

**Why this instead of a stack**: a live catalog delta (the change-feed long-poll) must be
able to rebuild every derived shelf/screen **without losing the user's current place** —
Fire TV's `replaceEmbeddedCatalog` walks the current state tree and rebuilds only the
catalog-derived parts while preserving screen identity, selected season, playback, etc.
A generic push/pop stack has no clean way to do a targeted "patch the data under
screen 3 of 5 without disturbing 1, 2, 4, 5."

**Loading is a real state, not a default.** The very first value (before session
restoration completes) must be an explicit `Loading` state — never default straight to
"signed out" or any concrete screen, or a device with valid saved credentials will
visibly flash the wrong screen for however long the restore's network round-trips take.

## The state variants (reproduce all of these)

`Loading`, `PlaybackLoading` (mid-session handoff, e.g. next-episode/reseek — keep the
last video frame or a plain black backdrop, never a spinner overlay), `PreparingPlayback`
(fresh play request; shows title+artwork+a Resume-when-ready affordance), `RequestingActivation`,
`Activating` (shows the pairing code), `Dashboard`, `Settings`, `Catalog`, `ArtistShelf`,
`ArtistAlbums`, `MovieShelf`, `MovieDetail`, `ShowShelf`, `ShowSeasons`, `Player` (branches
internally to a video vs. music treatment based on `MediaKind`). `MovieShelf` /
`ShowShelf` / `ArtistShelf` carry the originating catalog-shelf title (Movies/Shows/Music
or a genre name) so the full grid can show it at the top (#353); a catalog delta rebuilds
that same subset rather than widening a genre grid back to the whole kind.

Global overlays render as siblings on top of whatever the current state is, not as their
own state variant: a toast/notification host, a minimized-player mini-bar, an exit-confirm
modal, a testing-mode banner (debug builds).

## Identity rules — get these wrong and cross-server merge breaks

- **Content fingerprint** (not entry key) is the identity for watch state, likes, and the
  movie watchlist. It's stable across servers/rescans; the entry key is server-local and
  changes on rescan. Always persist/compare by fingerprint for these three concerns.
- **Shows and artists** (client-side groupings, not server entities) use a normalized
  canonical title as identity: `trim().lowercase()`. Two servers' same show must merge into
  one shelf card; two different shows must never collide.
- **A show row/card is only emitted when the group has a preview season**: a numbered
  season (`season > 0`) containing a numbered episode (`episode > 0`). Season 0 (specials),
  null/unnumbered seasons, and episodes with null/zero/negative numbers are bonus/extras
  and must not create a show row on their own — including when a search matches only those
  files (they would otherwise label as "0 seasons"). Fire TV: `CatalogGrouping.previewSeasons`
  after grouping. Roku: `CatalogGrouping.Shows` drops groups where `HasPreviewSeason` is
  false. Extras stay on a group that *does* have a real season.
- **Server/device trust anchor is the certificate fingerprint**, never host/port — a
  server's IP can change (DHCP) without re-pairing; a *different* certificate must never be
  silently accepted for a saved server identity, even at the same address. Fingerprint
  comparison is case/whitespace-normalized; storage keeps the raw value.

## Watch state / resume

- **Watched threshold: position ≥ 95% of duration.** (Not 90% — an earlier build used 90%
  and real content has credits starting before the nominal end; migrate old records on
  read if porting from a store that used a different threshold.)
- **A watched item always resumes from 0**, never from its old position — resume position
  lookup must explicitly exclude anything already flagged watched.
- Progress is written **locally only** — there is no server-side "playback progress"
  endpoint. Report cadence: every 15s while actively playing, plus one final write on
  screen teardown/dispose (must fire even if the app is being killed by the OS, not only on
  a clean unmount).
- Continue-Watching row: cap at **6** items, one card per show (most-recently-touched
  episode represents the whole show), exclude anything already watched, sort by
  last-touched descending.
- Season/episode-list "Resume" button is independent of the capped Continue-Watching row —
  it always resolves the single most-recently-touched unfinished episode *within that
  show*, even if a chronologically later episode in the same show was already watched.

## Kid mode

- PIN stored as `SHA-256(salt + pin)`, salt is per-device random bytes generated once at
  enable time. No KDF iteration — single round is the established baseline (a stronger KDF
  is a reasonable platform improvement but must stay parity in *behavior*, not necessarily
  hash algorithm).
- The PIN gates **management** of the feature (viewing/editing/disabling it in Settings),
  not per-session content unlock — there is no "type a PIN to browse restricted content"
  flow anywhere in the product.
- Rating checks are **fail-closed**: an item with no known rating is hidden the moment a
  rating limit is active; `null` limit means unrestricted. Movie and TV ratings use
  separate ordered scales; music has no rating concept and is never gated by rating (only
  by the allowed-kinds toggle).
- **Single enforcement chokepoint**: apply the kid-mode filter to the catalog entry list at
  the one place every derived shelf/grid/search-result is built from — never re-implement
  the check per-screen. This is what prevents restricted content leaking through a screen
  that "forgot" to check.
- Known accepted limitation: tightening a restriction takes effect instantly (re-filter);
  *widening* one only fully reflects after the next full catalog resync, because the
  unfiltered manifest isn't retained once filtered. Reproduce this rather than "fixing" it
  silently on a new platform — if a platform fixes it, that's a real behavior change and
  needs a parity decision, not an accidental one.

## Playback session & negotiation discipline

- **Exactly one playback negotiation in flight at a time.** A second play/skip/autoplay
  request while one is pending is dropped, not queued — reproduce with a simple busy-guard.
- **A monotonic generation counter** invalidates a negotiation result that arrives after the
  user has already navigated away from it (double-tap play, rapid skip, cancel-then-replay).
  Check the generation when the async result lands; discard silently if stale.
- **Every abandoned reservation is explicitly released** (`/stop/{session_id}`) — on
  minimize, on stop, on error, on screen teardown, on being superseded by a new
  negotiation. Never rely on server-side idle expiry as the primary cleanup path.
- Preview (hover) playback is a **fully real, authenticated playback session** with
  `preview: true` — not a separate lightweight mechanism — and must be released the same
  disciplined way, just on a much shorter timescale (Fire TV: ~30s cap, released the
  instant focus moves away even if that's before 30s).

## Fail-open vs. fail-loud

Enhancements degrade silently: artwork prefetch, hover previews, preload-next,
like/unlike round-trip to the server, notification dismissal, client-error reporting
itself. None of these should ever show a user-facing error on failure — log and move on.

User-initiated actions and connection state changes are fail-loud: starting/joining a
swarm, LAN pairing, explicit play, explicit settings changes, and playback failures that
actually stop what's playing all surface a toast. Reproduce the severity/duration
convention (three tiers, roughly 4s/5s/7s for success/warning/error, capped concurrent
toast count, newest-message-wins dedupe on repeat) — see ux-rules.md for the exact values.

## Capability merge algorithm (reproduce exactly)

```
merge(probed, baseline):
  containers  = union(baseline.containers, probed.containers)   # dedupe, baseline first
  video_codecs = union(baseline.video_codecs, probed.video_codecs)
  audio_codecs = union(baseline.audio_codecs, probed.audio_codecs)
  max_width   = max(probed.max_width, baseline.max_width)   # then clamp to panel resolution
  max_height  = max(probed.max_height, baseline.max_height) # then clamp to panel resolution
  max_bitrate = clamp(probed.max_bitrate, baseline.max_bitrate, ABSOLUTE_CEILING)
  hdr         = probed.hdr OR baseline.hdr   # sticky-true, never sticky-false
```
A total probe failure must degrade to **exactly** the baseline — every step above is a
no-op when `probed` is empty/absent, never a partial/broken profile.
