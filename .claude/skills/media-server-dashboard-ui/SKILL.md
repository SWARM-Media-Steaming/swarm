---
name: media-server-dashboard-ui
description: Use when building or changing anything in the Tauri media server's web dashboard (apps/server/ui — index.html, app.js, details.js, swarm.js, notifications.js, media.js, ai.js, style.css). Covers the no-framework vanilla-JS conventions, shared palette, tabs, script-load-order pitfalls, persistent progress panels, info popups, and toast/error behavior. For durable backend workers use media-server-background-work; for the Kotlin TV surface use tv-client-ui-conventions.
---

# Media server dashboard UI conventions

`apps/server/ui` is plain HTML/CSS/vanilla JS served into a Tauri
webview — no React/Vue/build step. Read this before adding or changing
anything here; several of these are silent failure modes that only show
up under real timing, not in a quick look at the code.

## `.d-none` vs. a class with its own `display:` — a specificity tie goes to source order, not to the classes actually present

Real bug, found live building the info modal (`.modal-backdrop`) and
independently confirmed already broken on `.badge-count` (the
Notifications tab badge): `.d-none { display: none; }` and a component
class that sets its own `display` (e.g. `display: flex`) are equal
specificity — `(0,1,0)` each, one class selector apiece. When two rules
tie on specificity, CSS resolves the conflict by **source order in the
stylesheet**, completely independent of which classes are actually on
the element or which was added last via `classList`. Any component class
declared *after* `.d-none` in `style.css` (line 59) silently wins its
own `display` even while `d-none` is present on the element — the
"hidden" state never actually renders as hidden, even though
`classList.contains("d-none")` correctly returns `true` and the JS logic
is otherwise completely correct. This is invisible from reading the JS
(state toggling looks right) and easy to misdiagnose as a JS bug when
"the modal won't close" — it's pure CSS cascade order.

Fix: for any class that both sets its own `display` *and* gets toggled
with `.d-none`, add an explicit `.that-class.d-none { display: none; }`
override — two class selectors beats one regardless of source order, so
it wins unconditionally. See `.modal-backdrop.d-none`, `.modal-link.d-none`,
and `.badge-count.d-none` in `style.css` for the pattern. When adding a
*new* class that sets `display` and will ever be paired with `.d-none`,
add this override at the same time — don't rely on declaring it before
`.d-none` in the file, since that's fragile to any later reordering.

## No framework: `invoke()`, real DOM, real `<script>` tags

Every backend call goes through `window.__TAURI__.core.invoke("cmd", {...})`
(aliased to `invoke` at the top of `app.js`) — a Tauri IPC command
defined on the Rust side, not a REST call. There's no component
framework: screens are built by setting `.innerHTML` on a container
element with a template string, then wiring `addEventListener` calls
onto the elements that string just created. Follow this same shape for
new panels rather than introducing a different pattern (a template
engine, a framework) for one feature.

Six script files load in a fixed order (`index.html`'s bottom):
`app.js` → `details.js` → `swarm.js` → `notifications.js` → `media.js`
→ `ai.js`. `app.js` owns boot/tabs/toast/shared helpers; the other five
each own one tab's content and are expected to define whatever function
`app.js`'s `showTab()` dispatches to for that tab (`refreshDetails`,
`refreshSwarm`, `refreshNotifications`, `refreshMedia`, `refreshAi`).
The `about` tab is the one exception — see "Tabs" below.

**Real bug, found live, twice**: calling any of those tab-refresh
functions (or anything else defined in a later-loading file) from code
that runs unconditionally at a file's own top level is a genuine race,
not a one-off — each classic `<script>` gets a microtask checkpoint
after it runs, and if an `invoke()` round trip resolves fast enough,
the continuation can call a function that hasn't been defined yet
(surfaces as e.g. `refreshNotificationBadge is not defined`, easy to
misdiagnose as a settings/persistence bug since it's caught by the same
top-level `try/catch` that also handles "onboarding not finished").
Fix: gate any such call behind `document.addEventListener("DOMContentLoaded", ...)`,
which only fires after every classic script in the document has
finished executing. Regression test: `apps/server/ui/test/boot_order.test.js`
— extend it if you add a new cross-file call at boot.

