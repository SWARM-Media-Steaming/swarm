// ---- AI tab: MCP server enable/config ---------------------------------------
//
// The MCP server itself (apps/server/src/mcp.rs) only starts once, inside
// AppState::core, at the same time ServerCore does — see that file's doc
// comment. Saving a setting here takes effect on the *next* restart, not
// live; this tab is honest about that rather than implying an instant toggle.

// Cached CLI-detection results (from `detect_ai_tools`) so the gating checks
// in refreshScanAssist/refreshReorganize don't each re-shell three CLIs.
let aiToolsById = {};

async function refreshAi(showDetectionProgress = false) {
  try {
    const settings = await invoke("get_settings");
    const tokenInput = document.getElementById("mcpAccessTokenInput");
    tokenInput.value = settings.mcp_access_token || "";
    document.getElementById("generateMcpTokenBtn").innerHTML = settings.mcp_access_token
      ? '<i class="bi bi-arrow-repeat"></i>Regenerate token'
      : '<i class="bi bi-key-fill"></i>Create access token';
    document.getElementById("copyMcpTokenBtn").disabled = !settings.mcp_access_token;
    renderMcpStatus(settings);
    renderMcpConfigSnippet(settings);
    renderAiProviders(settings, []);
    let tools = [];
    const progressToast = showDetectionProgress
      ? showToast("Checking installed AI tools and available usage…", "progress", { duration: 0 })
      : null;
    try {
      tools = await invoke("detect_ai_tools");
    } catch (err) {
      showToast(String(err), "error");
    } finally {
      dismissToast(progressToast);
    }
    aiToolsById = Object.fromEntries(tools.map(t => [t.id, t]));
    renderAiProviders(settings, tools);
    await refreshScanAssist(settings);
    await refreshReorganize(settings);
  } catch (err) {
    showToast(String(err), "error");
  }
}

function providerReady(settings, id) {
  const provider = settings.ai_providers.find(p => p.id === id);
  const tool = aiToolsById[id];
  return Boolean(provider && provider.enabled && tool && tool.installed && tool.signedIn && tool.usageAvailable);
}

// ---- AI tab: "Enabled AI tools" (issue #252) ------------------------------
//
// One row per provider (Claude/Codex/Grok): a toggle plus a live detection
// pill for that provider's locally-installed CLI and its sign-in state —
// modelled on the SWARM Automation app's "Enabled AI tools" panel. No model
// box, no API key, no Save button: toggling persists immediately, and SWARM
// drives whichever CLI is signed in on this machine. `tools` comes from the
// `detect_ai_tools` command; `[]` on the first paint before it resolves.

function renderAiProviders(settings, tools) {
  const list = document.getElementById("aiProvidersList");
  const toolById = Object.fromEntries((tools || []).map(t => [t.id, t]));
  list.innerHTML = settings.ai_providers
    .map(p => {
      const tool = toolById[p.id];
      let pill = '<span class="ai-provider-pill ai-provider-pill-checking">Checking…</span>';
      let hint = "";
      if (tool) {
        if (!tool.installed) {
          pill = '<span class="ai-provider-pill ai-provider-pill-off">Not installed</span>';
          hint = `Install ${esc(tool.cliLabel)} to use it here.`;
        } else if (!tool.signedIn) {
          pill = '<span class="ai-provider-pill ai-provider-pill-warn">Sign-in required</span>';
          hint = `${esc(tool.cliLabel)} is installed — run its login command, then Refresh.`;
        } else if (!tool.usageAvailable) {
          pill = `<span class="ai-provider-pill ai-provider-pill-warn">${tool.usageRemainingPercent == null ? "Usage unavailable" : "Usage below 10%"}</span>`;
          hint = tool.usageRemainingPercent == null
            ? "Could not verify usage — AI calls are paused."
            : `${esc(tool.usageStatus)} — at least 10% is required.`;
        } else {
          pill = '<span class="ai-provider-pill ai-provider-pill-on">Signed in</span>';
          hint = `${esc(tool.usageStatus)}${tool.version ? ` · ${esc(tool.version)}` : ""}`;
        }
      }
      return `
    <div class="ai-provider-row" data-provider-id="${esc(p.id)}">
      <label class="toggle checkbox-label ai-provider-toggle"><input type="checkbox" class="ai-provider-enabled" ${p.enabled ? "checked" : ""}> ${esc(p.label)}</label>
      ${pill}
      <span class="ai-provider-hint muted">${hint}</span>
      <a class="ai-provider-docs" href="${esc(tool ? tool.docsUrl : "")}" target="_blank" rel="noopener noreferrer"><i class="bi bi-box-arrow-up-right"></i></a>
    </div>`;
    })
    .join("") +
    '<div class="ai-provider-actions"><button id="refreshAiToolsBtn" class="secondary-button"><i class="bi bi-arrow-repeat"></i>Refresh detection</button></div>';

  list.querySelectorAll(".ai-provider-enabled").forEach(input => {
    input.addEventListener("change", async () => {
      const id = input.closest(".ai-provider-row").dataset.providerId;
      const enabled = input.checked;
      const progressToast = showToast("Updating AI tool settings…", "progress", { duration: 0 });
      try {
        await invoke("set_ai_provider_enabled", { id, enabled });
        await refreshAi();
      } catch (err) {
        input.checked = !enabled;
        showToast(String(err), "error");
      } finally {
        dismissToast(progressToast);
      }
    });
  });

  list.querySelectorAll(".ai-provider-docs").forEach(link => {
    link.addEventListener("click", async event => {
      event.preventDefault();
      if (!link.getAttribute("href")) return;
      try {
        await invoke("open_external_url", { url: link.href });
      } catch (err) {
        showToast(String(err), "error");
      }
    });
  });

  const refreshBtn = document.getElementById("refreshAiToolsBtn");
  if (refreshBtn) refreshBtn.addEventListener("click", () => refreshAi(true));
}

// ---- AI tab: section tabs (issue #494) -----------------------------------------
//
// The AI page used to stack providers, scan assist, reorganize, and MCP in
// one long column. Each is now a section behind the #aiSectionTabs pill row:
// panels carry `data-ai-section="<name>"`, buttons carry
// `data-ai-section-target="<name>"`, and only the active section is shown.
let aiActiveSection = "providers";

