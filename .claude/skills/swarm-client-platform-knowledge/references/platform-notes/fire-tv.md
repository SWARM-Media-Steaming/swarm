# Platform notes — Fire TV (Android)

`clients/tv-android`. Kotlin, Jetpack Compose for TV (`tv-material3`), Media3/ExoPlayer,
Room, `androidx.security.crypto` (Keystore-backed encrypted prefs), Coil, OkHttp, kwik
(pure-JVM QUIC), Coroutines/`StateFlow`. `minSdk 25`, `targetSdk 35`.

This is the **reference implementation** — every other platform-notes file describes a
delta from this one. See `.claude/skills/tv-client-ui-conventions` and
`.claude/skills/tv-client-database` for Compose/Room-specific detail this file doesn't
duplicate.

## Portable-concept ↔ platform-primitive map

| Portable concept | Fire TV primitive |
|---|---|
| Observable app state | `StateFlow<UiState>` on a single `ViewModel` |
| Async I/O | Kotlin coroutines (`viewModelScope.launch`) |
| Screen focus/D-pad | Compose `FocusRequester` + `Modifier.focusProperties` + `tv-material3` |
| Durable relational storage | Room (SQLite), 4 migrated schema versions |
| Durable flat KV storage | `SharedPreferences` (watch state, likes, watchlist — deliberately not Room); episode watch state includes show/season/episode fallback identity and timestamp-ordered writes |
| Secrets | `EncryptedSharedPreferences` (AES256-GCM, Keystore-backed master key) |
| Device identity | `AndroidKeyStore` EC keypair, non-exportable private key, 20-year self-signed cert |
| Catalog/artwork cache | Plain files under `filesDir`, `AtomicFile` writes |
| Image loading | Coil, fixed 48MB memory / 200MB disk cache, custom TTL interceptor |
| Video/audio playback | Media3 `ExoPlayer` + `PlayerView` |
| Transport (remote) | Custom QUIC (kwik) + mTLS cert pinning + UDP hole-punch |
| Transport (LAN) | Same QUIC client, address from mDNS (`NsdManager`) instead of hole-punch |
| Media→player bridge | `PeerLoopbackProxy` — a local loopback HTTP server translating HTTP range requests into QUIC `PeerRequest`s, because ExoPlayer only speaks HTTP |
| Capability probe | `MediaCodecList` + `Display.hdrCapabilities` |

## Hard platform facts worth knowing before porting from this client

- **ExoPlayer needs an HTTP(S) URL, never a raw transport handle.** This is *why*
  `PeerLoopbackProxy` exists at all — it's pure translation glue with no product behavior
  of its own. A platform whose player already speaks HTTP directly (Roku's `Video` node)
  doesn't need this layer; don't port it, port what it's translating *to* (the HTTP path
  space in http-client-contract.md) directly.
- **QUIC + mTLS + hole-punch is Android/JVM-specific** (kwik). No other TV platform in this
  product line has a usable QUIC stack. Treat the entire transport stack
  (`transport/PeerQuicClient.kt`, `Punch.kt`, `PunchConnect.kt`, `Reflector.kt`) as
  Fire-TV-only implementation, not portable pattern — the *portable* thing is the
  `PeerRequest`/`PeerResponseHeader` JSON shape and path space it carries, which
  `apps/server/src/http_media.rs` already re-exposes over plain HTTP for exactly this
  reason.
- **`getLoopbackAddress()` resolved to `::1` on real Fire TV hardware and broke
  everything** — the loopback proxy binds literal `127.0.0.1`, not the "canonical"
  loopback address. A real-hardware-only bug; worth knowing before assuming "loopback" is a
  safe abstraction on any embedded platform.
- **`AndroidKeyStore`'s first touch after `adb install -r` reproducibly crashes** on real
  Fire TV hardware — the very next cold start after a reinstall always crashed on the first
  Keystore access, and the launch after that always succeeded. Fixed with a
  retry-with-delay wrapper (`KeystoreRetry.kt`, 3 attempts / 200ms). Any platform with an
  OS-level secure-storage daemon that starts asynchronously relative to app launch should
  expect a similar class of bug and build the retry in from the start rather than
  discovering it live.
- **Large catalog JSON parsing on the main thread caused ANRs** on lower-end/32-bit Fire TV
  sticks — catalog cache I/O and change-feed delta application had to move to a background
  dispatcher explicitly. Any platform doing meaningful JSON parsing of a large manifest
  should default to doing it off the render/UI thread from the start.
- **A multidex `ClassNotFoundException`** (`SwarmTvApplication` landed in a secondary dex
  file) was real and only caught by an actual device install — no emulator, no unit test
  could have caught it. General lesson: a deploy script that installs to real hardware and
  confirms clean launch is not optional tooling for a TV client, it's the only thing that
  catches this whole class of bug.
- **Fixed, not percentage-of-available, cache size budgets** for the image loader (48MB
  memory / 200MB disk) — TV-stick memory is shared unpredictably with the video decoder,
  the compositor, and the OS; a percentage-of-available default sized against desktop/phone
  assumptions is the wrong call on this class of hardware.

## Notification/discovery specifics

- mDNS via `NsdManager`, service type `_swarm-peer._udp.` A `WifiManager.MulticastLock` is
  required on Android or some Fire TV/Android TV Wi-Fi drivers filter mDNS traffic before
  NSD ever sees it — non-obvious, confirmed live.
- Only one `NsdManager.resolveService` call may be in flight at a time — the client
  serializes resolution through a small internal queue.

## Testing posture

15 instrumented `androidTest/uat/*.kt` scenarios drive the real UI with real D-pad key
events (never `performClick()` — a documented, deliberate choice: on real Fire TV hardware
a synthetic click can land on the clipped edge of an off-screen lazy-list item and produce
false positives). 14 JVM unit tests lock down pure-logic invariants (rating scale, preview
timing math, pause recommendation scoring, artwork cache key derivation, resume-episode
selection, watch-state migration, LAN route trust, playback-connection-tracker dedup,
diagnostics redaction). See `.claude/skills/swarm-tv-uat-suite` and
`.claude/skills/swarm-closed-loop-tv-testing` for the harness detail.
