// ---- Swarm tab: STUN link, membership, per-swarm device roster -------------
//
// Join-code *generation* is intentionally not here: the STUN server only
// lets the swarm's owning user (a session-cookie browser login) mint codes
// (`POST /swarms/{id}/codes` has no Bearer path), and this device only ever
// holds a device Bearer token — it was never designed to also hold a user
// login. This tab only ever consumes a code (LAN pairing or SWARM TV
// activation), never mints one.

// Client-reported errors used to have a panel on this tab (they arrive over
// the same authenticated peer connection swarm membership uses) — moved to
// their own Notifications tab (notifications.js) once the badge count grew
// into its own "things to look at" surface distinct from swarm
// membership/roster management.

function formatFingerprint(fingerprint) {
  return `${fingerprint.slice(0, 12)}…${fingerprint.slice(-8)}`;
}

async function loadLocalPeers() {
  const list = document.getElementById("localPeersList");
  try {
    const peers = await invoke("list_local_peers");
    list.innerHTML = peers.length ? `<table>
      <thead><tr><th>Name</th><th>Paired</th><th class="info-trigger" data-info="device-fingerprint" tabindex="0" role="button">Certificate <i class="bi bi-info-circle info-affordance"></i></th><th></th></tr></thead>
      <tbody>${peers.map(peer => `<tr>
        <td>${esc(peer.name)}</td>
        <td>${esc(new Date(peer.paired_at * 1000).toLocaleString())}</td>
        <td class="mono" title="${esc(peer.fingerprint)}">${esc(formatFingerprint(peer.fingerprint))}</td>
        <td><button class="danger-button compact" data-revoke-local="${esc(peer.fingerprint)}"><i class="bi bi-x-lg"></i>Revoke</button></td>
      </tr>`).join("")}</tbody>
    </table>` : `<p class="muted">No LAN clients have been paired yet.</p>`;
    list.querySelectorAll("[data-revoke-local]").forEach(btn => {
      btn.addEventListener("click", async () => {
        try {
          await invoke("revoke_local_peer", { fingerprint: btn.dataset.revokeLocal });
          showToast("LAN client revoked.", "success");
          await loadLocalPeers();
        } catch (err) {
          showToast(String(err), "error");
        }
      });
    });
  } catch (err) {
    list.innerHTML = `<p class="muted">Unable to load paired LAN clients.</p>`;
    showToast(String(err), "error");
  }
}

// Approve TV is a single code box for every pairing path: LAN pairing and
// plain-HTTP pairing are both fast, fully local checks (no network round
// trip), so they're tried first and only fall through to the SWARM
// activation lookup (which hits the STUN service) if the code isn't a
// pending local request. A given 8-digit code is only ever valid for one of
// the three, so trying them in sequence is safe.
document.getElementById("approveTvBtn").addEventListener("click", async () => {
  const input = document.getElementById("approveTvCode");
  const code = input.value.replace(/\D/g, "");
  const status = document.getElementById("approveTvStatus");
  if (code.length !== 8) {
    showToast("Enter the 8-digit code shown on the TV.", "error");
    return;
  }
  try {
    const pairing = await invoke("approve_lan_pairing", { code });
    input.value = "";
    showToast(`${pairing.name} was approved. The TV will connect automatically.`, "success");
    await loadLocalPeers();
    return;
  } catch (_lanErr) {
    // Not a pending LAN code -- fall through and try plain-HTTP pairing.
  }
  try {
    const deviceName = await invoke("approve_http_media_pairing", { code });
    input.value = "";
    showToast(`${deviceName} was approved. It will connect automatically.`, "success");
    await loadHttpMediaDevices();
    return;
  } catch (_httpErr) {
    // Not a pending plain-HTTP code either -- fall through and try a SWARM activation.
  }
  try {
    const pending = await invoke("lookup_tv_activation", { code });
    status.classList.remove("d-none");
    status.innerHTML = `<div class="note">
      <strong>${esc(pending.device_name)}</strong><br>
      <span class="muted">${esc(pending.platform)} · expires ${esc(new Date(pending.expires_at).toLocaleTimeString())}</span>
      <button id="confirmTvBtn" class="primary-button confirm-tv-button"><i class="bi bi-check-lg"></i>Approve this TV</button>
    </div>`;
    document.getElementById("confirmTvBtn").addEventListener("click", async () => {
      try {
        await invoke("approve_tv_activation", { activationId: pending.activation_id });
        showToast(`${pending.device_name} was added to your swarm.`, "success");
        input.value = "";
        status.classList.add("d-none");
        await refreshSwarm();
      } catch (err) {
        showToast(String(err), "error");
      }
    });
  } catch (_swarmErr) {
    showToast("Invalid or expired code.", "error");
  }
});

