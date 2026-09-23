---
name: tv-client-ui-conventions
description: Use when building or changing any screen/composable in the Fire TV client (clients/tv-android/app/src/main/kotlin/app/swarm/tv/app/ui and MainActivity/SwarmViewModel's UiState). Covers the shared palette, explicit TV button focus colors, D-pad focus, player lifecycle, screen-awake behavior, subtitles, artwork/icon safe zones, loading, and layouts confirmed against a real 1080p Fire TV screen. For real-device debugging methodology, see swarm-real-device-debugging.
---

# TV client UI conventions

Established/refined across several rounds of live-device feedback on
`clients/tv-android/app`. Read this before adding or changing a screen —
several of these are silent failure modes: the code compiles, looks
right in a design tool, and is still wrong the moment a real D-pad
touches it.

## Theme: one shared palette across all three SWARM UIs

`ui/theme/Theme.kt`'s `Swarm*` colors are a deliberate port of the STUN
server's and Tauri media server's shared `:root` CSS variables (see
`media-server-dashboard-ui`) — same hex values, same names in spirit
(`SwarmBackground` = `--bg` = `#101828`, `SwarmAccent` = `--accent` =
`#00c2ff`, etc.), so all three apps read as one product. If you add a
new semantic color here, add the matching CSS variable on the server
side too (and vice versa) — don't let them drift.

Always reference the named `Swarm*` constants (`SwarmBackground`,
`SwarmSurface`, `SwarmSurfaceMuted`, `SwarmBorder`, `SwarmAccent`,
`SwarmAccentHot`, `SwarmGreen`, `SwarmText`, `SwarmMuted`), never a
hand-picked hex literal — a literal that happens to look fine in
isolation is how a screen quietly drifts off-palette.

## tv-material3 `Button`: the focused/pressed state trap

`ButtonDefaults.colors(...)` takes *separate* params per interaction
state: `containerColor`/`contentColor` (default/unfocused),
`focusedContainerColor`/`focusedContentColor`,
`pressedContainerColor`/`pressedContentColor`. **Passing only the first
pair silently falls back to tv-material3's own built-in defaults for
the other two** — colors that were never designed against this app's
dark palette and can clash badly (confirmed live: a chip with a
correct-looking default state turned near-black-text-on-gray the moment
D-pad focus landed on it, because only `containerColor`/`contentColor`
had been set).

This is a real, load-bearing gap, not a one-off — a repo-wide grep for
`ButtonDefaults.colors(` will find call sites that still only set the
default pair (they haven't been reported because users don't linger in
a focused state on those particular buttons the way they do scanning
across many chips). When you touch a `Button`, set all three pairs
explicitly:

```kotlin
colors = ButtonDefaults.colors(
    containerColor = if (isSelected) SwarmAccent else SwarmSurfaceMuted,
    contentColor = if (isSelected) Color(0xFF04263A) else SwarmText,
    focusedContainerColor = SwarmAccent,
    focusedContentColor = Color(0xFF04263A),
    pressedContainerColor = SwarmAccent,
    pressedContentColor = Color(0xFF04263A),
)
```

`Color(0xFF04263A)` (near-black navy) is this app's standing choice for
text-on-`SwarmAccent`, matching the server dashboard's own button
foreground (`#04263a`) — reuse it rather than inventing a new
on-accent color.

## D-pad focus trapping for overlays/modals

A `Box` overlay drawn on top of background content is only *visually*
modal — D-pad focus can still traverse straight through it into
whatever's behind it unless you explicitly trap focus. Confirmed live:
a genre-picker overlay looked correct but pressing down moved focus
onto the (still-visible-behind-it) content grid, leaving the overlay on
screen but functionally unreachable/stuck.

Fix: disable focus on the background content for as long as the
overlay is showing, via `Modifier.focusProperties { canFocus = ... }`
on the background's root container:

```kotlin
Column(modifier = Modifier.focusProperties { canFocus = !showOverlay }.[...]) { ... }
```

Pair this with `BackHandler(onBack = onDismiss)` on the overlay itself
so the physical Back button closes it — see "no on-screen
Cancel/Back buttons" below.

## Initial/re-focus: `FocusRequester` + `LaunchedEffect`

Every screen or overlay that should land D-pad focus somewhere specific
the moment it appears (not "whatever the system picks first," and not
"nothing," which strands the remote) uses the same pair:

```kotlin
val playFocusRequester = remember { FocusRequester() }
LaunchedEffect(entry) { playFocusRequester.requestFocus() }
// ...
Button(modifier = Modifier.focusRequester(playFocusRequester), ...)
```

Key the `LaunchedEffect` on whatever identifies "a fresh instance of
this content" (e.g. `entry`, or `url` for a re-used player screen
autoplaying into the next episode) — otherwise a recomposition that
doesn't represent real navigation can steal focus back unexpectedly, or
a genuine navigation can fail to re-request it.