## Avoid initial-state-flash: reveal the body only once, deliberately

`index.html` has an inline `<style>body { visibility: hidden; }</style>`
in `<head>`, deliberately *not* moved into `style.css`. The three
top-level views (`onboardFolderView`/`onboardSwarmView`/`dashView`) all
start `class="d-none"` in the markup, but `.d-none` is defined in the
external `style.css`, which loads over its own separate request — on a
slow enough load, the raw un-hidden HTML could briefly paint before
`style.css` caught up, flashing the onboarding card even on an install
that was always headed straight to the dashboard. The inline style
parses as part of the very first HTML chunk, before any external file
round trip, so it can't race. `app.js`'s `show(id)` is the one place
that reveals `document.body.style.visibility = "visible"`, called only
once `boot()` has actually resolved which view to display. (The Fire TV
client has the same class of bug, fixed the same way conceptually via a
real `Loading` state — see `tv-client-ui-conventions`.)

If you add a new top-level view that can be chosen at boot, route its
reveal through the same `show()` call rather than unhiding it by hand —
that's the one chokepoint this guarantee depends on.

## `esc()`: mandatory on every interpolated value in a template string

`app.js`'s `esc(v)` HTML-escapes a value before it's spliced into an
`.innerHTML` template string. **Every** value that isn't a literal —
user input, a device name, an error message, anything from `invoke()`'s
response — must go through `esc()` at the point it's interpolated, no
exceptions, since this is the app's only XSS boundary (there's no
framework auto-escaping on your behalf here). Grep the file you're
editing for other `${esc(...)}` call sites and match that pattern
exactly; a bare `${value}` in a template string is a real vulnerability
here, not just a style nit.

## `showToast()`: every catch block reports here, never silently

`showToast(message, type, opts)` (`type`: `"success"` | `"warning"` |
`"error"`, default duration 4.5s / 7s for errors) is the single
notification surface for the whole app — no scattered inline status
text. Every `try { await invoke(...) } catch (err) { ... }` in this
codebase routes `err` to `showToast(String(err), "error")` in the catch
block; a new call site that swallows an error without a toast is a
regression, not a simplification. The one deliberate exception is a
best-effort background poll (`refreshNotificationBadge`'s `catch {}`)
where a transient failure every 30s isn't worth a toast — comment why
if you add another one of these, don't let a silent catch look
accidental.

## Shared palette: keep in sync with the Fire TV client

`style.css`'s `:root` variables (`--bg: #101828`, `--surface: #151f32`,
`--surface-muted: #1f2a44`, `--border: #31405f`, `--accent: #00c2ff`,
`--green: #34d399`, `--text: #ecf6ff`, `--muted: #9fb0c9`) are the
canonical source the Fire TV client's `SwarmTvTheme` was ported *from*
— same hex values on both sides deliberately, so the two apps read as
one product (see `tv-client-ui-conventions`). Reference these
`var(--...)` tokens, never a hand-picked hex literal; if you introduce
a genuinely new semantic color, add it here first and port it to the
Kotlin side too rather than letting one app silently drift off-palette.

## Structural building blocks — reuse before inventing new ones

- `.card` / `.card-head` (+ `.card-head-actions`) — the standard
  bordered content block with a title-row-plus-actions header. Almost
  every panel is one or more of these.
