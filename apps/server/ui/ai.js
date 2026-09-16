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

// ---- AI tab: scan & scrape assist -------------------------------------------
//
// Offers AI help only for entries the last `run_scrape` (or library
// maintenance) pass actually failed to match — see `list_scrape_issues` in
// gui.rs, backed by `AppState::last_scrape_issues` (in-memory, current
// session only). Applying a suggestion reuses the existing `rescrape_entry`
// command with the AI-confirmed TMDb id, exactly like a manual "fix match"
// would — this feature only ever proposes, the user always clicks Apply.
//
// No enable toggle (issue #296): this is always on, gated only by an
// enabled+ready AI provider (see the "Enabled AI tools" panel above) and
// the "Ask AI"/"Check now" click itself. Issues render as a grid grouped by
// `issue.kind` (movie/episode/track) instead of one jumbled list.

const SCRAPE_ASSIST_GROUPS = [
  { kind: "movie", label: "Movies" },
  { kind: "episode", label: "Shows" },
  { kind: "track", label: "Music" },
];

function renderScrapeAssistIssues(issues) {
  const wrap = document.getElementById("scrapeAssistWrap");
  wrap.classList.toggle("d-none", issues.length === 0);
  const groups = document.getElementById("scrapeAssistGroups");
  groups.innerHTML = SCRAPE_ASSIST_GROUPS
    .map(group => {
      const groupIssues = issues.filter(issue => issue.kind === group.kind);
      if (groupIssues.length === 0) return "";
      const cards = groupIssues
        .map(
          issue => `
      <div class="scrape-assist-card service-card" data-entry-key="${esc(issue.entry_key)}">
        <div class="scrape-assist-card-title">${esc(issue.title)}</div>
        <div class="scrape-assist-card-reason">${esc(issue.reason)}</div>
        <button class="secondary-button compact ask-ai-btn"><i class="bi bi-stars"></i>Ask AI</button>
        <div class="ai-suggestion muted"></div>
      </div>`
        )
        .join("");
      return `
    <div class="scrape-assist-group">
      <h3 class="review-heading">${esc(group.label)} <span class="muted">(${groupIssues.length})</span></h3>
      <div class="scrape-assist-grid">${cards}</div>
    </div>`;
    })
    .join("");

  groups.querySelectorAll(".ask-ai-btn").forEach(btn => {
    btn.addEventListener("click", async () => {
      const card = btn.closest(".scrape-assist-card");
      const entryKey = card.dataset.entryKey;
      const suggestionBox = card.querySelector(".ai-suggestion");
      btn.disabled = true;
      suggestionBox.textContent = "Asking AI…";
      const progressToast = showToast("Asking AI for a media match…", "progress", { duration: 0 });
      try {
        const suggestion = await invoke("ai_scrape_assist", { entryKey });
        suggestionBox.innerHTML = `Suggested: <strong>${esc(suggestion.tmdb_title)}</strong>${
          suggestion.suggested_year ? ` (${esc(String(suggestion.suggested_year))})` : ""
        } <button class="secondary-button compact apply-ai-suggestion-btn"><i class="bi bi-check-lg"></i>Apply</button>`;
        suggestionBox.querySelector(".apply-ai-suggestion-btn").addEventListener("click", async () => {
          try {
            await invoke("rescrape_entry", { entryKey, tmdbUrl: suggestion.tmdb_url });
            showToast("Applied.", "success");
            await refreshAi();
            await refreshLibrary();
          } catch (err) {
            showToast(String(err), "error");
          }
        });
      } catch (err) {
        suggestionBox.textContent = String(err);
        showToast(String(err), "error");
      } finally {
        btn.disabled = false;
        dismissToast(progressToast);
      }
    });
  });
}

async function refreshScanAssist(settings) {
  const status = document.getElementById("aiScanAssistStatus");
  const hasProvider = settings.ai_providers.some(p => providerReady(settings, p.id));
  status.textContent = "No enabled AI tool is signed in with at least 10% usage remaining.";
  status.classList.toggle("d-none", hasProvider);

  let issues = [];
  try {
    issues = await invoke("list_scrape_issues");
  } catch (err) {
    showToast(String(err), "error");
  }
  renderScrapeAssistIssues(issues);
}

document.getElementById("runScrapeAssistNowBtn").addEventListener("click", async () => {
  const btn = document.getElementById("runScrapeAssistNowBtn");
  btn.disabled = true;
  const progressToast = showToast("Asking AI to resolve unmatched titles…", "progress", { duration: 0 });
  try {
    const outcome = await invoke("run_scrape_assist_now");
    if (outcome.attempted === 0) {
      showToast("Nothing to check — run a library scan first.", "success");
    } else {
      showToast(`Resolved ${outcome.resolved} of ${outcome.attempted}.`, "success");
    }
    await refreshAi();
    await refreshLibrary();
  } catch (err) {
    showToast(String(err), "error");
  } finally {
    btn.disabled = false;
    dismissToast(progressToast);
  }
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
      reorgActionToasts[id] = showToast("Reorganization started in the background. You’ll be notified when it finishes.", "progress", { duration: 0 });
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
      reorgActionToasts[id] = showToast("Undo started in the background. You’ll be notified when it finishes.", "progress", { duration: 0 });
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