function showAiSection(name) {
  aiActiveSection = name;
  document.querySelectorAll("#aiSectionTabs [data-ai-section-target]").forEach(btn => {
    const active = btn.dataset.aiSectionTarget === name;
    btn.classList.toggle("tab-active", active);
    btn.setAttribute("aria-selected", String(active));
  });
  document.querySelectorAll("#tabPanel-ai [data-ai-section]").forEach(panel => {
    panel.classList.toggle("d-none", panel.dataset.aiSection !== name);
  });
}

document.querySelectorAll("#aiSectionTabs [data-ai-section-target]").forEach(btn => {
  btn.addEventListener("click", () => showAiSection(btn.dataset.aiSectionTarget));
});

// ---- AI tab: scan & scrape assist -------------------------------------------
//
// Offers AI help only for entries the last `run_scrape` (or library
// maintenance) pass actually failed to match — see `list_scrape_issues` in
// gui.rs, backed by `AppState::last_scrape_issues` (in-memory, current
// session only).
//
// Issue #494: findings render as one compact, fixed-height table with a
// Type column, filtered by a type tab row and a search box, instead of one
// card per finding. "Ask AI for all" identifies every finding in batched AI
// requests (`ai_scrape_assist_all`), not one request per row. Each
// suggestion is confirmed on TMDb (movie and TV catalogs both, so a show
// filed under Movies still matches) and carries:
//   - a metadata match, applied through `apply_scrape_assist_suggestions`
//     (the same rescrape path as a manual TMDb URL fix), and
//   - a canonical rename (movie/show/season folders and file name), turned
//     into an ordinary reviewable reorganize plan by
//     `plan_scrape_assist_renames` — nothing is moved until that plan is
//     approved on the Reorganize media tab, and it can be undone there.
// Suggestions live server-side (keyed by entry key); this page only ever
// sends entry keys back, never paths or TMDb ids of its own. Nothing is
// applied without a click: "Auto-fix all" is the explicit opt-in to apply
// every same-type match in one go (renames are never automatic).

const SCRAPE_ASSIST_TYPES = [
  { kind: "all", label: "All" },
  { kind: "movie", label: "Movies" },
  { kind: "episode", label: "Shows" },
  { kind: "track", label: "Music" },
];
const SCRAPE_ASSIST_TYPE_LABELS = { movie: "Movie", episode: "TV episode", track: "Music track" };

let scrapeAssistIssues = [];
let scrapeAssistSuggestions = {}; // entry_key -> suggestion
let scrapeAssistErrors = {}; // entry_key -> last AI/TMDb error text
let scrapeAssistPending = new Set(); // entry keys with an AI request in flight
let scrapeAssistType = "all";
let scrapeAssistQuery = "";
let scrapeAssistBusy = false;
let scrapeAssistProgressToast = null;

function visibleScrapeAssistIssues() {
  const query = scrapeAssistQuery.trim().toLowerCase();
  return scrapeAssistIssues.filter(issue => {
    if (scrapeAssistType !== "all" && issue.kind !== scrapeAssistType) return false;
    if (!query) return true;
    const suggestion = scrapeAssistSuggestions[issue.entry_key];
    return [issue.title, issue.reason, suggestion?.current_path, suggestion?.tmdb_title, suggestion?.rename_to]
      .some(text => String(text || "").toLowerCase().includes(query));
  });
}

function renderScrapeAssistTypeTabs() {
  const wrap = document.getElementById("scrapeAssistTypeTabs");
  const tabs = SCRAPE_ASSIST_TYPES.filter(
    type => type.kind === "all" || scrapeAssistIssues.some(issue => issue.kind === type.kind)
  );
  if (!tabs.some(type => type.kind === scrapeAssistType)) scrapeAssistType = "all";
  wrap.innerHTML = tabs
    .map(type => {
      const count = type.kind === "all"
        ? scrapeAssistIssues.length
        : scrapeAssistIssues.filter(issue => issue.kind === type.kind).length;
      const active = type.kind === scrapeAssistType;
      return `<button type="button" class="pill-tab${active ? " tab-active" : ""}" data-kind="${esc(type.kind)}" role="tab" aria-selected="${active}">${esc(type.label)} <span class="muted">(${count})</span></button>`;
    })
    .join("");
  wrap.querySelectorAll(".pill-tab").forEach(btn => {
    btn.addEventListener("click", () => {
      scrapeAssistType = btn.dataset.kind;
      renderScrapeAssist();
    });
  });
}

function scrapeAssistSuggestionCell(issue) {
  if (scrapeAssistPending.has(issue.entry_key)) return '<span class="muted">Asking AI…</span>';
  const suggestion = scrapeAssistSuggestions[issue.entry_key];
  if (!suggestion) {
    const error = scrapeAssistErrors[issue.entry_key];
    return error ? `<span class="scrape-assist-warn">${esc(error)}</span>` : '<span class="muted">—</span>';
  }
  const year = suggestion.suggested_year ? ` (${esc(String(suggestion.suggested_year))})` : "";
  const kind = suggestion.media_type === "tv" ? "TV show" : "Movie";
  const mismatch = suggestion.kind_mismatch
    ? `<br><span class="scrape-assist-warn"><i class="bi bi-exclamation-triangle-fill"></i> Actually a ${kind.toLowerCase()} — rename to fix</span>`
    : "";
  return `<strong>${esc(suggestion.tmdb_title)}</strong>${year} <span class="muted">· ${kind}</span>${mismatch}`;
}

function scrapeAssistRenameCell(issue) {
  const suggestion = scrapeAssistSuggestions[issue.entry_key];
  if (!suggestion) return '<span class="muted">—</span>';
  if (!suggestion.rename_to) return `<span class="muted">${esc(suggestion.rename_note || "—")}</span>`;
  return `<span class="mono">${esc(suggestion.rename_root_label)}/${esc(suggestion.rename_to)}</span>`;
}

