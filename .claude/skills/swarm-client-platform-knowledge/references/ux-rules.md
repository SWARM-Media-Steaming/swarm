# UX rules that must hold on any TV platform

Generalized from Fire TV's `tv-client-ui-conventions` skill, which stays the authority for
Compose-specific implementation detail. This file is the platform-neutral version: what
must be true regardless of whether the platform is Compose for TV, SceneGraph, or
something future.

## Shared palette — one visual identity across every SWARM surface

These are shared with the STUN server's and media server dashboard's own `:root` CSS
variables. Porting a new client means porting these **hex-for-hex**, not "something close":

| Token | Hex | Role |
|---|---|---|
| Background | `#101828` | app background |
| Surface | `#151F32` | cards, panels |
| SurfaceMuted | `#1F2A44` | secondary surfaces, unfocused chip fill |
| Border | `#31405F` | default borders |
| Accent | `#00C2FF` | focus color, primary accent (cyan) |
| AccentHot | `#F5C451` | pressed color, "gold" accent |
| Error | `#FF5D7A` | errors, destructive actions |
| Like | `#FF4D67` | the like/heart badge specifically (distinct from Error) |
| Green | `#34D399` | success, "connected"/"online" |
| Text | `#ECF6FF` | primary text |
| Muted | `#9FB0C9` | secondary text |
| on-Accent foreground | `#04263A` | text/icon color on top of Accent or AccentHot fills |
| pressed foreground | `#241A00` | text/icon color on top of a pressed-state fill, where distinct from on-Accent |

If a client platform's design work needs a new semantic color, add the matching CSS
variable on the server dashboard side too (and vice versa) — don't let a client-only color
drift the shared identity apart. This is a [[swarm-cross-client-parity]] trigger, not just
a client concern.

## The one button interaction language

