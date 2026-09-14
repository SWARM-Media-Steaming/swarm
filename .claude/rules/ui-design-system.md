# UI Design System Rules

Everything under `ui/` (`index.html`, `style.css`, `app.js`) is one
continuously-developed product surface. Any change to it — a new setting, a
new panel, a new view — must read as if the same person built it, not as a
patchwork of one-off styles from different tasks. Follow the rules below
instead of inventing a parallel pattern.

This section is written to be portable: the tokens → components → layout →
binding structure applies to any small Tauri/Electron/vanilla-JS desktop
app, not just this one. To reuse it elsewhere, copy the section and swap the
token values, brand name, and component inventory for that app's own
`style.css`/`app.js` — keep the structure and the "extend, don't duplicate"
rule intact.

### Stack constraints

- Plain HTML/CSS/JS. No frontend framework, no bundler, no build step —
  `tauri.conf.json` points `frontendDist` straight at `ui/`.
- `window.__TAURI__` is available globally (`withGlobalTauri`); all
  backend calls go through `invoke`/`listen`, not fetch/XHR.
- Keep the three responsibilities in their own file: structure and
  data-attributes in `index.html`, all visual rules in `style.css`, all
  behavior in `app.js`. No inline `style="…"` and no inline `<script>`
  blocks.

### Design tokens

- Every color, and the light/dark mode itself, comes from the CSS custom
  properties declared once in `:root` (`--bg`, `--sidebar`, `--panel`,
  `--panel-2`, `--line`, `--line-soft`, `--text`, `--muted`, `--muted-2`,
  plus the accent set `--violet`/`--cyan`/`--green`/`--amber`/`--red`).
- Never hardcode a new hex/rgb color in a rule. If an existing token fits,
  use it; if none fits, add one token to `:root` and reference it — don't
  scatter a one-off literal through a component's CSS.
- The app is dark-only today (`color-scheme: dark`). Don't add a
  light-mode branch without extending the token set to support both.

### Layout skeleton

- Top-level shell is a two-column CSS grid: `.sidebar` (brand, `nav`,
  `.sidebar-foot`) and `main` (`.topbar` + view sections).
- The app is a single page with a view-switcher, not per-page navigation:
  each top-level area is a `<section id="view-<name>" class="view">`,
  toggled by `.view.active`, driven by `nav-item[data-view-target]`
  buttons and the `pageTitles` map in `app.js`. Adding a new top-level area
  means adding all three: the nav item, the view section, and the
  `pageTitles` entry — never a standalone route or separate HTML file.
- Every view opens with the same header shape: an `.eyebrow` (uppercase,
  small, violet, letter-spaced label) above an `<h2>`, inside
  `.section-intro`/`.section-heading`. Reuse that shape for any new view or
  section rather than inventing a new heading style.

### Component vocabulary — reuse, don't reinvent

Before writing new CSS, check whether one of these already expresses the
shape needed, and extend/compose it instead of writing a parallel one-off
class:

- `.panel` / `.service-card` — the bordered, rounded, gradient-background
  card used for every content block.
- `.status-pill` (`stopped`/`running`/`paused`/`error`) and `.suite-state` /
  `.requirement-item` variants — the shared color-coded state vocabulary.
  New states should map onto this same stopped/running/paused/error/blocked
  palette rather than introducing new status colors.
- Buttons: `.primary-button`, `.secondary-button`, `.danger-button`,
  `.icon-button`, `.text-button` (plus the `.compact` modifier) are the only
  button variants. A new action picks one of these, it doesn't get bespoke
  styling.
- `.toggle` — the custom switch used for every boolean setting.
- `.banner` (`warning`/`policy`), `.toast-stack`/`.toast`, `.activity-feed`,
  and the `.help-dot` + `#help-modal` pair are the established affordances
  for warnings, transient notices, live activity, and contextual help —
  reuse them instead of adding a new dialog or notice pattern.

### Declarative state binding

- Settings inputs don't get individual event handlers. They're marked with
  `data-config="<key>"` (app-level) or `data-repo-config="<key>"`
  (per-repository) and wired generically in `app.js` (see the
  `querySelectorAll('[data-config]')` / `'[data-repo-config]'` loops). A new
  setting should add the data attribute and let the existing generic
  read/write/dirty-tracking loop pick it up, not a new one-off listener.
- Help content follows the same registry pattern: add an entry to
  `HELP_TOPICS` keyed by the `data-help` id on a `.help-dot`, rather than
  writing a bespoke tooltip.

### Accessibility & interaction conventions

- Icon-only controls get `aria-label`. Live-updating regions (toasts,
  activity feed, log stream) get `aria-live="polite"`. The help modal is a
  `role="dialog" aria-modal="true"` overlay closed by its own close button —
  follow that shape for any new modal instead of a new overlay mechanism.
- Respect `prefers-reduced-motion` for any new animation, matching the
  existing toast-spinner handling.

### Responsive rules

- The two existing breakpoints (`1060px`, `820px`) collapse multi-column
  grids to fewer columns and hide low-value chrome (e.g. the repo switcher,
  the feedback-loop illustration). A new multi-column panel or grid must add
  its collapse rule at these same two breakpoints rather than introducing a
  new breakpoint.