function renderScrapeAssist() {
  const wrap = document.getElementById("scrapeAssistWrap");
  const empty = document.getElementById("scrapeAssistEmpty");
  const tabCount = document.getElementById("aiScrapeTabCount");
  wrap.classList.toggle("d-none", scrapeAssistIssues.length === 0);
  empty.classList.toggle("d-none", scrapeAssistIssues.length > 0);
  tabCount.textContent = scrapeAssistIssues.length ? `(${scrapeAssistIssues.length})` : "";
  renderScrapeAssistTypeTabs();

  const rows = visibleScrapeAssistIssues();
  const tbody = document.getElementById("scrapeAssistRows");
  tbody.innerHTML = rows.length
    ? rows
        .map(issue => {
          const suggestion = scrapeAssistSuggestions[issue.entry_key];
          const pending = scrapeAssistPending.has(issue.entry_key) || scrapeAssistBusy;
          const isTrack = issue.kind === "track";
          const path = suggestion?.current_path;
          return `
      <tr data-entry-key="${esc(issue.entry_key)}">
        <td><span class="scrape-assist-type scrape-assist-type-${esc(issue.kind)}">${esc(SCRAPE_ASSIST_TYPE_LABELS[issue.kind] || issue.kind)}</span></td>
        <td><span class="scrape-assist-title">${esc(issue.title)}</span>${path ? `<span class="scrape-assist-path mono muted" title="${esc(path)}">${esc(path)}</span>` : ""}</td>
        <td class="scrape-assist-reason">${esc(issue.reason)}</td>
        <td>${scrapeAssistSuggestionCell(issue)}</td>
        <td class="scrape-assist-rename">${scrapeAssistRenameCell(issue)}</td>
        <td class="scrape-assist-actions">
          <button class="secondary-button compact scrape-assist-ask" ${pending || isTrack ? "disabled" : ""} title="${isTrack ? "AI assist only helps with movies and TV episodes" : "Ask AI about this file"}"><i class="bi bi-stars"></i>${suggestion ? "Re-ask" : "Ask AI"}</button>
          <button class="secondary-button compact scrape-assist-apply" ${suggestion?.can_apply_metadata && !pending ? "" : "disabled"} title="Apply the TMDb match"><i class="bi bi-check-lg"></i>Apply</button>
          <button class="secondary-button compact scrape-assist-rename-btn" ${suggestion?.rename_to && !pending ? "" : "disabled"} title="Review this rename on the Reorganize media tab"><i class="bi bi-pencil-square"></i>Rename</button>
        </td>
      </tr>`;
        })
        .join("")
    : `<tr><td colspan="6" class="muted">No findings match this filter.</td></tr>`;

  const withSuggestion = rows.filter(issue => scrapeAssistSuggestions[issue.entry_key]);
  const applicable = withSuggestion.filter(issue => scrapeAssistSuggestions[issue.entry_key].can_apply_metadata);
  const renamable = withSuggestion.filter(issue => scrapeAssistSuggestions[issue.entry_key].rename_to);
  const askable = rows.filter(issue => issue.kind !== "track");
  document.getElementById("askAiAllBtn").disabled = scrapeAssistBusy || askable.length === 0;
  document.getElementById("applyAllSuggestionsBtn").disabled = scrapeAssistBusy || applicable.length === 0;
  document.getElementById("planAllRenamesBtn").disabled = scrapeAssistBusy || renamable.length === 0;
  document.getElementById("runScrapeAssistNowBtn").disabled = scrapeAssistBusy || scrapeAssistIssues.length === 0;
  document.getElementById("scrapeAssistSummary").textContent =
    `Showing ${rows.length} of ${scrapeAssistIssues.length} finding(s) · ${withSuggestion.length} with an AI suggestion · ${applicable.length} ready to apply · ${renamable.length} rename(s) suggested`;
}

function storeScrapeAssistResults(results) {
  for (const result of results) {
    if (result.suggestion) {
      scrapeAssistSuggestions[result.entry_key] = result.suggestion;
      delete scrapeAssistErrors[result.entry_key];
    } else {
      delete scrapeAssistSuggestions[result.entry_key];
      scrapeAssistErrors[result.entry_key] = result.error || "AI could not identify this file.";
    }
  }
}

// Wraps a scan-assist action so only one runs at a time and every row's
// buttons reflect that while it does.
async function runScrapeAssistAction(message, action) {
  if (scrapeAssistBusy) return;
  scrapeAssistBusy = true;
  renderScrapeAssist();
  const progressToast = showToast(message, "progress", { duration: 0 });
  scrapeAssistProgressToast = progressToast;
  try {
    await action();
  } catch (err) {
    showToast(String(err), "error");
  } finally {
    scrapeAssistBusy = false;
    scrapeAssistProgressToast = null;
    dismissToast(progressToast);
    renderScrapeAssist();
  }
}

listen("ai-scrape-assist-progress", ({ payload }) => {
  const messageEl = scrapeAssistProgressToast?.querySelector(".toast-message");
  if (messageEl) messageEl.textContent = `Asking AI… ${payload.processed} of ${payload.total} file(s) identified`;
});

async function askAiForEntries(entryKeys) {
  const results = await invoke("ai_scrape_assist_all", { entryKeys });
  storeScrapeAssistResults(results);
  const found = results.filter(result => result.suggestion).length;
  showToast(`AI identified ${found} of ${results.length} file(s).`, found ? "success" : "warning");
}

async function applyScrapeAssistSuggestions(entryKeys) {
  const outcome = await invoke("apply_scrape_assist_suggestions", { entryKeys });
  const message = `Applied ${outcome.applied} match(es)${outcome.skipped ? `, ${outcome.skipped} skipped` : ""}.`;
  showToast(outcome.errors.length ? `${message} ${outcome.errors[0]}` : message, outcome.errors.length ? "warning" : "success");
  await refreshScanAssist();
  await refreshLibrary();
}

async function planScrapeAssistRenames(entryKeys) {
  const plans = await invoke("plan_scrape_assist_renames", { entryKeys });
  const moves = plans.reduce((total, plan) => total + plan.item_count, 0);
  showToast(`Rename plan ready: ${moves} file(s) to review on the Reorganize media tab.`, "success");
  const settings = await invoke("get_settings");
  await refreshReorganize(settings);
  showAiSection("reorganize");
}