Every conventional action button uses the same three-state sequence, always: **white at
rest, cyan when focused, gold while pressed**, with the on-Accent foreground color for text
on both the focused and pressed states. Do not let a platform's default button styling
leak through unstyled — every interaction state must be set explicitly, not left to
inherit a platform default that was never designed against this palette. (Fire TV's own
skill documents a real, live bug from exactly this: a button that looked correct at rest
turned illegibly dark the moment focus landed on it, because only the resting/default color
pair had been set and the framework's own built-in focus color silently took over.)

Selected/toggled state (shuffle on, a selected filter chip) is communicated by the label
text or font weight, not by breaking this three-color sequence with a fourth ad-hoc color.

## Focus is a first-class design concern, not an afterthought

- **Every screen must land initial D-pad focus somewhere specific** the instant it appears
  — never leave focus on "whatever the platform's default focus search finds first," and
  never leave it unset (which strands the remote with no visible focus target at all).
  When a fresh instance of a screen/overlay appears, re-request focus explicitly; key this
  off of "is this genuinely a new instance," not off of every recomposition/redraw, or a
  routine state update can steal focus back from wherever the user just navigated to.
- **A modal/overlay drawn on top of other content must explicitly trap focus** — visually
  covering something is not the same as making it unreachable by D-pad. Disable focus on
  whatever's behind the overlay for as long as it's showing, and always pair this with the
  overlay handling the physical Back key itself (see below).
- **Two distinct card-focus idioms coexist by design**, not by accident: shelf rows /
  quick-access rows / season / episode cards deliberately **do not grow on focus** (only a
  very slight press-shrink) — focus there is signalled by container/border color instead.
  Full grids (Browse-All screens) and a few specific rows (pause-screen recommendations,
  server rows) **do** grow on focus. Reproduce this split rather than picking one style for
  everything; it was a deliberate, live-tuned decision (see `CatalogCard` vs.
  `MovieShelfScreen`'s grid cards in the Fire TV source for the concrete before/after).
- **Any "scroll a list then focus an item in it" sequence needs two real steps**, not one:
  scroll/programmatically position the list to the target index, wait for that layout pass
  to actually complete, *then* request focus. Requesting focus on an item that hasn't been
  laid out yet silently no-ops on every platform's UI framework, not just one.
- A search/text field must **never re-filter live as the user types** in a D-pad
  environment — apply the filter only on explicit submit (platform's equivalent of an IME
  "Done" action). Typing that triggers a recompose/relayout underneath a still-focused text
  field reads as the field mysteriously losing focus or "kicking you out."
- A text-entry field should **only open the on-screen keyboard on an explicit D-pad
  Center/Enter press while focused**, never merely from receiving focus as the user arrows
  past it — a keyboard that pops open every time focus passes over a field is a real,
  confirmed-live annoyance, not a hypothetical one.

## Back button semantics

The platform's physical Back key is the *only* way to close a modal/overlay/filter
panel — never add an on-screen "Cancel"/"Back" button next to it; it's redundant D-pad
real estate the user has to navigate around, and every overlay already gets a working
physical Back for free once it's wired up.

Two navigation surfaces get **more than one Back tier**, and both must be reproduced:
- The catalog browse page: Back first closes the search popup if it's open, then scrolls
  to the top and focuses the top bar (search icon / Movies / Shows / Music); only a
  *second* Back (with the top bar already focused) continues to the screen's real Back
  action. The top bar sits outside the scrolling page, so DOWN from it is explicit (scroll
  the target into composition, then focus the picked category's tile, else the first tile,
  else the first asset) rather than left to the platform's geometric focus search — which
  left the top bar reachable but impossible to leave downward.
- The video/music player: video's Back pauses first, then a second Back (from the paused
  overlay) actually exits; music's Back **minimizes** the player instead of stopping it —
  the visible on-screen Close/Stop control is the only way to actually end music playback.
  These are two intentionally different semantics for two intentionally different media
  types, not an inconsistency to "fix."

## Loading, empty, and error states

- A dedicated loading state/screen for **cold start and navigation-level loading** (browsing,
  connecting) is expected and should have personality (Fire TV rotates through a large list
  of themed loading messages) — but **mid-playback buffering must never replace the video
  frame with a full-screen loading treatment**. Keep the last rendered frame on screen and
  surface buffering through the same lightweight toast/notification mechanism used for
  other transient status, so the picture never visibly "goes away" just because the network
  blipped.
- Every list/grid needs an explicit, worded empty state ("No shows in the catalog yet.",
  "No matches for the current search/filter.") — never a bare blank area a user might read
  as "still loading" or "broken."
- Toast/notification conventions: three severities (success/warning/error) with
  increasing duration (roughly 4s/5s/7s), a small fixed cap on simultaneously visible
  toasts (drop oldest), and re-showing an identical message refreshes its timer instead of
  stacking a duplicate. Toasts are always non-focusable — they must never be reachable by
  or steal D-pad focus.
- Fail-open features (previews, prefetch, background sync, notification dismiss, like
  round-trip) never surface an error toast on failure. Fail-loud features (explicit user
  actions, connection state, active-playback failures) always do. See state-model.md.
- **A user pressing Connect always gets an answer.** "Silent on failure" is right for a
  background reconnect and wrong for a button someone just pressed: no feedback reads as a
  broken button. A failed user-initiated connect says so in plain words ("Couldn't reach X
  right now. Make sure it is turned on and on the same network as this TV.") without
  implying the pairing is at fault, and a connect that takes seconds shows "Connecting…".
- **A status word must mean what the control beside it needs.** "Connected" means there is
  a session and Browse can open it. "Found on the network" is a different state ("available")
  and must not share the word, or a list shows "connected" next to a greyed-out Browse. If a
  found, paired server is on screen, the primary action must be able to use it (Browse
  connects first, then opens the catalog) instead of dead-ending until the person finds the
  row's own Connect button.

## Layout for a 10-foot screen

- Reserve an action-safe margin around the edges of every non-video/non-player screen
  (roughly 5% per side is the Fire TV baseline) so nothing critical sits under a TV's
  overscan-prone edge. The video/player screen itself is the one deliberate exception —
  it's full-bleed.
- A detail screen (movie/show detail) must fit its primary action (Play) on screen
  **without scrolling** — a D-pad "press select to watch" flow that requires scrolling
  first to reach Play is a confirmed bad pattern. Trim/truncate secondary content
  (synopsis line clamps, cast list truncation) before you let a detail screen require
  scrolling to reach its main action.
- A full-bleed backdrop image with a horizontal gradient scrim (not a separate banner
  stacked above a poster) is the standard treatment for detail screens — it keeps
  poster/title/text legible regardless of how bright or busy the backdrop art itself is.
- For a "pick one of several short, variable-length options" control (genre filter chips,
  track/subtitle pickers), prefer a wrapping flow layout over a fixed-column grid — a rigid
  grid of short, ragged-length labels reads as visually unbalanced; a wrapping "tag cloud"
  reads correctly at any option count. Reserve strict grids for uniform-sized tiles
  (artwork cards) where a rigid grid is actually the right shape.

## Keep-awake, not a wake lock

Prevent the platform's screensaver/sleep only while genuinely relevant — any video
playback, and music playback specifically while actually playing (including while
minimized to a mini-player). Release it immediately on pause/stop/navigate-away. Do not
reach for a broad system wake-lock equivalent that would keep the whole device awake beyond
active playback — that's a meaningfully different (worse) power/behavior contract than what
this rule intends, even though the visible symptom of not having it is the same on first
glance.