- `.grid` + `stat(label, value, mono, infoId)` (`app.js` helper) — the
  auto-fitting label/value tile grid used for status summaries. Pass
  `infoId` to make a tile open the info modal (below) — omit it (as
  `swarm.js`'s own `stat()` calls do) for a plain, non-interactive tile.
- The info modal (`app.js`'s `INFO_TOPICS`/`openInfoModal`, `#infoModalBackdrop`
  in `index.html`, `.modal-*` in `style.css`) — one shared "what am I
  looking at" popup (icon, title, one-paragraph explanation, optional
  external link) triggered by any element anywhere carrying
  `data-info="<topicId>"` matching a key in `INFO_TOPICS`, via a single
  delegated `document` click/keydown listener rather than a listener per
  element. Reach for this before building a bespoke tooltip/popover for a
  new "explain this concept" need — add a topic to `INFO_TOPICS` and a
  `data-info` attribute, don't invent a second popup mechanism. Keep
  `body` to one or two sentences (this exists specifically so the default
  view can stay concise); only set `link`/`linkLabel` when there's a real
  external resource worth reading further (a protocol, a standard, a
  third-party service) — most topics don't need one. Its `link` opens via
  `open_external_url` (below), not a bare `<a target="_blank">` — see
  that entry before wiring up any other external link in this UI.
- **External links must go through `open_external_url`, never a bare
  `<a target="_blank">`.** Real bug, found live: inside this app's Tauri
  webview (unlike a real browser tab), `target="_blank"` doesn't open the
  OS's default browser — it's silently swallowed, with no error and
  nothing in the console, so it's easy to ship and not notice until a
  real user clicks it. `app.js` keeps `href`/`target`/`rel` on the
  `<a>` for semantics (hover preview, right-click "copy link", screen
  readers) but intercepts the click, calls `preventDefault()`, and
  invokes `open_external_url` (`gui.rs`) instead — a plain app command
  wrapping `tauri-plugin-opener`'s `OpenerExt::open_url`, the officially
  supported way to hand a URL to the OS. It's a plain command (not
  invoked as `plugin:opener|open_url` straight from JS) specifically so
  no `capabilities/default.json` permission entry is needed — same
  reasoning `choose_media_folder` wraps the dialog plugin instead of
  exposing it to JS directly (see that file's own description). The info
  modal's `#infoModalLink` already does this; reuse it (or the same
  `open_external_url` invoke) for any new outbound link rather than a
  raw anchor tag.
- `.row` — a flex row of inputs/buttons for an inline form (used for
  "add a root", "join a swarm", etc.).
- `.muted`, `.mono`, `.d-none`, `button.secondary`, `button.danger` —
  standing utility classes; reach for these before writing new CSS.
- Icons are Bootstrap Icons (`<i class="bi bi-icon-name">`), always
  paired with the button/label text, never icon-only for a primary
  action.
- Tabs: `TABS` array in `app.js` (currently `media`, `details`, `swarm`,
  `notifications`, `ai`, `about`, in on-screen left-to-right order —
  `media` is deliberately first and is also the tab `enterDashboard()`
  opens on at boot),
  `tabPanel-<name>` / `tabBtn-<name>` id convention, `showTab(name)`
  toggles `.d-none`/`.tab-active` and dispatches to that tab's
  `refresh*()`. Adding a tab means adding to `TABS`, adding the matching
  `tabPanel-`/`tabBtn-` markup, and adding the
  `if (name === "...") refresh...()` dispatch line. `about` is the one
  tab with no dispatch line — its content is static, nothing to refresh.