document.getElementById("scrapeAssistRows").addEventListener("click", event => {
  const btn = event.target.closest("button");
  const row = btn?.closest("tr[data-entry-key]");
  if (!btn || !row || btn.disabled) return;
  const entryKey = row.dataset.entryKey;
  if (btn.classList.contains("scrape-assist-ask")) {
    scrapeAssistPending.add(entryKey);
    renderScrapeAssist();
    invoke("ai_scrape_assist", { entryKey })
      .then(suggestion => storeScrapeAssistResults([{ entry_key: entryKey, suggestion }]))
      .catch(err => {
        storeScrapeAssistResults([{ entry_key: entryKey, suggestion: null, error: String(err) }]);
        showToast(String(err), "error");
      })
      .finally(() => {
        scrapeAssistPending.delete(entryKey);
        renderScrapeAssist();
      });
  } else if (btn.classList.contains("scrape-assist-apply")) {
    runScrapeAssistAction("Applying the TMDb match…", () => applyScrapeAssistSuggestions([entryKey]));
  } else if (btn.classList.contains("scrape-assist-rename-btn")) {
    runScrapeAssistAction("Preparing the rename for review…", () => planScrapeAssistRenames([entryKey]));
  }
});

document.getElementById("askAiAllBtn").addEventListener("click", () => {
  const entryKeys = visibleScrapeAssistIssues()
    .filter(issue => issue.kind !== "track")
    .map(issue => issue.entry_key);
  runScrapeAssistAction(`Asking AI to identify ${entryKeys.length} file(s) at once…`, () => askAiForEntries(entryKeys));
});

document.getElementById("applyAllSuggestionsBtn").addEventListener("click", () => {
  const entryKeys = visibleScrapeAssistIssues()
    .filter(issue => scrapeAssistSuggestions[issue.entry_key]?.can_apply_metadata)
    .map(issue => issue.entry_key);
  runScrapeAssistAction(`Applying ${entryKeys.length} TMDb match(es)…`, () => applyScrapeAssistSuggestions(entryKeys));
});

document.getElementById("planAllRenamesBtn").addEventListener("click", () => {
  const entryKeys = visibleScrapeAssistIssues()
    .filter(issue => scrapeAssistSuggestions[issue.entry_key]?.rename_to)
    .map(issue => issue.entry_key);
  runScrapeAssistAction("Preparing renames for review…", () => planScrapeAssistRenames(entryKeys));
});

document.getElementById("scrapeAssistSearchInput").addEventListener("input", event => {
  scrapeAssistQuery = event.target.value;
  renderScrapeAssist();
});

async function refreshScanAssist(settings) {
  if (settings) {
    const status = document.getElementById("aiScanAssistStatus");
    const hasProvider = settings.ai_providers.some(p => providerReady(settings, p.id));
    status.textContent = "No enabled AI tool is signed in with at least 10% usage remaining.";
    status.classList.toggle("d-none", hasProvider);
  }

  try {
    scrapeAssistIssues = await invoke("list_scrape_issues");
    const suggestions = await invoke("list_scrape_assist_suggestions");
    const open = new Set(scrapeAssistIssues.map(issue => issue.entry_key));
    scrapeAssistSuggestions = Object.fromEntries(suggestions.map(s => [s.entry_key, s]));
    for (const key of Object.keys(scrapeAssistErrors)) {
      if (!open.has(key)) delete scrapeAssistErrors[key];
    }
  } catch (err) {
    showToast(String(err), "error");
  }
  renderScrapeAssist();
}

document.getElementById("runScrapeAssistNowBtn").addEventListener("click", () => {
  runScrapeAssistAction("Asking AI to resolve and apply every unmatched title…", async () => {
    const outcome = await invoke("run_scrape_assist_now");
    if (outcome.attempted === 0) {
      showToast("Nothing to check — run a library scan first.", "success");
    } else {
      showToast(`Resolved ${outcome.resolved} of ${outcome.attempted}. Anything left needs a rename or manual review.`, "success");
    }
    await refreshScanAssist();
    await refreshLibrary();
  });
});

// ---- AI tab: reorganize media ------------------------------------------------
//
// A plan only ever proposes; nothing on disk changes until
// `approve_ai_reorg_plan` runs (never a file delete or overwrite — see
// `reorganize.rs`). Successful moves are journaled so they can be safely
// undone during this session. Plans live in memory only
// (`AppState::reorg_plans`), so they don't survive a restart — a fresh scan
// is cheap enough that this isn't worth persisting. No enable toggle (issue
// #296): clicking "Scan for cleanup" is itself the permission to use AI for
// the filenames `classify` can't place on its own.
//
// Issue #312: each item in a proposed plan carries its own include/exclude
// checkbox (`reorgExcluded`, keyed by plan id, holding the excluded `from`
// paths) so a file can be left out of the approved run. Once a plan finishes
// applying or undoing, its id goes into `reorgConfirmedIds` so it drops out
// of the rendered list for good — the panel resets to the all-library scan
// control, with a plain-text confirmation of what happened
// shown briefly above it (in addition to the toast, which can be missed).
let reorgExcluded = {};
let reorgConfirmedIds = new Set();
let reorgConfirmTimer = null;
let reorgCleanupRoots = [];
// Issue #319: an approve/undo toast must stay open for the whole
// background apply/undo run (not just the default toast duration), so the
// in-progress toast for each plan id is kept here and dismissed by the
// matching "ai-reorganize-finished"/"ai-reorganize-undone" listener below.
let reorgActionToasts = {};
// A plan's item grid is paged 50 at a time via `list_reorg_plan_items`
// (offset/limit against that plan's full item list, which only ever lives
// in server memory — see `reorg_plans` in gui.rs) instead of the whole
// plan being shipped to and rendered into the DOM at once. The search box
// re-queries that same command with the search term, so a match is found
// across every item in the plan, not just whatever page happens to be
// loaded, and resets every visible plan back to its first page.
const REORG_PAGE_SIZE = 50;
let reorgSearchQuery = "";
let reorgSearchDebounce = null;
let reorgPageOffset = {}; // planId -> current item offset
let reorgPlanTotal = {}; // planId -> total items matching the current search
let reorgRequestSeq = {}; // planId -> latest items-fetch request number, to drop stale responses

// Plans are split into a tab per media type instead of stacking every
// root's card in one long column — a library with both a movies mess and a
// shows mess used to bury one plan under the other's (possibly huge) item
// table. Tabs are derived from `reorgCleanupRoots` (so a tab only appears
// for a media type that actually has a configured library), not from the
// plans themselves, so an empty tab still has somewhere to say "nothing to
// review" rather than vanishing.
const REORG_CATEGORIES = [
  { key: "movies", label: "Movies" },
  { key: "shows", label: "Shows" },
  { key: "music", label: "Music" },
];
let reorgLastPlans = []; // last fetched plan list, re-rendered on a tab switch without a refetch
let reorgActiveCategory = null;