## Avoid initial-state-flash: don't default state to the wrong screen

`SwarmViewModel`'s `UiState` sealed class has a real `Loading` state
that is the *actual* initial value of `_state`, shown only until
session restoration has concluded one way or the other. This exists
because of a confirmed live bug: `_state` used to default straight to
`PasscodeEntry`, which is indistinguishable from "you're signed out" —
so a device with a fully saved session still flashed the STUN
URL/passcode entry screen for however long `establishSignaling()` +
`loadRoster()`'s real network round trips took before snapping to
Dashboard. If you add a new top-level flow with its own "is there
saved state to resume" check, give it the same real `Loading` state
rather than defaulting to whatever screen is easiest to render first —
"looks right for a second, then jumps" is a real bug, not a cosmetic
one. (The media server dashboard has the same class of bug fixed the
same way, by a different mechanism — see `media-server-dashboard-ui`.)

## Shared loading indicator, and why its GIF has a solid background

`ui/components/SwarmLoadingIndicator` is the one loading spinner used
everywhere a loading state needs a visual (cold-start `Loading`,
browse-library loading, player buffering) — reuse it rather than a
bespoke spinner per screen, so "SWARM is thinking" always looks the
same.

Its backing assets are **flat solid-colored squares**, not transparent
ones. True single-color-key GIF transparency was attempted and
abandoned: hand-generating it via Pillow (matte color + shared-palette
quantization + an explicit `transparency=` index) did not reliably
preserve the key color at a consistent palette index across frames on
verification — some frames landed on a different index, or an RGBA
tuple a few values off from the matte, producing visible fringing.
Matching each GIF's flat background to whatever backdrop it actually
sits on is simpler and robust. Two variants exist for exactly this
reason: `res/drawable/loading.gif` (flat `SwarmBackground` navy — the
cold-start `Loading` state, `CatalogScreen`'s browse-loading state) and
`res/drawable/loading_black.gif` (flat black — `PlayerScreen`'s own
pure-black backdrop), selected via `SwarmLoadingIndicator`'s
`onBlackBackground: Boolean` param. Both were generated from the same
20-frame source animation — the black variant via a color-distance
threshold swap (any pixel within ~40 of the navy background RGB
becomes pure black, everything else untouched) rather than
regenerating the animation from scratch, so the mascot/pulse/timing
stay pixel-identical between variants. Adding a *third* backdrop color
somewhere new: reuse this same threshold-swap approach against
`loading.gif` (not `loading_black.gif`, to avoid compounding two lossy
requantization passes), and add a matching branch to
`SwarmLoadingIndicator` rather than a one-off `AsyncImage` at the new
call site.

## Layout: fit a real 1080p Fire TV screen, no scrolling for primary actions

Detail screens (`MovieDetailScreen`, etc.) are laid out to fit Play/
cast/actions on screen without scrolling — confirmed live as a real
bug: an earlier layout needed scrolling to reach Play, which is a bad
fit for a "press select to watch" D-pad flow. When adding fields to a
detail screen, prefer trimming/truncating (single-line cast list with
`TextOverflow.Ellipsis`, a fixed-height overview block with `maxLines`)
over letting the screen grow past one viewport.

A synopsis/description block spans the full screen width with a fixed
height (`Modifier.fillMaxWidth().height(100.dp)`, `maxLines` +
`TextOverflow.Ellipsis`), placed *below* the poster/info row rather
than squeezed into the narrow info column next to the poster — a
narrower column left it cramped to a few half-legible lines.

Full-bleed backdrop image behind everything (not a separate banner
stacked above the poster row) is this app's standing pattern for detail
screens, with a horizontal gradient scrim so poster/title/text stay
legible regardless of the backdrop art's own brightness — see
`MovieDetailScreen`'s `Brush.horizontalGradient` scrim for the exact
alpha stops to reuse.

## Catalog category tiles: no section label

`CatalogScreen`'s first-row category strip (`CategoryRow`) is only the
tiles themselves — do not put a "Categories" (or similar) `ShelfHeader`
above them. The outlined genre tiles already read as filters; the label
was removed in #352 as redundant chrome on Movies/Shows/Music.

## Centered wrapping picker: `FlowRow`, not a fixed-column grid

For a "pick one of many short options" overlay (e.g. filter-by-genre),
use `androidx.compose.foundation.layout.FlowRow`
(`@ExperimentalLayoutApi`) with `Arrangement.spacedBy(_, Alignment.CenterHorizontally)`,
wrapped in `Modifier.heightIn(max = ...).verticalScroll(rememberScrollState())`
since `FlowRow` itself isn't lazy/scrollable. This was a deliberate
redesign away from a fixed-column `LazyVerticalGrid` for this use case
— a grid with many short, ragged-length labels reads as uncentered and
visually unbalanced; a wrapping "tag cloud" reads correctly regardless
of how many options there are or how their lengths vary. Reach for
`LazyVerticalGrid` instead when the items are uniform-sized tiles
(artwork cards) where a strict column grid is the right shape, not for
short variable-length text chips.

## No on-screen Cancel/Back buttons in overlays

A remote's physical Back button, wired via `BackHandler(onBack = ...)`,
replaces any on-screen "Cancel"/"Back" button in a modal or overlay —
don't add one. It's redundant D-pad real estate the user has to
navigate past, and every overlay in this app already gets a working
physical Back for free via `BackHandler`.

## Playback controls and lifecycle

Conventional action buttons everywhere use
`ui/components/SwarmButtonColors.kt`'s `swarmActionButtonColors()`: white at
rest with dark navy text, cyan when D-pad focused, and hot accent while
pressed. Do not reproduce `ButtonDefaults.colors` at call sites. Selected
shuffle/like/tab state is communicated in the label or weight rather than by
breaking this palette. `SelectableChip` and number-pad keys are distinct
semantic controls and intentionally retain their own selected/keypad styling.

Media3 owns movie/show transport controls rather than Compose. Keep
`applySwarmPlaybackControlColors` in `PlayerScreen` aligned with the shared
sequence by applying a stateful white/cyan/hot tint to its native image
buttons.

Music has two distinct exits: physical Back minimizes the active session, and
the visible **Close** button stops it. Do not add a visible Minimize button;
Back already provides that action. The compact mini-player is aligned at the
bottom-right and its root measures only its visible controls. Never wrap it in
a full-screen focusable overlay, because that blocks D-pad access to the UI
behind it.

Player ownership is hoisted in `MainActivity` so recomposing or minimizing a
screen does not accidentally create another ExoPlayer or leave an abandoned
transcode alive. Every close, skip, cancellation, and runtime error path must
release the previous player before negotiating another stream.

## Keep the TV awake only during active playback

Use the activity window's `FLAG_KEEP_SCREEN_ON`, driven centrally from the
hoisted playback state. Enable it for any video player and for music only while
the player is actually playing, including while minimized. Clear it from
`DisposableEffect.onDispose`. Do not request a broad Android wake lock: the
window flag prevents the Fire TV screensaver while playback is active without
keeping the device awake after pause, close, or navigation.

## Side-loaded subtitles

Peer paths in `PlaybackPlan.subtitles` are not directly fetchable URLs.
`CatalogSession` must route each path through the same authenticated loopback
proxy used for media. Attach the resulting WebVTT URLs as Media3
`MediaItem.SubtitleConfiguration`s before preparing the item. Leave selection
and the Off option to `PlayerView`/Media3 rather than building a competing
custom subtitle picker.

## Launcher artwork needs a TV-safe zone

Fire TV applies its own icon mask and visual crop. A source mascot that fills
most of a 512px square will look oversized even though the PNG dimensions are
technically valid. Preserve the existing mascot artwork, scale it down, and
center it on a transparent 512x512 canvas. The current launcher's nontransparent
bounds are roughly 211x265px at `(149,117)`, which provides a tested safe zone.
The wide `tv_banner` is a separate resource and should remain independently
padded. After any change, assemble the APK and inspect the packaged icon rather
than judging only the source PNG.

## Client-error reporting: two related but distinct pipelines

Both funnel into the same server-side pipeline (peer QUIC
`/errors/report` → `client_errors` SQLite table → the media server
dashboard's "Client errors" panel, see `media-server-dashboard-ui`) via
`SwarmViewModel.reportClientError` (private), but have two different
public entry points for two different triggers:

- `reportPlaybackRuntimeError` — automatic, fired from
  `PlayerScreen.onPlaybackRuntimeError` when ExoPlayer itself throws
  after negotiation already succeeded (network drop mid-stream, a
  decoder error).
- `reportAssetProblem(entry)` — user-initiated, a "Report a problem"
  button on a detail screen, for the things that don't throw an
  exception but are still wrong (mislabeled title, wrong artwork, audio
  out of sync). Guard the button's own local `problemReported` state
  (`remember(entry) { mutableStateOf(false) }`) so a second press
  before navigating away can't spam duplicate reports — it resets
  naturally per fresh screen instance, no explicit clearing needed.

Add a third entry point the same way (a new private call into
`reportClientError` with a distinct `message`) rather than overloading
either existing function with a new meaning.