// Same shape as loadLocalPeers() above, for devices that pair over plain
// HTTP instead of the peer/LAN protocol (http_media.rs) — a separate list
// since they're a separate credential (a bearer token, not a cert
// fingerprint) with no "online" status to show. token_hash is a plain
// SHA-256 hex string, the same length/shape as a cert fingerprint, so
// formatFingerprint()'s truncation applies unchanged.
async function loadHttpMediaDevices() {
  const list = document.getElementById("httpMediaDevicesList");
  try {
    const devices = await invoke("list_http_media_devices");
    list.innerHTML = devices.length ? `<table>
      <thead><tr><th>Name</th><th>Paired</th><th>Token</th><th></th></tr></thead>
      <tbody>${devices.map(device => `<tr>
        <td>${esc(device.name)}</td>
        <td>${esc(new Date(device.paired_at * 1000).toLocaleString())}</td>
        <td class="mono" title="${esc(device.token_hash)}">${esc(formatFingerprint(device.token_hash))}</td>
        <td><button class="danger-button compact" data-revoke-http-media="${esc(device.token_hash)}"><i class="bi bi-x-lg"></i>Revoke</button></td>
      </tr>`).join("")}</tbody>
    </table>` : `<p class="muted">No plain-HTTP devices have been paired yet.</p>`;
    list.querySelectorAll("[data-revoke-http-media]").forEach(btn => {
      btn.addEventListener("click", async () => {
        try {
          await invoke("revoke_http_media_device", { tokenHash: btn.dataset.revokeHttpMedia });
          showToast("Device revoked.", "success");
          await loadHttpMediaDevices();
        } catch (err) {
          showToast(String(err), "error");
        }
      });
    });
  } catch (err) {
    list.innerHTML = `<p class="muted">Unable to load paired plain-HTTP devices.</p>`;
    showToast(String(err), "error");
  }
}

// ---- SWARM service link status ---------------------------------------------
//
// The server's link to the SWARM service used to be attempted once at startup;
// if it failed nothing said so, and every SWARM-paired TV showed this server
// offline while everything here looked healthy. The backend now retries on its
// own and reports where it stands, so this only has to say so out loud: a tab
// badge (visible from any tab), a status block on this tab, and a toast when
// the state changes.

let lastSwarmLinkState = null;
let lastSwarmLinkHtml = null;

function formatOutage(failingSince) {
  if (!failingSince) return "";
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - failingSince);
  if (seconds < 60) return "less than a minute";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  return hours < 48 ? `${hours} h` : `${Math.floor(hours / 24)} days`;
}

function swarmLinkStatusHtml(status) {
  if (status.state === "unreachable") {
    const outage = formatOutage(status.failing_since);
    return `<div class="link-status link-status-warn">
      <strong><i class="bi bi-exclamation-triangle-fill"></i>SWARM service unreachable</strong>
      <p class="muted">SWARM-paired TVs show this server as offline until it reconnects. TVs on this network are unaffected. Retrying automatically.</p>
      <div class="link-status-meta">
        ${status.base_url ? `<span class="mono">${esc(status.base_url)}</span>` : ""}
        ${status.last_error ? `<span>${esc(status.last_error)}</span>` : ""}
        ${outage ? `<span>Unreachable for ${esc(outage)}</span>` : ""}
      </div>
      <div class="row action-row">
        <button id="forgetSwarmLinkBtn" class="danger-button compact" title="Only if this address is no longer right"><i class="bi bi-x-circle"></i>Forget this SWARM service</button>
      </div>
    </div>`;
  }
  if (status.state === "connecting") {
    return `<div class="link-status"><span class="muted"><i class="bi bi-arrow-repeat"></i> Connecting to the SWARM service…</span></div>`;
  }
  if (status.state === "connected") {
    return `<div class="link-status"><span class="note"><i class="bi bi-check-circle-fill"></i> Connected to the SWARM service</span></div>`;
  }
  return "";
}