// A plan's category comes from the asset type of the root it was scanned
// from, not the plan itself — `null` when that root is no longer configured
// (e.g. removed after the scan), in which case the plan is shown under
// every tab rather than silently hidden.
function reorgPlanCategory(plan) {
  return reorgCleanupRoots.find(root => root.label === plan.root_label)?.asset_type ?? null;
}

// Approve/undo's persistent "in progress" toast starts as plain text (the
// file-move phase has no progress signal) and grows a determinate bar once
// the post-move rescan starts reporting `ai-reorganize-progress` — same
// `.progress-bar`/`.progress-bar-fill` component the transcription and
// library-maintenance panels already use, just embedded in a toast instead
// of a fixed panel. The reorganize-scoped rescan is a small fraction of a
// whole-library scan, so the bar exists mainly to prove the wait is moving,
// not to promise a precise ETA (see the AI-tools discussion this answers).
function startReorgProgressToast(message) {
  const toast = showToast(message, "progress", { duration: 0 });
  const messageEl = toast?.querySelector(".toast-message");
  if (messageEl) {
    messageEl.innerHTML =
      `<span class="reorg-toast-text">${esc(message)}</span>` +
      `<div class="progress-bar reorg-toast-progress d-none"><div class="progress-bar-fill"></div></div>`;
  }
  return toast;
}

listen("ai-reorganize-progress", ({ payload }) => {
  const toast = reorgActionToasts[payload.id];
  const textEl = toast?.querySelector(".reorg-toast-text");
  const bar = toast?.querySelector(".reorg-toast-progress");
  const fill = bar?.querySelector(".progress-bar-fill");
  if (!textEl || !bar || !fill) return;
  bar.classList.remove("d-none");
  const progress = payload.progress;
  if (progress.phase === "discovering") {
    fill.style.width = "5%";
    textEl.textContent = `Scanning… ${progress.found} file(s) found so far`;
  } else {
    const ratio = progress.total ? progress.processed / progress.total : 1;
    fill.style.width = `${Math.round(ratio * 100)}%`;
    textEl.textContent = `Scanning ${progress.processed} of ${progress.total} file(s)…`;
  }
});

function showReorgConfirmation(message, hasErrors) {
  const el = document.getElementById("aiReorgConfirmMsg");
  if (!el) return;
  el.textContent = message;
  el.className = hasErrors ? "error" : "note";
  clearTimeout(reorgConfirmTimer);
  reorgConfirmTimer = setTimeout(() => {
    el.textContent = "";
    el.className = "note d-none";
  }, hasErrors ? 7000 : 5000);
}

async function refreshReorganize(settings) {
  try {
    const roots = await invoke("list_media_roots");
    reorgCleanupRoots = roots.filter(root => ["movies", "shows", "music"].includes(root.asset_type));
    const summary = document.getElementById("aiReorganizeRootsSummary");
    const scanButton = document.getElementById("aiReorganizeScanBtn");
    summary.textContent = reorgCleanupRoots.length
      ? `Scans all ${reorgCleanupRoots.length} configured Movies, Shows, and Music ${reorgCleanupRoots.length === 1 ? "library" : "libraries"}.`
      : "Add a Movies, Shows, or Music library to scan for cleanup.";
    scanButton.disabled = reorgCleanupRoots.length === 0;
  } catch (err) {
    reorgCleanupRoots = [];
    document.getElementById("aiReorganizeScanBtn").disabled = true;
    showToast(String(err), "error");
  }

  let plans = [];
  try {
    plans = await invoke("list_ai_reorg_plans");
  } catch (err) {
    showToast(String(err), "error");
  }
  renderReorgPlans(plans);
}

function renderReorgCategoryTabs(activePlans) {
  const tabsWrap = document.getElementById("aiReorgCategoryTabs");
  const presentCategories = REORG_CATEGORIES.filter(category =>
    reorgCleanupRoots.some(root => root.asset_type === category.key)
  );
  if (presentCategories.length === 0) {
    tabsWrap.classList.add("d-none");
    tabsWrap.innerHTML = "";
    return;
  }
  if (!presentCategories.some(category => category.key === reorgActiveCategory)) {
    reorgActiveCategory = presentCategories[0].key;
  }
  tabsWrap.classList.remove("d-none");
  tabsWrap.innerHTML = presentCategories
    .map(category => {
      const count = activePlans.filter(plan => reorgPlanCategory(plan) === category.key).length;
      const isActive = category.key === reorgActiveCategory;
      return `<button type="button" class="reorg-category-tab${isActive ? " tab-active" : ""}" data-category="${category.key}" role="tab" aria-selected="${isActive}">${esc(category.label)}${count ? ` <span class="muted">(${count})</span>` : ""}</button>`;
    })
    .join("");
  tabsWrap.querySelectorAll(".reorg-category-tab").forEach(btn => {
    btn.addEventListener("click", () => {
      if (btn.dataset.category === reorgActiveCategory) return;
      reorgActiveCategory = btn.dataset.category;
      renderReorgPlans(reorgLastPlans);
    });
  });
}

