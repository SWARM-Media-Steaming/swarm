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
    document.getElementById("mcpEnabledCheck").checked = settings.mcp_enabled;
    document.getElementById("mcpPortInput").value = settings.mcp_port;
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
      <label class="checkbox-label ai-provider-toggle"><input type="checkbox" class="ai-provider-enabled" ${p.enabled ? "checked" : ""}> ${esc(p.label)}</label>
      ${pill}
      <span class="ai-provider-hint muted">${hint}</span>
      <a class="ai-provider-docs" href="${esc(tool ? tool.docsUrl : "")}" target="_blank" rel="noopener noreferrer"><i class="bi bi-box-arrow-up-right"></i></a>
    </div>`;
    })
    .join("") +
    '<div class="ai-provider-actions"><button id="refreshAiToolsBtn" class="secondary"><i class="bi bi-arrow-repeat"></i>Refresh detection</button></div>';

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

async function refreshScanAssist(settings) {
  document.getElementById("aiScanAssistCheck").checked = settings.ai_scan_assist_enabled;
  const status = document.getElementById("aiScanAssistStatus");
  const hasProvider = settings.ai_providers.some(p => providerReady(settings, p.id));
  if (settings.ai_scan_assist_enabled && !hasProvider) {
    status.textContent = "Enabled, but no enabled AI tool is signed in with at least 10% usage remaining.";
    status.classList.add("error");
  } else {
    status.textContent = settings.ai_scan_assist_enabled ? "Enabled." : "Disabled.";
    status.classList.remove("error");
  }

  const wrap = document.getElementById("scrapeAssistWrap");
  if (!settings.ai_scan_assist_enabled) {
    wrap.classList.add("d-none");
    return;
  }
  let issues = [];
  try {
    issues = await invoke("list_scrape_issues");
  } catch (err) {
    showToast(String(err), "error");
  }
  wrap.classList.toggle("d-none", issues.length === 0);
  const list = document.getElementById("scrapeAssistList");
  list.innerHTML = issues
    .map(
      issue => `
    <li data-entry-key="${esc(issue.entry_key)}">
      <span class="issue-title">${esc(issue.title)}</span> — <span class="issue-reason">${esc(issue.reason)}</span>
      <button class="secondary ask-ai-btn" style="margin-left:8px; padding:2px 8px; font-size:.75rem"><i class="bi bi-stars"></i>Ask AI</button>
      <div class="ai-suggestion muted" style="margin-top:4px; font-size:.8rem"></div>
    </li>`
    )
    .join("");

  list.querySelectorAll(".ask-ai-btn").forEach(btn => {
    btn.addEventListener("click", async () => {
      const li = btn.closest("li");
      const entryKey = li.dataset.entryKey;
      const suggestionBox = li.querySelector(".ai-suggestion");
      btn.disabled = true;
      suggestionBox.textContent = "Asking AI…";
      const progressToast = showToast("Asking AI for a media match…", "progress", { duration: 0 });
      try {
        const suggestion = await invoke("ai_scrape_assist", { entryKey });
        suggestionBox.innerHTML = `Suggested: <strong>${esc(suggestion.tmdb_title)}</strong>${
          suggestion.suggested_year ? ` (${esc(String(suggestion.suggested_year))})` : ""
        } <button class="secondary apply-ai-suggestion-btn" style="padding:2px 8px; font-size:.75rem"><i class="bi bi-check-lg"></i>Apply</button>`;
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

document.getElementById("saveAiScanAssistBtn").addEventListener("click", async () => {
  try {
    await invoke("set_ai_scan_assist_enabled", { enabled: document.getElementById("aiScanAssistCheck").checked });
    showToast("Saved.", "success");
    await refreshAi();
  } catch (err) {
    showToast(String(err), "error");
  }
});

// ---- AI tab: reorganize media ------------------------------------------------
//
// A plan only ever proposes; nothing on disk changes until
// `approve_ai_reorg_plan` runs (never a delete, never an overwrite — see
// `reorganize.rs`). Plans live in memory only (`AppState::reorg_plans`), so
// they don't survive a restart — a fresh scan is cheap enough that this
// isn't worth persisting.

async function refreshReorganize(settings) {
  document.getElementById("aiReorganizeCheck").checked = settings.ai_reorganize_enabled;
  document.getElementById("aiReorganizeStatus").textContent = settings.ai_reorganize_enabled ? "Enabled." : "Disabled.";

  const scanWrap = document.getElementById("aiReorganizeScanWrap");
  scanWrap.classList.toggle("d-none", !settings.ai_reorganize_enabled);
  if (settings.ai_reorganize_enabled) {
    try {
      const roots = await invoke("list_media_roots");
      document.getElementById("aiReorganizeRootSelect").innerHTML = roots
        .map(r => `<option value="${esc(r.label)}">${esc(r.label)}</option>`)
        .join("");
    } catch (err) {
      showToast(String(err), "error");
    }
  }

  let plans = [];
  try {
    plans = await invoke("list_ai_reorg_plans");
  } catch (err) {
    showToast(String(err), "error");
  }
  renderReorgPlans(plans);
}

function renderReorgPlans(plans) {
  const wrap = document.getElementById("aiReorgPlansList");
  if (!plans || plans.length === 0) {
    wrap.innerHTML = "";
    return;
  }
  wrap.innerHTML = plans
    .slice()
    .reverse()
    .map(plan => {
      const itemsHtml =
        plan.items
          .map(
            item => `
        <li>
          <span class="mono">${esc(item.from)}</span> → <span class="mono">${esc(item.to)}</span>
          ${item.ai_assisted ? '<span class="muted" style="font-size:.72rem"> (AI-assisted)</span>' : ""}
          ${item.conflict ? `<br><span class="issue-reason">${esc(item.conflict)} — left in place</span>` : ""}
        </li>`
          )
          .join("") || '<li class="muted">Nothing to reorganize — this root already looks consistent.</li>';
      const summaryHtml = plan.apply_summary
        ? `<p class="muted">${plan.apply_summary.applied} moved, ${plan.apply_summary.skipped} skipped.${
            plan.apply_summary.errors.length ? `<br>${plan.apply_summary.errors.map(esc).join("<br>")}` : ""
          }</p>`
        : "";
      const actionsHtml =
        plan.status === "proposed"
          ? `<button class="secondary approve-reorg-btn" data-plan-id="${plan.id}"><i class="bi bi-check-lg"></i>Approve &amp; apply</button>
           <button class="secondary reject-reorg-btn" data-plan-id="${plan.id}"><i class="bi bi-x-lg"></i>Reject</button>`
          : "";
      return `
        <div class="card" style="margin-top:12px; background:var(--surface-muted)">
          <div class="row" style="justify-content:space-between; align-items:center">
            <strong>${esc(plan.root_label)}</strong>
            <span class="muted">${plan.items.length} item(s), ${plan.ai_assisted_count} AI-assisted, ${plan.conflict_count} conflict(s) — <em>${esc(plan.status)}</em></span>
          </div>
          <ul class="issue-list" style="margin-top:8px">${itemsHtml}</ul>
          ${summaryHtml}
          <div class="row" style="margin-top:8px">${actionsHtml}</div>
        </div>`;
    })
    .join("");

  wrap.querySelectorAll(".approve-reorg-btn").forEach(btn => {
    btn.addEventListener("click", async () => {
      const id = Number(btn.dataset.planId);
      btn.disabled = true;
      try {
        await invoke("approve_ai_reorg_plan", { id });
        showToast("Reorganization started in the background. You’ll be notified when it finishes.", "progress");
        await refreshAi();
      } catch (err) {
        showToast(String(err), "error");
        btn.disabled = false;
      }
    });
  });
  wrap.querySelectorAll(".reject-reorg-btn").forEach(btn => {
    btn.addEventListener("click", async () => {
      const id = Number(btn.dataset.planId);
      try {
        await invoke("reject_ai_reorg_plan", { id });
        await refreshAi();
      } catch (err) {
        showToast(String(err), "error");
      }
    });
  });
}

listen("ai-reorganize-finished", async ({ payload }) => {
  const hasErrors = payload.errors?.length > 0;
  const detail = payload.applied + " file(s) moved, " + payload.skipped + " skipped.";
  showToast(
    (hasErrors ? "Reorganization finished with issues: " : "Reorganization complete: ") + detail,
    hasErrors ? "warning" : "success",
    { duration: hasErrors ? 7000 : 4500 }
  );
  await Promise.all([refreshAi(), refreshLibrary(), refreshNotificationBadge()]);
});

document.getElementById("aiReorganizeScanBtn").addEventListener("click", async () => {
  const btn = document.getElementById("aiReorganizeScanBtn");
  const rootLabel = document.getElementById("aiReorganizeRootSelect").value;
  if (!rootLabel) {
    showToast("Add a media root first.", "error");
    return;
  }
  btn.disabled = true;
  const progressToast = showToast("Scanning the media root and preparing a reorganization plan…", "progress", { duration: 0 });
  try {
    await invoke("ai_reorganize_scan", { rootLabel });
    await refreshAi();
  } catch (err) {
    showToast(String(err), "error");
  } finally {
    btn.disabled = false;
    dismissToast(progressToast);
  }
});

document.getElementById("saveAiReorganizeBtn").addEventListener("click", async () => {
  try {
    await invoke("set_ai_reorganize_enabled", { enabled: document.getElementById("aiReorganizeCheck").checked });
    showToast("Saved.", "success");
    await refreshAi();
  } catch (err) {
    showToast(String(err), "error");
  }
});

function renderMcpStatus(settings) {
  const status = document.getElementById("mcpStatus");
  status.textContent = settings.mcp_enabled
    ? settings.mcp_access_token
      ? `Enabled on port ${settings.mcp_port} — restart SWARM after changing the server or token.`
      : "Access token required before the MCP Server can start."
    : "Disabled.";
  status.classList.toggle("error", settings.mcp_enabled && !settings.mcp_access_token);
}

function renderMcpConfigSnippet(settings) {
  const card = document.getElementById("mcpConfigCard");
  card.classList.toggle("d-none", !settings.mcp_enabled || !settings.mcp_access_token);
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

document.getElementById("saveMcpSettingsBtn").addEventListener("click", async () => {
  try {
    const enabled = document.getElementById("mcpEnabledCheck").checked;
    if (enabled && !document.getElementById("mcpAccessTokenInput").value) {
      showToast("Create an access token before enabling the MCP Server.", "error");
      return;
    }
    const portValue = document.getElementById("mcpPortInput").value.trim();
    const port = portValue ? Number(portValue) : 7890;
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      showToast("Port must be a whole number between 1 and 65535.", "error");
      return;
    }
    await invoke("set_mcp_enabled", { enabled });
    await invoke("set_mcp_port", { port });
    showToast("Saved. Restart the app for this to take effect.", "success");
    await refreshAi();
  } catch (err) {
    showToast(String(err), "error");
  }
});

document.getElementById("generateMcpTokenBtn").addEventListener("click", async () => {
  try {
    const token = await invoke("generate_mcp_access_token");
    document.getElementById("mcpAccessTokenInput").value = token;
    showToast("Access token created. Restart SWARM if the MCP Server is already enabled.", "success");
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