async function refreshSwarmLinkStatus() {
  let status;
  try {
    status = await invoke("get_swarm_link_status");
  } catch {
    // Best-effort background poll — a failed check isn't worth a toast every interval.
    return;
  }
  const state = status && status.state ? status.state : "not_linked";

  const badge = document.getElementById("swarmLinkBadge");
  if (badge) badge.classList.toggle("d-none", state !== "unreachable");

  // Re-render only when the content changed, so a button under the cursor is
  // not replaced out from under a click.
  const box = document.getElementById("swarmLinkStatus");
  const html = swarmLinkStatusHtml({ ...status, state });
  if (box && html !== lastSwarmLinkHtml) {
    lastSwarmLinkHtml = html;
    box.innerHTML = html;
    box.classList.toggle("d-none", html === "");
    const forget = document.getElementById("forgetSwarmLinkBtn");
    if (forget) {
      forget.addEventListener("click", async () => {
        try {
          await invoke("forget_swarm_link");
          showToast("Forgot the SWARM service.", "success");
          await refreshSwarm();
        } catch (err) {
          showToast(String(err), "error");
        }
      });
    }
  }

  if (state !== lastSwarmLinkState) {
    if (state === "unreachable") {
      showToast("Can't reach the SWARM service. SWARM-paired TVs will show this server as offline until it reconnects.", "warning");
    } else if (state === "connected" && lastSwarmLinkState === "unreachable") {
      showToast("Reconnected to the SWARM service.", "success");
    }
    lastSwarmLinkState = state;
  }
}

async function refreshSwarm() {
  refreshSwarmLinkStatus();
  loadLocalPeers();
  loadHttpMediaDevices();
  const content = document.getElementById("swarmContent");
  let link;
  try {
    link = await invoke("get_swarm_link");
  } catch (err) {
    content.innerHTML = `<p class="muted">Unable to load swarm status.</p>`;
    showToast(String(err), "error");
    return;
  }

  if (!link) {
    content.innerHTML = `<p class="muted"><i class="bi bi-link-45deg"></i> Not linked to a SWARM service yet.</p>`;
    return;
  }

  content.innerHTML = `
    <div id="swarmList"></div>
    <div class="row action-row">
      <button id="resyncBtn" class="secondary-button"><i class="bi bi-arrow-repeat"></i>Resync now</button>
    </div>`;

  const swarmList = document.getElementById("swarmList");
  swarmList.innerHTML = link.swarms.map(s => `
    <div class="service-card swarm-card">
      <div class="card-head">
        <strong class="swarm-name"><i class="bi bi-diagram-3"></i><span>${esc(s.name)}</span></strong>
        <button class="danger-button compact" data-leave-swarm="${esc(s.id)}"><i class="bi bi-box-arrow-right"></i>Leave</button>
      </div>
      <div id="roster-${esc(s.id)}" class="muted">Loading roster…</div>
    </div>`).join("");

  for (const s of link.swarms) {
    loadRoster(s.id);
  }
  swarmList.querySelectorAll("[data-leave-swarm]").forEach(btn => {
    btn.addEventListener("click", async () => {
      try {
        await invoke("leave_swarm", { swarmId: btn.dataset.leaveSwarm });
        showToast("Left swarm.", "success");
        await refreshSwarm();
      } catch (err) {
        showToast(String(err), "error");
      }
    });
  });

  document.getElementById("resyncBtn").addEventListener("click", async () => {
    try {
      await invoke("resync_swarm");
      showToast("Resynced.", "success");
      await refreshSwarm();
    } catch (err) {
      showToast(String(err), "error");
    }
  });
}

async function loadRoster(swarmId) {
  const el = document.getElementById(`roster-${swarmId}`);
  if (!el) return;
  try {
    const roster = await invoke("get_swarm_devices", { swarmId });
    if (!roster.devices.length) {
      el.innerHTML = `<span class="muted">No devices yet.</span>`;
      return;
    }
    const metaKeys = [...new Set(roster.devices.flatMap(d => Object.keys(d.metadata || {})))];
    el.innerHTML = `<table>
      <thead><tr>
        <th>Name</th><th>Type</th><th>Online</th><th>Last seen</th>
        <th class="info-trigger" data-info="device-fingerprint" tabindex="0" role="button">Fingerprint <i class="bi bi-info-circle info-affordance"></i></th>
        ${metaKeys.map(k => `<th>${esc(k)}</th>`).join("")}
      </tr></thead>
      <tbody>` + roster.devices.map(d => `<tr>
        <td>${esc(d.name)}</td>
        <td>${esc(d.device_type)}</td>
        <td>${d.online ? "✓" : "—"}</td>
        <td class="mono">${esc(d.last_seen_at || "—")}</td>
        <td class="mono">${esc((d.cert_fingerprint || "").slice(0, 12))}…</td>
        ${metaKeys.map(k => `<td class="mono">${esc((d.metadata || {})[k] ?? "—")}</td>`).join("")}
      </tr>`).join("") + `</tbody></table>`;
  } catch (err) {
    el.innerHTML = `<span class="muted">Unable to load roster.</span>`;
    showToast(String(err), "error");
  }
}