function renderReorgPlans(plans) {
  reorgLastPlans = plans || [];
  const wrap = document.getElementById("aiReorgPlansList");
  const activePlans = reorgLastPlans.filter(plan => !reorgConfirmedIds.has(plan.id));
  renderReorgCategoryTabs(activePlans);
  if (activePlans.length === 0) {
    wrap.innerHTML = "";
    document.getElementById("aiReorgSearchWrap")?.classList.add("d-none");
    return;
  }
  const visiblePlans = activePlans.filter(plan => {
    const category = reorgPlanCategory(plan);
    return category === null || category === reorgActiveCategory;
  });
  if (visiblePlans.length === 0) {
    const categoryLabel = REORG_CATEGORIES.find(c => c.key === reorgActiveCategory)?.label || reorgActiveCategory;
    wrap.innerHTML = `<p class="muted">Nothing to review under ${esc(categoryLabel)} right now.</p>`;
    document.getElementById("aiReorgSearchWrap")?.classList.add("d-none");
    return;
  }
  wrap.innerHTML = visiblePlans
    .slice()
    .reverse()
    .map(plan => {
      const statusDetails = {
        proposed: ["bi-clipboard-check", "Ready for review"],
        applying: ["bi-arrow-repeat", "Reorganization in progress…"],
        applied: ["bi-check-circle-fill", "Reorganization complete"],
        rejected: ["bi-x-circle", "Plan rejected"],
        undoing: ["bi-arrow-counterclockwise", "Undo in progress…"],
        undone: ["bi-check-circle", "Reorganization undone"]
      }[plan.status] || ["bi-info-circle", plan.status];
      const itemsHtml = plan.item_count
        ? `<div class="table-scroll reorg-items-table-wrap">
        <table class="reorg-items-table">
          <thead><tr><th>Include</th><th>From / To</th></tr></thead>
          <tbody id="reorg-items-tbody-${plan.id}"><tr><td colspan="2" class="muted">Loading…</td></tr></tbody>
        </table>
      </div>
      <div class="row reorg-pager">
        <span class="muted reorg-pager-status" id="reorg-pager-status-${plan.id}"></span>
        <button class="secondary-button compact reorg-page-prev" data-plan-id="${plan.id}" disabled><i class="bi bi-chevron-left"></i>Prev</button>
        <button class="secondary-button compact reorg-page-next" data-plan-id="${plan.id}" disabled>Next<i class="bi bi-chevron-right"></i></button>
      </div>`
        : '<p class="muted">Nothing to reorganize — this root already looks consistent.</p>';
      const misplacedHtml =
        plan.misplaced && plan.misplaced.length
          ? `
        <h3 class="review-heading misplaced-heading"><i class="bi bi-signpost-split-fill"></i> ${plan.misplaced.length} item(s) belong in a different library</h3>
        <ul class="issue-list misplaced-items">${plan.misplaced
          .map(
            item => `
        <li>
          <span class="mono">${esc(item.path)}</span>
          <br><span class="muted">Classified as ${esc(item.kind)} — included in the reviewed plan for “<strong>${esc(
              item.correct_root_label
            )}</strong>”</span>
        </li>`
          )
          .join("")}</ul>`
          : "";
      const summaryHtml = plan.apply_summary
        ? `<p class="muted">${plan.apply_summary.applied} moved, ${plan.apply_summary.skipped} skipped.${
            plan.apply_summary.errors.length ? `<br>${plan.apply_summary.errors.map(esc).join("<br>")}` : ""
          }</p>`
        : "";
      const undoSummaryHtml = plan.undo_summary
        ? `<p class="muted">Undo: ${plan.undo_summary.applied} restored, ${plan.undo_summary.skipped} skipped.${
            plan.undo_summary.errors.length ? `<br>${plan.undo_summary.errors.map(esc).join("<br>")}` : ""
          }</p>`
        : "";
      const actionsHtml =
        plan.status === "proposed"
          ? `<button class="secondary-button approve-reorg-btn" data-plan-id="${plan.id}"><i class="bi bi-check-lg"></i>Approve &amp; apply</button>
           <button class="secondary-button reject-reorg-btn" data-plan-id="${plan.id}"><i class="bi bi-x-lg"></i>Reject</button>`
          : plan.status === "applied" && plan.apply_summary?.applied
            ? `<button class="secondary-button undo-reorg-btn" data-plan-id="${plan.id}"><i class="bi bi-arrow-counterclockwise"></i>Undo</button>`
            : "";
      return `
        <div class="service-card ai-reorg-plan" data-plan-id="${plan.id}">
          <div class="reorg-status reorg-status-${esc(plan.status)}" role="status">
            <i class="bi ${statusDetails[0]}"></i>
            <strong>${esc(statusDetails[1])}</strong>
            ${plan.status === "applied" && plan.apply_summary ? `<span>${plan.apply_summary.applied} moved, ${plan.apply_summary.skipped} skipped</span>` : ""}
          </div>
          <div class="row plan-summary">
            <strong>${esc(plan.root_label)}</strong>
            <span class="muted">${plan.item_count} item(s), ${plan.ai_assisted_count} AI-assisted, ${plan.tmdb_year_count} TMDb-year, ${plan.orphan_count} orphaned, ${plan.duplicate_count} duplicate(s), ${plan.conflict_count} conflict(s)</span>
          </div>
          ${itemsHtml}
          ${misplacedHtml}
          ${summaryHtml}
          ${undoSummaryHtml}
          <div class="row plan-actions">${actionsHtml}</div>
        </div>`;
    })
    .join("");

  wrap.querySelectorAll(".reorg-page-prev").forEach(btn => {
    btn.addEventListener("click", () => {
      const id = Number(btn.dataset.planId);
      reorgPageOffset[id] = Math.max(0, (reorgPageOffset[id] || 0) - REORG_PAGE_SIZE);
      loadReorgPlanItems(id);
    });
  });
  wrap.querySelectorAll(".reorg-page-next").forEach(btn => {
    btn.addEventListener("click", () => {
      const id = Number(btn.dataset.planId);
      reorgPageOffset[id] = (reorgPageOffset[id] || 0) + REORG_PAGE_SIZE;
      loadReorgPlanItems(id);
    });
  });
  wrap.querySelectorAll(".approve-reorg-btn").forEach(btn => {
    btn.addEventListener("click", async () => {
      const id = Number(btn.dataset.planId);
      const excludedPaths = Array.from(reorgExcluded[id] || []);
      btn.disabled = true;
      // Issue #319: stays open until the "ai-reorganize-finished" event for
      // this id dismisses it — the background apply run can take far longer
      // than a toast's default duration.
      reorgActionToasts[id] = startReorgProgressToast("Reorganization started in the background. You’ll be notified when it finishes.");
      try {
        await invoke("approve_ai_reorg_plan", { id, excludedPaths });
        await refreshAi();
      } catch (err) {
        showToast(String(err), "error");
        btn.disabled = false;
        dismissToast(reorgActionToasts[id]);
        delete reorgActionToasts[id];
      }
    });
  });
  wrap.querySelectorAll(".reject-reorg-btn").forEach(btn => {
    btn.addEventListener("click", async () => {
      const id = Number(btn.dataset.planId);
      btn.disabled = true;
      const progressToast = showToast("Rejecting plan…", "progress", { duration: 0 });
      try {
        await invoke("reject_ai_reorg_plan", { id });
        delete reorgExcluded[id];
        delete reorgPageOffset[id];
        delete reorgPlanTotal[id];
        delete reorgRequestSeq[id];
        // Issue #319: once rejection completes, drop the plan from the
        // panel/table for good, same as an applied or undone plan.
        reorgConfirmedIds.add(id);
        showToast("Plan rejected.", "success");
        await refreshAi();
      } catch (err) {
        showToast(String(err), "error");
        btn.disabled = false;
      } finally {
        dismissToast(progressToast);
      }
    });
  });
  wrap.querySelectorAll(".undo-reorg-btn").forEach(btn => {
    btn.addEventListener("click", async () => {
      const id = Number(btn.dataset.planId);
      btn.disabled = true;
      reorgActionToasts[id] = startReorgProgressToast("Undo started in the background. You’ll be notified when it finishes.");
      try {
        await invoke("undo_ai_reorg_plan", { id });
        await refreshAi();
      } catch (err) {
        showToast(String(err), "error");
        btn.disabled = false;
        dismissToast(reorgActionToasts[id]);
        delete reorgActionToasts[id];
      }
    });
  });

  document.getElementById("aiReorgSearchWrap")?.classList.toggle("d-none", visiblePlans.length === 0);

  // Kick off (or continue, at whatever page was already open) the item
  // fetch for every visible plan that actually has items — the table body
  // itself starts as a "Loading…" placeholder above.
  for (const plan of visiblePlans) {
    if (plan.item_count) loadReorgPlanItems(plan.id);
  }
}