- Sections *inside* a tab use the `.pill-tabs`/`.pill-tab` row (same
  component as the Reorganize media `.reorg-category-tab` filter), not a
  long vertical stack of panels. The AI tab is the reference (issue #494):
  `#aiSectionTabs` buttons carry `data-ai-section-target="<name>"`, panels
  carry `data-ai-section="<name>"`, and `showAiSection(name)` in `ai.js`
  shows one at a time. A list that can grow with the library (scan-assist
  findings, reorganize items) is a fixed-height table with a sticky header
  that scrolls inside itself (`.scrape-assist-table-wrap`,
  `.reorg-items-table-wrap`), filtered by a pill-tab row and a search box,
  never a card per item.
- A small red count bubble on a tab button (`.badge-count`, see
  `#notificationBadge` on the Notifications tab) is this app's pattern
  for "there's something here you haven't looked at yet" — paired with a
  `setInterval`-polled `refresh*Badge()` function (30s interval is the
  existing standard) rather than a push mechanism, since there's no
  websocket/event channel wired up for this yet.

## Long-running progress belongs outside re-rendered content

The Local subtitle generation panel is the reference pattern for a process
that can run for hours or days. Its static markup lives in `index.html` above
`#library`; `renderMediaTab()` may replace the entire library subtree without
destroying the progress node. Do not inject long-running status into a
container whose `.innerHTML` is rebuilt by search, filters, or navigation.

`media.js` polls `get_transcription_status` once per second and updates only
text, width, and active classes. Its catch is deliberately silent because a
background poll must not produce a toast every second during startup or a
transient IPC failure. User-initiated operations still toast every failure.
Represent disabled, download, verify, transcribe, playback-paused, idle, and
failed states in the same always-present panel. Combine durable checkpoint
counts with native in-section progress so the bar keeps moving during one
expensive checkpoint.

The panel is sticky within the Media card, not globally fixed: it remains
visible while a long library scrolls but cannot cover other tabs. Pair its
Details toggle with an `INFO_TOPICS` popup explaining download size, time/CPU
implications, pause/resume behavior, and local data handling; the success toast
must state that an automatic download is starting when applicable. See
`media-server-background-work` for the backend half.

## Adding a new report/feed-style panel: follow the "Client errors" shape

The Notifications tab's "Client errors" panel (`notifications.js`'s
`loadClientErrors`,
`style.css`'s `.client-error-row`/`.client-error-meta`/`.client-error-context`)
is the reference shape for "a list of server-recorded events a human
should triage and dismiss": a header with a live count and a
conditionally-shown "Clear all" (`.d-none`-toggled based on whether
there's anything to clear), each row showing a primary message plus a
`flex-wrap`ped meta line of small icon+text tags
(`<i class="bi bi-...">` + value), an optional secondary context block,
and a per-row delete button (`data-delete-*` attribute + a single
`querySelectorAll(...).forEach(addEventListener)` pass after the
`innerHTML` render, the standard way this codebase wires up
dynamically-created rows). Copy this shape for any new "list of things
from the backend that accumulate and get dismissed" panel rather than
designing a new list layout from scratch. This panel moved here from the
Swarm tab once its badge grew into its own "things to look at" surface
distinct from swarm membership/roster management — a similar future
panel might belong on this tab too rather than wherever its data
technically originates.

## Adding a "row of things" list: give each row a real delineated card

`.media-root-row` (Details tab, `details.js`'s `refreshMediaRoots`) is
the reference shape for a *simpler* list than "Client errors" above —
no per-row expand state, no meta-tag line, just "one thing plus a
delete/remove action" — but still needs its own bordered/backgrounded
row (padding, `border-radius`, `margin-bottom`) rather than bare
adjacent `.row` divs with nothing marking where one entry ends and the
next begins. Real feedback this fixed: a plain stack of `.row` divs (no
border, no background, no margin) read as "too close together, no
delineation" once there was more than one entry — the fix is the same
"give it a card" shape `.client-error-row` already uses, scaled down.

## One code box, several pairing paths tried in sequence

The Swarm tab's "Approve a TV" box (`approveTvCode`/`approveTvBtn` in
`index.html`, the click handler in `swarm.js`) is shared across every
device-pairing flow this app has, not one box per flow. Each flow is
tried in turn — fully local, no-network-round-trip checks first
(`approve_lan_pairing`, then `approve_http_media_pairing`), falling
through to the one flow that actually hits the STUN service
(`lookup_tv_activation`/`approve_tv_activation`) only if none of the
local ones claim the code. This works because every flow's codes share
one 8-digit format and a given code is only ever pending in one of
them, so trying each `invoke()` in sequence and catching-and-falling-through
on failure is safe — no need to ask the user which kind of device
they're approving. When a new pairing flow is added anywhere in this
app (device_a on the peer/cert protocol, device_b on plain HTTP, or
otherwise), add its `approve_*` call as another rung in this same
try/catch chain rather than a new input box — see the ordering comment
directly above the click handler in `swarm.js` for why local checks are
tried before the network one specifically (latency and cost, not
correctness — a network call working first would still behave
correctly, just slower for the common local case).