function renderReorgItemRow(planId, item) {
  const excludable = !item.conflict;
  const isExcluded = excludable && reorgExcluded[planId]?.has(item.from);
  const checkboxHtml = excludable
    ? `<label class="checkbox-label reorg-item-select"><input type="checkbox" class="reorg-item-toggle" data-from="${esc(item.from)}" ${isExcluded ? "" : "checked"}>Include</label>`
    : "";
  return `
    <tr class="reorg-item-row${isExcluded ? " reorg-item-excluded" : ""}">
      <td class="reorg-item-include">${checkboxHtml}</td>
      <td>
        <div class="reorg-item-paths mono">
          <div class="reorg-item-from"><span class="reorg-item-label muted">FROM</span> ${esc(item.from)}</div>
          <div class="reorg-item-to"><span class="reorg-item-label muted">TO</span> ${item.destination_root_label ? `<strong>${esc(item.destination_root_label)}:</strong> ` : ""}${esc(item.to)}</div>
        </div>
        ${item.kind === "orphan" ? '<span class="orphan-label"><i class="bi bi-exclamation-triangle"></i> Orphaned — no matching video found, moved out of the way</span>' : ""}
        ${item.kind === "duplicate" ? '<span class="duplicate-label"><i class="bi bi-files"></i> Duplicate of an already-organized file — moved aside, original left untouched</span>' : ""}
        ${item.ai_assisted ? '<span class="muted ai-assisted-label">AI-assisted</span>' : ""}
        ${item.year_source === "tmdb" ? '<span class="muted tmdb-year-label">Year via TMDb</span>' : ""}
        ${item.conflict ? `<span class="issue-reason">${esc(item.conflict)} — left in place</span>` : ""}
      </td>
    </tr>`;
}

// Fetches one 50-item page of `planId`'s items (filtered server-side by
// `reorgSearchQuery` against the plan's *entire* item list, not just what's
// currently loaded) and renders it into that plan's table body. `renderReorgPlans`
// rebuilds the whole plans list (and its pager buttons) on every refresh, so
// this never caches element references across the `await` — it re-queries
// by id/data-plan-id once the fetch resolves, so it always lands on
// whatever's actually live in the DOM rather than a detached element from
// before a rebuild. `reorgRequestSeq` drops a response that's been
// superseded by a newer request for the same plan (e.g. two quick Prev/Next
// clicks), so results can never apply out of order.
async function loadReorgPlanItems(planId) {
  const seq = (reorgRequestSeq[planId] = (reorgRequestSeq[planId] || 0) + 1);
  const startPrevBtn = document.querySelector(`.reorg-page-prev[data-plan-id="${planId}"]`);
  const startNextBtn = document.querySelector(`.reorg-page-next[data-plan-id="${planId}"]`);
  if (startPrevBtn) startPrevBtn.disabled = true;
  if (startNextBtn) startNextBtn.disabled = true;
  const offset = reorgPageOffset[planId] || 0;
  try {
    const page = await invoke("list_reorg_plan_items", {
      id: planId,
      offset,
      limit: REORG_PAGE_SIZE,
      search: reorgSearchQuery,
    });
    if (reorgRequestSeq[planId] !== seq) return; // superseded by a newer request
    reorgPlanTotal[planId] = page.total;
    const tbody = document.getElementById(`reorg-items-tbody-${planId}`);
    // The plan may have been approved/rejected/undone (and dropped from the
    // rendered list) while this fetch was in flight.
    if (!tbody) return;
    tbody.innerHTML = page.items.length
      ? page.items.map(item => renderReorgItemRow(planId, item)).join("")
      : `<tr><td colspan="2" class="muted">${reorgSearchQuery.trim() ? "No items match your search." : "Nothing on this page."}</td></tr>`;
    tbody.querySelectorAll(".reorg-item-toggle").forEach(cb => {
      cb.addEventListener("change", () => {
        const from = cb.dataset.from;
        if (!reorgExcluded[planId]) reorgExcluded[planId] = new Set();
        if (cb.checked) reorgExcluded[planId].delete(from);
        else reorgExcluded[planId].add(from);
        cb.closest(".reorg-item-row")?.classList.toggle("reorg-item-excluded", !cb.checked);
      });
    });
    const statusEl = document.getElementById(`reorg-pager-status-${planId}`);
    if (statusEl) {
      statusEl.textContent = page.total === 0 ? "" : `${offset + 1}–${offset + page.items.length} of ${page.total}`;
    }
    const prevBtn = document.querySelector(`.reorg-page-prev[data-plan-id="${planId}"]`);
    const nextBtn = document.querySelector(`.reorg-page-next[data-plan-id="${planId}"]`);
    if (prevBtn) prevBtn.disabled = offset <= 0;
    if (nextBtn) nextBtn.disabled = offset + REORG_PAGE_SIZE >= page.total;
  } catch (err) {
    if (reorgRequestSeq[planId] !== seq) return;
    const tbody = document.getElementById(`reorg-items-tbody-${planId}`);
    if (tbody) tbody.innerHTML = `<tr><td colspan="2" class="error">${esc(String(err))}</td></tr>`;
    showToast(String(err), "error");
  }
}

// Debounced so every keystroke doesn't fire its own backend query; once it
// settles, every currently-rendered plan resets to its first page and
// re-fetches against the new search term — search always runs over each
// plan's full item set, never just whatever page was on screen.
document.getElementById("aiReorgSearchInput")?.addEventListener("input", event => {
  reorgSearchQuery = event.target.value;
  clearTimeout(reorgSearchDebounce);
  reorgSearchDebounce = setTimeout(() => {
    document.querySelectorAll("#aiReorgPlansList .ai-reorg-plan").forEach(card => {
      const id = Number(card.dataset.planId);
      reorgPageOffset[id] = 0;
      loadReorgPlanItems(id);
    });
  }, 250);
});

listen("ai-reorganize-finished", async ({ payload }) => {
  dismissToast(reorgActionToasts[payload.id]);
  delete reorgActionToasts[payload.id];
  const hasErrors = payload.errors?.length > 0;
  const detail = payload.applied + " file(s) moved, " + payload.skipped + " skipped.";
  showToast(
    (hasErrors ? "Reorganization finished with issues: " : "Reorganization complete: ") + detail,
    hasErrors ? "warning" : "success",
    { duration: hasErrors ? 7000 : 4500 }
  );
  showReorgConfirmation(`“${payload.root_label}” reorganized — ${detail}`, hasErrors);
  reorgConfirmedIds.add(payload.id);
  delete reorgExcluded[payload.id];
  delete reorgPageOffset[payload.id];
  delete reorgPlanTotal[payload.id];
  delete reorgRequestSeq[payload.id];
  await Promise.all([refreshAi(), refreshLibrary(), refreshNotificationBadge()]);
});

listen("ai-reorganize-undone", async ({ payload }) => {
  dismissToast(reorgActionToasts[payload.id]);
  delete reorgActionToasts[payload.id];
  const hasErrors = payload.errors?.length > 0;
  const detail = payload.applied + " file(s) restored, " + payload.skipped + " skipped.";
  showToast(
    (hasErrors ? "Undo finished with issues: " : "Reorganization undone: ") + detail,
    hasErrors ? "warning" : "success",
    { duration: hasErrors ? 7000 : 4500 }
  );
  showReorgConfirmation(`“${payload.root_label}” undo — ${detail}`, hasErrors);
  reorgConfirmedIds.add(payload.id);
  delete reorgExcluded[payload.id];
  delete reorgPageOffset[payload.id];
  delete reorgPlanTotal[payload.id];
  delete reorgRequestSeq[payload.id];
  await Promise.all([refreshAi(), refreshLibrary(), refreshNotificationBadge()]);
});

document.getElementById("aiReorganizeScanBtn").addEventListener("click", async () => {
  const btn = document.getElementById("aiReorganizeScanBtn");
  const status = document.getElementById("aiReorgScanStatus");
  let roots;
  try {
    roots = (await invoke("list_media_roots")).filter(root => ["movies", "shows", "music"].includes(root.asset_type));
  } catch (err) {
    showToast(String(err), "error");
    return;
  }
  if (!roots.length) {
    showToast("Add a Movies, Shows, or Music library first.", "error");
    return;
  }
  btn.disabled = true;
  const progressToast = showToast(`Scanning all ${roots.length} media libraries for cleanup…`, "progress", { duration: 0 });
  const failures = [];
  let completed = 0;
  try {
    status.className = "note";
    for (const root of roots) {
      const progress = `Scanning ${completed + 1} of ${roots.length}: ${root.label}…`;
      status.textContent = progress;
      progressToast?.querySelector(".toast-message")?.replaceChildren(progress);
      try {
        await invoke("ai_reorganize_scan", { rootLabel: root.label });
      } catch (err) {
        failures.push(`${root.label}: ${String(err)}`);
      }
      completed += 1;
    }
    await refreshAi();
    if (failures.length) {
      status.textContent = `Scan finished: ${completed - failures.length} of ${roots.length} libraries produced review plans. ${failures.join(" | ")}`;
      status.className = "note error";
      showToast(`Cleanup scan finished with ${failures.length} ${failures.length === 1 ? "error" : "errors"}.`, "warning");
    } else {
      status.textContent = `Scan complete: review the ${roots.length} library plans below.`;
      status.className = "note";
      showToast(`Cleanup scan complete for all ${roots.length} libraries.`, "success");
    }
  } finally {
    btn.disabled = false;
    dismissToast(progressToast);
  }
});

function renderMcpStatus(settings) {
  const status = document.getElementById("mcpStatus");
  status.textContent = settings.mcp_access_token
    ? `Enabled on port ${settings.mcp_port} — restart SWARM after creating a new token.`
    : "Create an access token to enable the MCP Server.";
  status.classList.toggle("error", !settings.mcp_access_token);
}

function renderMcpConfigSnippet(settings) {
  const card = document.getElementById("mcpConfigCard");
  card.classList.toggle("d-none", !settings.mcp_access_token);
  const snippet = {
    mcpServers: {
      swarm: {
        type: "streamableHttp",
        url: `http://<this-machine's-LAN-IP>:${settings.mcp_port}/mcp`,
        headers: {
          Authorization: `Bearer ${settings.mcp_access_token || "<access-token>"}`,
        },
      },
    },
  };
  document.getElementById("mcpConfigSnippet").textContent =
    JSON.stringify(snippet, null, 2) +
    "\n\n// Replace <this-machine's-LAN-IP> with this computer's network address\n// (check your OS's network settings — \"localhost\" only works if the\n// MCP client runs on this same machine).";
}

document.getElementById("generateMcpTokenBtn").addEventListener("click", async () => {
  try {
    const token = await invoke("generate_mcp_access_token");
    document.getElementById("mcpAccessTokenInput").value = token;
    showToast("Access token created. Restart SWARM to enable the MCP Server with it.", "success");
    await refreshAi();
  } catch (err) {
    showToast(String(err), "error");
  }
});

document.getElementById("copyMcpTokenBtn").addEventListener("click", async () => {
  const token = document.getElementById("mcpAccessTokenInput").value;
  if (!token) return;
  try {
    await navigator.clipboard.writeText(token);
    showToast("Access token copied.", "success");
  } catch (err) {
    showToast(`Could not copy the token: ${err}`, "error");
  }
});
