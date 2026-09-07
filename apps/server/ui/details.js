// ---- Details tab: bandwidth/transcoding/cache panels + media-root config ---

async function refreshDetails() {
  await Promise.all([refreshMediaRoots(), refreshTmdbKeyField(), refreshOpenSubtitlesKeyField(), refreshTranscriptionSetting(), refreshTranscodingControls(), refreshBandwidth(), refreshTranscoding(), refreshArtworkCache(), refreshSoftwareUpdate()]);
}

let softwareUpdatePending = null;

async function refreshSoftwareUpdate() {
  const settings = await invoke("get_settings");
  const select = document.getElementById("autoUpdateModeSelect");
  select.value = settings.auto_update || "notify";
  select.dataset.previous = select.value;
  const status = document.getElementById("softwareUpdateStatus");
  if (!softwareUpdatePending) {
    status.textContent = `Running version ${settings.app_version}.`;
  }
}

function showPendingUpdate(summary) {
  softwareUpdatePending = summary;
  const status = document.getElementById("softwareUpdateStatus");
  status.textContent = `Version ${summary.version} is available.` + (summary.notes ? ` ${summary.notes.trim().split("\n")[0]}` : "");
  document.getElementById("installUpdateBtn").classList.remove("d-none");
}

document.getElementById("autoUpdateModeSelect").addEventListener("change", async (event) => {
  const select = event.currentTarget;
  const previous = select.dataset.previous || "notify";
  const mode = select.value;
  try {
    await invoke("set_auto_update", { mode });
    select.dataset.previous = mode;
    showToast("Update preference saved.", "success");
  } catch (err) {
    select.value = previous;
    showToast(String(err), "error");
  }
});

document.getElementById("checkUpdateBtn").addEventListener("click", async (event) => {
  const button = event.currentTarget;
  button.disabled = true;
  document.getElementById("softwareUpdateStatus").textContent = "Checking for updates…";
  try {
    const summary = await invoke("check_for_update");
    if (summary) {
      showPendingUpdate(summary);
    } else {
      softwareUpdatePending = null;
      document.getElementById("installUpdateBtn").classList.add("d-none");
      document.getElementById("softwareUpdateStatus").textContent = "SWARM Server is up to date.";
    }
  } catch (err) {
    showToast(String(err), "error");
  } finally {
    button.disabled = false;
  }
});

document.getElementById("installUpdateBtn").addEventListener("click", async (event) => {
  event.currentTarget.disabled = true;
  showToast("Downloading update… the server will restart when it's ready.", "success");
  try {
    await invoke("install_update"); // process restarts on success
  } catch (err) {
    event.currentTarget.disabled = false;
    showToast(String(err), "error");
  }
});

listen("update-available", (event) => showPendingUpdate(event.payload));
listen("update-staged", (event) => {
  document.getElementById("softwareUpdateStatus").textContent =
    `Version ${event.payload.version} is installed and will run after the next restart.`;
});

async function refreshTranscodingControls() {
  const settings = await invoke("get_settings");
  const set = (id, value) => {
    const select = document.getElementById(id);
    select.value = value;
    select.dataset.previous = value;
  };
  set("videoEncoderModeSelect", settings.video_encoder_mode || "auto");
  set("maxTranscodeHeightSelect", String(settings.max_transcode_height ?? 0));
  set("hlsSegmentSecondsSelect", String(settings.hls_segment_seconds ?? 4));
}

async function refreshTmdbKeyField() {
  const settings = await invoke("get_settings");
  document.getElementById("uploadBudgetEnabledCheck").checked = settings.streaming_upload_budget_enabled;
  document.getElementById("artworkDiskCacheEnabledCheck").checked = settings.artwork_disk_cache_enabled;
  document.getElementById("autoLibraryWatchEnabledCheck").checked = settings.auto_library_watch_enabled;
  document.getElementById("comprehensiveCheck").checked = settings.comprehensive_check;
  document.getElementById("scanMusicTracksCheck").checked = settings.scan_music_tracks;
  const status = document.getElementById("tmdbKeyStatus");
  status.textContent = settings.has_tmdb_key ? "A key is saved. Scraping is enabled." : "No key saved yet — scraping is disabled until one is added.";
  status.classList.toggle("error", !settings.has_tmdb_key);
}

async function refreshOpenSubtitlesKeyField() {
  const settings = await invoke("get_settings");
  const status = document.getElementById("openSubtitlesKeyStatus");
  status.textContent = settings.has_opensubtitles_key
    ? "An API key is saved. Subtitle download is available from each movie or episode's Manage panel."
    : "No key saved — local Whisper generation still works, but subtitle download is disabled.";
  status.classList.toggle("error", !settings.has_opensubtitles_key);
}

async function refreshTranscriptionSetting() {
  const settings = await invoke("get_settings");
  document.getElementById("localTranscriptionEnabledCheck").checked = settings.local_transcription_enabled;
  document.getElementById("transcriptionPauseWhileStreamingCheck").checked = settings.transcription_pause_while_streaming;
  document.getElementById("transcriptionSkipIfSubtitlesExistCheck").checked = settings.transcription_skip_if_subtitles_exist;
  const statusEl = document.getElementById("localTranscriptionSettingStatus");
  try {
    const status = await invoke("get_transcription_status");
    if (!settings.local_transcription_enabled) {
      statusEl.textContent = status.model_installed
        ? "Paused. The installed model and completed work are preserved."
        : "Off. The ~142 MB Whisper model will download automatically the first time this is enabled.";
    } else {
      statusEl.textContent = status.message;
    }
  } catch (err) {
    statusEl.textContent = "Unable to load subtitle-generation status.";
  }
}

document.getElementById("localTranscriptionEnabledCheck").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await invoke("set_local_transcription_enabled", { enabled });
    if (enabled) {
      openInfoModal("local-subtitles");
      showToast("Local subtitles enabled. SWARM will download the Whisper model automatically if needed.", "success", { duration: 7000 });
    } else {
      showToast("Local subtitle generation paused. Progress has been saved.", "success");
    }
    await Promise.all([refreshTranscriptionSetting(), refreshTranscriptionProgress()]);
  } catch (err) {
    event.currentTarget.checked = !enabled;
    showToast(String(err), "error");
  }
});

document.getElementById("transcriptionPauseWhileStreamingCheck").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await invoke("set_transcription_pause_while_streaming", { enabled });
    showToast(
      enabled
        ? "Subtitle generation will pause while clients are streaming."
        : "Subtitle generation may now use CPU while clients are streaming.",
      "success",
    );
    await refreshTranscriptionSetting();
  } catch (err) {
    event.currentTarget.checked = !enabled;
    showToast(String(err), "error");
  }
});

document.getElementById("transcriptionSkipIfSubtitlesExistCheck").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await invoke("set_transcription_skip_if_subtitles_exist", { enabled });
    showToast(
      enabled
        ? "Bulk generation will skip movies/episodes that already have subtitles."
        : "Bulk generation will regenerate subtitles for every eligible movie/episode.",
      "success",
    );
  } catch (err) {
    event.currentTarget.checked = !enabled;
    showToast(String(err), "error");
  }
});

document.getElementById("uploadBudgetEnabledCheck").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await invoke("set_streaming_upload_budget_enabled", { enabled });
    await refreshBandwidth();
    showToast(enabled ? "Internet streaming budget enabled." : "Internet streaming budget disabled.", "success");
  } catch (err) {
    event.currentTarget.checked = !enabled;
    showToast(String(err), "error");
  }
});

document.getElementById("artworkDiskCacheEnabledCheck").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await invoke("set_artwork_disk_cache_enabled", { enabled });
    showToast(
      enabled ? "Artwork disk cache enabled." : "Artwork disk cache disabled.",
      "success",
    );
    await refreshArtworkCache();
  } catch (err) {
    event.currentTarget.checked = !enabled;
    showToast(String(err), "error");
  }
});

document.getElementById("autoLibraryWatchEnabledCheck").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await invoke("set_auto_library_watch_enabled", { enabled });
    showToast(enabled ? "Automatic library detection enabled." : "Automatic library detection disabled.", "success");
  } catch (err) {
    event.currentTarget.checked = !enabled;
    showToast(String(err), "error");
  }
});

document.getElementById("comprehensiveCheck").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await invoke("set_comprehensive_check", { enabled });
    showToast(enabled ? "Comprehensive Check enabled." : "Fast filesystem checks enabled.", "success");
  } catch (err) {
    event.currentTarget.checked = !enabled;
    showToast(String(err), "error");
  }
});

document.getElementById("scanMusicTracksCheck").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await invoke("set_scan_music_tracks", { enabled });
    showToast(enabled ? "Music track scanning enabled." : "Music track scanning disabled.", "success");
  } catch (err) {
    event.currentTarget.checked = !enabled;
    showToast(String(err), "error");
  }
});

document.getElementById("videoEncoderModeSelect").addEventListener("change", async (event) => {
  const select = event.currentTarget;
  const previous = select.dataset.previous || "auto";
  const mode = select.value;
  try {
    await invoke("set_video_encoder_mode", { mode });
    select.dataset.previous = mode;
    showToast(`Video encoder set to ${select.options[select.selectedIndex].text.toLowerCase()}.`, "success");
  } catch (err) {
    select.value = previous;
    showToast(String(err), "error");
  }
});

document.getElementById("maxTranscodeHeightSelect").addEventListener("change", async (event) => {
  const select = event.currentTarget;
  const previous = select.dataset.previous || "0";
  const height = Number(select.value);
  try {
    await invoke("set_max_transcode_height", { height });
    select.dataset.previous = select.value;
    showToast(height === 0 ? "No transcode resolution cap." : `Transcodes capped at ${height}p.`, "success");
  } catch (err) {
    select.value = previous;
    showToast(String(err), "error");
  }
});

document.getElementById("hlsSegmentSecondsSelect").addEventListener("change", async (event) => {
  const select = event.currentTarget;
  const previous = select.dataset.previous || "4";
  const seconds = Number(select.value);
  try {
    await invoke("set_hls_segment_seconds", { seconds });
    select.dataset.previous = select.value;
    showToast(`HLS segments set to ${seconds} seconds. New streams use this length.`, "success");
  } catch (err) {
    select.value = previous;
    showToast(String(err), "error");
  }
});

document.getElementById("saveTmdbKeyBtn").addEventListener("click", async () => {
  const input = document.getElementById("tmdbKeyInput");
  try {
    await invoke("set_tmdb_api_key", { key: input.value });
    input.value = "";
    await refreshTmdbKeyField();
    showToast("TMDb key saved.", "success");
  } catch (err) {
    showToast(String(err), "error");
  }
});

document.getElementById("saveOpenSubtitlesKeyBtn").addEventListener("click", async () => {
  const input = document.getElementById("openSubtitlesKeyInput");
  try {
    await invoke("set_opensubtitles_api_key", { key: input.value });
    input.value = "";
    await refreshOpenSubtitlesKeyField();
    showToast("OpenSubtitles key saved.", "success");
  } catch (err) {
    showToast(String(err), "error");
  }
});

// ---- Streaming bandwidth: live graph + "now" panel --------------------
// The "Entries" and "Library size" totals that used to live in a separate
// Status panel here moved to the Media (browse) tab; "Active playback
// sessions" and the upload-budget limit moved into this panel; the device
// fingerprint / library thumbprint tiles were dropped outright — see
// GitHub issue #63.

/// Renders the whole chart from scratch every call (data update or hover
/// frame alike) — the dataset is at most 720 points, so a full redraw stays
/// cheap and avoids maintaining a separate cached-vs-crosshair layer.
let bandwidthChartState = null;

async function refreshBandwidth() {
  try {
    const [samples, status] = await Promise.all([
      invoke("get_bandwidth_history"),
      invoke("get_status").catch(() => null),
    ]);
    renderBandwidthStatus(samples, status);
    drawBandwidthChart(samples, null);
  } catch (err) {
    // Best-effort background poll (every 5s) — a transient failure isn't
    // worth a toast; the next tick tries again.
  }
}

function renderBandwidthStatus(samples, status) {
  const grid = document.getElementById("bandwidthStatusGrid");
  const currentMbps = samples.length ? samples[samples.length - 1].bps / 1_000_000 : 0;
  let tiles = stat("Current streaming bandwidth", formatMbps(currentMbps), false, "streaming-bandwidth");
  if (status) {
    tiles +=
      stat("Active playback sessions", status.active_playback_sessions, false, "active-sessions") +
      stat(
        "Streaming upload budget",
        status.streaming_upload_budget_enabled ? (status.streaming_upload_budget_bps / 1_000_000).toFixed(1) + " Mbps" : "Unlimited",
        false,
        "upload-budget",
      );
    const check = document.getElementById("uploadBudgetEnabledCheck");
    if (check) check.checked = status.streaming_upload_budget_enabled;
  }
  grid.innerHTML = tiles;
}

function formatMbps(mbps) {
  return `${mbps < 10 ? mbps.toFixed(1) : Math.round(mbps)} Mbps`;
}

function formatClock(ms) {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

// Rounds a chart's max value up to a clean gridline (1/2/5/10/20/50…)
// rather than an arbitrary "whatever the peak sample happened to be".
function niceMax(value) {
  if (!(value > 0)) return 1;
  const magnitude = Math.pow(10, Math.floor(Math.log10(value)));
  const normalized = value / magnitude;
  const niceNormalized = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return niceNormalized * magnitude;
}

function hexToRgba(hex, alpha) {
  const clean = hex.replace("#", "");
  const value = parseInt(clean, 16);
  return `rgba(${(value >> 16) & 255}, ${(value >> 8) & 255}, ${value & 255}, ${alpha})`;
}

function drawBandwidthChart(samples, hoverIndex) {
  const canvas = document.getElementById("bandwidthChart");
  document.getElementById("bandwidthChartEmpty").classList.toggle("d-none", samples.length > 0);
  const ctx = canvas.getContext("2d");
  if (!samples.length) {
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    bandwidthChartState = null;
    return;
  }

  const dpr = window.devicePixelRatio || 1;
  const width = canvas.clientWidth || canvas.parentElement.clientWidth;
  const height = canvas.clientHeight || 130;
  canvas.width = width * dpr;
  canvas.height = height * dpr;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);

  const padding = { top: 10, right: 54, bottom: 20, left: 4 };
  const plotW = Math.max(width - padding.left - padding.right, 1);
  const plotH = Math.max(height - padding.top - padding.bottom, 1);
  const minMs = samples[0].timestamp_ms;
  const maxMs = samples[samples.length - 1].timestamp_ms;
  const spanMs = Math.max(maxMs - minMs, 1);
  const maxMbps = niceMax(Math.max(...samples.map((s) => s.bps)) / 1_000_000);

  const x = (ms) => padding.left + ((ms - minMs) / spanMs) * plotW;
  const y = (mbps) => padding.top + plotH - (mbps / maxMbps) * plotH;

  const styles = getComputedStyle(document.documentElement);
  const border = styles.getPropertyValue("--border").trim();
  const muted = styles.getPropertyValue("--muted").trim();
  const accent = styles.getPropertyValue("--accent").trim();
  const surface = styles.getPropertyValue("--surface").trim();
  const text = styles.getPropertyValue("--text").trim();

  // Recessive gridlines + y-axis ticks at 0 / half / max.
  ctx.strokeStyle = border;
  ctx.lineWidth = 1;
  ctx.font = "11px -apple-system, BlinkMacSystemFont, sans-serif";
  ctx.textBaseline = "middle";
  ctx.fillStyle = muted;
  [0, maxMbps / 2, maxMbps].forEach((value) => {
    const yy = Math.round(y(value)) + 0.5;
    ctx.beginPath();
    ctx.moveTo(padding.left, yy);
    ctx.lineTo(padding.left + plotW, yy);
    ctx.stroke();
    ctx.textAlign = "left";
    ctx.fillText(formatMbps(value), padding.left + plotW + 6, yy);
  });

  const linePoints = samples.map((s) => [x(s.timestamp_ms), y(s.bps / 1_000_000)]);

  // Area fill: series hue at ~10% opacity, never a saturated block.
  ctx.beginPath();
  linePoints.forEach(([px, py], i) => (i === 0 ? ctx.moveTo(px, py) : ctx.lineTo(px, py)));
  ctx.lineTo(x(maxMs), padding.top + plotH);
  ctx.lineTo(x(minMs), padding.top + plotH);
  ctx.closePath();
  ctx.fillStyle = hexToRgba(accent, 0.1);
  ctx.fill();

  // 2px line, round join/cap.
  ctx.beginPath();
  linePoints.forEach(([px, py], i) => (i === 0 ? ctx.moveTo(px, py) : ctx.lineTo(px, py)));
  ctx.strokeStyle = accent;
  ctx.lineWidth = 2;
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  ctx.stroke();

  // End marker (>=8px, surface ring) + direct end-label — the single
  // series' identity is the chart title, so no legend box is needed.
  const [lastPx, lastPy] = linePoints[linePoints.length - 1];
  ctx.beginPath();
  ctx.arc(lastPx, lastPy, 4, 0, Math.PI * 2);
  ctx.fillStyle = surface;
  ctx.fill();
  ctx.lineWidth = 2;
  ctx.strokeStyle = accent;
  ctx.stroke();
  ctx.beginPath();
  ctx.arc(lastPx, lastPy, 2.5, 0, Math.PI * 2);
  ctx.fillStyle = accent;
  ctx.fill();
  ctx.fillStyle = text;
  ctx.font = "700 11px -apple-system, BlinkMacSystemFont, sans-serif";
  ctx.textAlign = "left";
  ctx.fillText(formatMbps(samples[samples.length - 1].bps / 1_000_000), lastPx + 8, lastPy);

  // X-axis start/end clock labels.
  ctx.fillStyle = muted;
  ctx.font = "11px -apple-system, BlinkMacSystemFont, sans-serif";
  ctx.textAlign = "left";
  ctx.fillText(formatClock(minMs), padding.left, padding.top + plotH + 14);
  ctx.textAlign = "right";
  ctx.fillText(formatClock(maxMs), padding.left + plotW, padding.top + plotH + 14);

  // Crosshair: a vertical hairline tracking the hovered/nearest sample.
  if (hoverIndex != null && linePoints[hoverIndex]) {
    const [px] = linePoints[hoverIndex];
    ctx.beginPath();
    ctx.moveTo(px, padding.top);
    ctx.lineTo(px, padding.top + plotH);
    ctx.strokeStyle = border;
    ctx.lineWidth = 1;
    ctx.stroke();
  }

  bandwidthChartState = { samples, padding, plotW, minMs, spanMs };
}

function bandwidthIndexForX(px) {
  const { samples, padding, plotW, minMs, spanMs } = bandwidthChartState;
  const ratio = Math.min(Math.max((px - padding.left) / plotW, 0), 1);
  const targetMs = minMs + ratio * spanMs;
  let closest = 0;
  let closestDist = Infinity;
  samples.forEach((s, i) => {
    const dist = Math.abs(s.timestamp_ms - targetMs);
    if (dist < closestDist) {
      closestDist = dist;
      closest = i;
    }
  });
  return closest;
}

const bandwidthCanvas = document.getElementById("bandwidthChart");
const bandwidthTooltip = document.getElementById("bandwidthTooltip");

bandwidthCanvas.addEventListener("mousemove", (event) => {
  if (!bandwidthChartState) return;
  const rect = bandwidthCanvas.getBoundingClientRect();
  const index = bandwidthIndexForX(event.clientX - rect.left);
  const sample = bandwidthChartState.samples[index];
  drawBandwidthChart(bandwidthChartState.samples, index);
  bandwidthTooltip.innerHTML = `<strong>${esc(formatMbps(sample.bps / 1_000_000))}</strong><span>${esc(formatClock(sample.timestamp_ms))}</span>`;
  const px = bandwidthChartState.padding.left + ((sample.timestamp_ms - bandwidthChartState.minMs) / bandwidthChartState.spanMs) * bandwidthChartState.plotW;
  bandwidthTooltip.style.left = `${px}px`;
  bandwidthTooltip.classList.remove("d-none");
});

bandwidthCanvas.addEventListener("mouseleave", () => {
  bandwidthTooltip.classList.add("d-none");
  if (bandwidthChartState) drawBandwidthChart(bandwidthChartState.samples, null);
});

window.addEventListener("resize", () => {
  if (bandwidthChartState) drawBandwidthChart(bandwidthChartState.samples, null);
  if (transcodingSamples) drawTranscodingChart(transcodingSamples, null);
  if (artworkCacheSnapshot) drawArtworkCacheChart(artworkCacheSnapshot, null);
});

// Every 5 seconds — matching the server's sample cadence — while the
// Details tab is the one on screen; refreshDetails() covers the moment the
// tab is first opened so there's no up-to-5s wait for the first paint.
setInterval(() => {
  const panel = document.getElementById("tabPanel-details");
  if (panel && !panel.classList.contains("d-none")) {
    refreshBandwidth();
    refreshTranscoding();
    refreshArtworkCache();
  }
}, 5000);

// ---- Transcoding: CPU cost of ffmpeg + local subtitles ---------------

/// Full redraw every call (data tick or hover frame). At most 720 samples,
/// two stacked series — same cheap-full-redraw approach as the bandwidth
/// chart above, no cached layer.
let transcodingSamples = null;
let transcodingChartState = null;

async function refreshTranscoding() {
  try {
    const samples = await invoke("get_transcoding_history");
    transcodingSamples = Array.isArray(samples) ? samples : [];
    renderTranscodingStatus(transcodingSamples);
    drawTranscodingChart(transcodingSamples, null);
  } catch (err) {
    // Best-effort background poll (every 5s); the next tick retries.
  }
}

function formatPercent(value) {
  return `${Math.round(value)}%`;
}

function renderTranscodingStatus(samples) {
  const grid = document.getElementById("transcodingStatusGrid");
  const latest = samples.length ? samples[samples.length - 1] : null;
  const subtitles = !latest
    ? "—"
    : latest.subtitle_active
      ? "Running"
      : latest.subtitle_queued > 0
        ? `${latest.subtitle_queued} queued`
        : "Idle";
  const cpuNow = latest ? latest.transcode_cpu_percent + latest.server_cpu_percent : 0;
  grid.innerHTML =
    stat("Active transcodes", latest ? latest.transcode_sessions : 0, false, "transcoding") +
    stat("Direct-play streams", latest ? latest.direct_sessions : 0, false, "transcoding") +
    stat("Subtitle generation", subtitles, false, "transcoding") +
    stat("CPU in use", formatPercent(cpuNow), false, "transcoding");
}

function drawTranscodingChart(samples, hoverIndex) {
  const canvas = document.getElementById("transcodingChart");
  document.getElementById("transcodingChartEmpty").classList.toggle("d-none", samples.length > 0);
  const ctx = canvas.getContext("2d");
  if (!samples.length) {
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    transcodingChartState = null;
    return;
  }

  const dpr = window.devicePixelRatio || 1;
  const width = canvas.clientWidth || canvas.parentElement.clientWidth;
  const height = canvas.clientHeight || 130;
  canvas.width = width * dpr;
  canvas.height = height * dpr;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);

  const padding = { top: 10, right: 44, bottom: 20, left: 4 };
  const plotW = Math.max(width - padding.left - padding.right, 1);
  const plotH = Math.max(height - padding.top - padding.bottom, 1);
  const minMs = samples[0].timestamp_ms;
  const maxMs = samples[samples.length - 1].timestamp_ms;
  const spanMs = Math.max(maxMs - minMs, 1);
  const totals = samples.map((s) => s.transcode_cpu_percent + s.server_cpu_percent);
  const maxPct = niceMax(Math.max(5, ...totals));

  const x = (ms) => padding.left + ((ms - minMs) / spanMs) * plotW;
  const y = (pct) => padding.top + plotH - (Math.min(pct, maxPct) / maxPct) * plotH;

  const styles = getComputedStyle(document.documentElement);
  const border = styles.getPropertyValue("--border").trim();
  const muted = styles.getPropertyValue("--muted").trim();
  const accent = styles.getPropertyValue("--accent").trim();
  const green = styles.getPropertyValue("--green").trim();
  const text = styles.getPropertyValue("--text").trim();

  ctx.strokeStyle = border;
  ctx.lineWidth = 1;
  ctx.font = "11px -apple-system, BlinkMacSystemFont, sans-serif";
  ctx.textBaseline = "middle";
  ctx.fillStyle = muted;
  [0, maxPct / 2, maxPct].forEach((value) => {
    const yy = Math.round(y(value)) + 0.5;
    ctx.beginPath();
    ctx.moveTo(padding.left, yy);
    ctx.lineTo(padding.left + plotW, yy);
    ctx.stroke();
    ctx.textAlign = "left";
    ctx.fillText(formatPercent(value), padding.left + plotW + 6, yy);
  });

  // Stacked bands: ffmpeg transcoding (0 → a), then the rest of the server
  // process stacked on top (a → a+b).
  const base = padding.top + plotH;
  const lowerTop = samples.map((s) => [x(s.timestamp_ms), y(s.transcode_cpu_percent)]);
  const upperTop = samples.map((s) => [x(s.timestamp_ms), y(s.transcode_cpu_percent + s.server_cpu_percent)]);
  const flatBase = lowerTop.map(([px]) => [px, base]);

  const fillBand = (topPts, bottomPts, color) => {
    ctx.beginPath();
    topPts.forEach(([px, py], i) => (i === 0 ? ctx.moveTo(px, py) : ctx.lineTo(px, py)));
    for (let i = bottomPts.length - 1; i >= 0; i--) ctx.lineTo(bottomPts[i][0], bottomPts[i][1]);
    ctx.closePath();
    ctx.fillStyle = hexToRgba(color, 0.16);
    ctx.fill();
  };
  fillBand(lowerTop, flatBase, accent);
  fillBand(upperTop, lowerTop, green);

  [[lowerTop, accent], [upperTop, green]].forEach(([pts, color]) => {
    ctx.beginPath();
    pts.forEach(([px, py], i) => (i === 0 ? ctx.moveTo(px, py) : ctx.lineTo(px, py)));
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.stroke();
  });

  const last = samples[samples.length - 1];
  const [lastPx, lastPy] = upperTop[upperTop.length - 1];
  ctx.fillStyle = text;
  ctx.font = "700 11px -apple-system, BlinkMacSystemFont, sans-serif";
  ctx.textAlign = "left";
  ctx.fillText(formatPercent(last.transcode_cpu_percent + last.server_cpu_percent), lastPx + 8, lastPy);

  ctx.fillStyle = muted;
  ctx.font = "11px -apple-system, BlinkMacSystemFont, sans-serif";
  ctx.textAlign = "left";
  ctx.fillText(formatClock(minMs), padding.left, padding.top + plotH + 14);
  ctx.textAlign = "right";
  ctx.fillText(formatClock(maxMs), padding.left + plotW, padding.top + plotH + 14);

  if (hoverIndex != null && upperTop[hoverIndex]) {
    const [px] = upperTop[hoverIndex];
    ctx.beginPath();
    ctx.moveTo(px, padding.top);
    ctx.lineTo(px, padding.top + plotH);
    ctx.strokeStyle = border;
    ctx.lineWidth = 1;
    ctx.stroke();
  }

  transcodingChartState = { samples, padding, plotW, minMs, spanMs };
}

const transcodingCanvas = document.getElementById("transcodingChart");
const transcodingTooltip = document.getElementById("transcodingTooltip");

transcodingCanvas.addEventListener("mousemove", (event) => {
  if (!transcodingChartState) return;
  const { samples, padding, plotW, minMs, spanMs } = transcodingChartState;
  const rect = transcodingCanvas.getBoundingClientRect();
  const ratio = Math.min(Math.max((event.clientX - rect.left - padding.left) / plotW, 0), 1);
  const targetMs = minMs + ratio * spanMs;
  let index = 0;
  let best = Infinity;
  samples.forEach((sample, i) => {
    const distance = Math.abs(sample.timestamp_ms - targetMs);
    if (distance < best) {
      best = distance;
      index = i;
    }
  });
  const sample = samples[index];
  drawTranscodingChart(samples, index);
  const total = sample.transcode_cpu_percent + sample.server_cpu_percent;
  transcodingTooltip.innerHTML = `<strong>${esc(formatPercent(total))} CPU</strong><span>${esc(`${sample.transcode_sessions} transcoding · ${formatClock(sample.timestamp_ms)}`)}</span>`;
  transcodingTooltip.style.left = `${padding.left + ((sample.timestamp_ms - minMs) / spanMs) * plotW}px`;
  transcodingTooltip.classList.remove("d-none");
});

transcodingCanvas.addEventListener("mouseleave", () => {
  transcodingTooltip.classList.add("d-none");
  if (transcodingChartState) drawTranscodingChart(transcodingChartState.samples, null);
});

// ---- Artwork cache: activity by client + disk usage -------------------

let artworkCacheSnapshot = null;
let artworkCacheChartState = null;

async function refreshArtworkCache() {
  try {
    const snapshot = await invoke("get_artwork_cache_snapshot");
    artworkCacheSnapshot = snapshot;
    document.getElementById("artworkDiskCacheEnabledCheck").checked = snapshot.enabled;
    document.getElementById("artworkCachePath").textContent = snapshot.cache_dir || "Not available";
    renderArtworkCacheStatus(snapshot);
    refreshArtworkCacheClients(snapshot.events);
    renderArtworkCacheRecent(snapshot.events);
    drawArtworkCacheChart(snapshot, null);
  } catch (err) {
    // This is a live, best-effort panel; the next five-second poll retries.
  }
}

function formatDiskUsage(bytes) {
  if (!(bytes > 0)) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / Math.pow(1024, index);
  return `${value < 10 && index > 0 ? value.toFixed(1) : Math.round(value)} ${units[index]}`;
}

function renderArtworkCacheStatus(snapshot) {
  const fills = snapshot.events.filter(event => event.kind === "cached").length;
  const hits = snapshot.events.filter(event => event.kind === "served_from_cache").length;
  document.getElementById("artworkCacheStatusGrid").innerHTML =
    stat("Cache status", snapshot.enabled ? "On" : "Off", false, "artwork-cache") +
    stat("Disk space used", formatDiskUsage(snapshot.disk_bytes), false, "artwork-cache") +
    stat("Files on disk", snapshot.file_count, false, "artwork-cache") +
    stat("Last 60 minutes", `${fills} cached · ${hits} hits`, false, "artwork-cache");
}

function refreshArtworkCacheClients(events) {
  const select = document.getElementById("artworkCacheClientSelect");
  const selected = select.value;
  const clients = [...new Set(events.map(event => event.client))].sort((a, b) => a.localeCompare(b));
  select.replaceChildren(new Option("All clients", ""), ...clients.map(client => new Option(client, client)));
  select.value = clients.includes(selected) ? selected : "";
}

function filteredArtworkCacheEvents(events) {
  const client = document.getElementById("artworkCacheClientSelect").value;
  return client ? events.filter(event => event.client === client) : events;
}

function renderArtworkCacheRecent(events) {
  const recent = filteredArtworkCacheEvents(events).slice(-5).reverse();
  const container = document.getElementById("artworkCacheRecent");
  if (!recent.length) {
    container.textContent = "No recent cache activity for this client.";
    return;
  }
  container.innerHTML = recent.map(event => {
    const action = event.kind === "cached" ? "Cached from media root" : "Served from local cache";
    const icon = event.kind === "cached" ? "bi-cloud-arrow-down" : "bi-lightning-charge-fill";
    return `<span><i class="bi ${icon}"></i><strong>${esc(event.client)}</strong> ${action.toLowerCase()} <time>${esc(formatClock(event.timestamp_ms))}</time></span>`;
  }).join("");
}

function artworkCacheBuckets(events) {
  const intervalMs = 5000;
  const endMs = Date.now();
  const startMs = endMs - 60 * 60 * 1000;
  const buckets = Array.from({ length: 721 }, (_, index) => ({
    timestamp_ms: startMs + index * intervalMs,
    cached: 0,
    served_from_cache: 0,
  }));
  events.forEach(event => {
    const index = Math.floor((event.timestamp_ms - startMs) / intervalMs);
    if (index >= 0 && index < buckets.length) buckets[index][event.kind] += 1;
  });
  return buckets;
}

function drawArtworkCacheChart(snapshot, hoverIndex) {
  const canvas = document.getElementById("artworkCacheChart");
  const events = filteredArtworkCacheEvents(snapshot.events);
  document.getElementById("artworkCacheChartEmpty").classList.toggle("d-none", events.length > 0);
  const ctx = canvas.getContext("2d");
  if (!events.length) {
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    artworkCacheChartState = null;
    return;
  }

  const buckets = artworkCacheBuckets(events);
  const dpr = window.devicePixelRatio || 1;
  const width = canvas.clientWidth || canvas.parentElement.clientWidth;
  const height = canvas.clientHeight || 130;
  canvas.width = width * dpr;
  canvas.height = height * dpr;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);

  const padding = { top: 10, right: 42, bottom: 20, left: 4 };
  const plotW = Math.max(width - padding.left - padding.right, 1);
  const plotH = Math.max(height - padding.top - padding.bottom, 1);
  const maxCount = Math.max(1, Math.ceil(Math.max(...buckets.flatMap(bucket => [bucket.cached, bucket.served_from_cache]))));
  const x = index => padding.left + (index / (buckets.length - 1)) * plotW;
  const y = count => padding.top + plotH - (count / maxCount) * plotH;
  const styles = getComputedStyle(document.documentElement);
  const border = styles.getPropertyValue("--border").trim();
  const muted = styles.getPropertyValue("--muted").trim();
  const accent = styles.getPropertyValue("--accent").trim();
  const green = styles.getPropertyValue("--green").trim();

  ctx.strokeStyle = border;
  ctx.fillStyle = muted;
  ctx.font = "11px -apple-system, BlinkMacSystemFont, sans-serif";
  ctx.textBaseline = "middle";
  [0, maxCount].forEach(count => {
    const yy = Math.round(y(count)) + 0.5;
    ctx.beginPath();
    ctx.moveTo(padding.left, yy);
    ctx.lineTo(padding.left + plotW, yy);
    ctx.stroke();
    ctx.textAlign = "left";
    ctx.fillText(String(count), padding.left + plotW + 6, yy);
  });

  [["cached", accent], ["served_from_cache", green]].forEach(([key, color]) => {
    ctx.beginPath();
    buckets.forEach((bucket, index) => index === 0 ? ctx.moveTo(x(index), y(bucket[key])) : ctx.lineTo(x(index), y(bucket[key])));
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.stroke();
  });

  ctx.fillStyle = muted;
  ctx.font = "11px -apple-system, BlinkMacSystemFont, sans-serif";
  ctx.textAlign = "left";
  ctx.fillText(formatClock(buckets[0].timestamp_ms), padding.left, padding.top + plotH + 14);
  ctx.textAlign = "right";
  ctx.fillText(formatClock(buckets[buckets.length - 1].timestamp_ms), padding.left + plotW, padding.top + plotH + 14);

  if (hoverIndex != null) {
    const px = x(hoverIndex);
    ctx.beginPath();
    ctx.moveTo(px, padding.top);
    ctx.lineTo(px, padding.top + plotH);
    ctx.strokeStyle = border;
    ctx.lineWidth = 1;
    ctx.stroke();
  }
  artworkCacheChartState = { buckets, padding, plotW };
}

const artworkCacheCanvas = document.getElementById("artworkCacheChart");
const artworkCacheTooltip = document.getElementById("artworkCacheTooltip");

artworkCacheCanvas.addEventListener("mousemove", event => {
  if (!artworkCacheChartState || !artworkCacheSnapshot) return;
  const rect = artworkCacheCanvas.getBoundingClientRect();
  const ratio = Math.min(Math.max((event.clientX - rect.left - artworkCacheChartState.padding.left) / artworkCacheChartState.plotW, 0), 1);
  const index = Math.round(ratio * (artworkCacheChartState.buckets.length - 1));
  const bucket = artworkCacheChartState.buckets[index];
  drawArtworkCacheChart(artworkCacheSnapshot, index);
  artworkCacheTooltip.innerHTML = `<strong>${bucket.cached} cached · ${bucket.served_from_cache} hits</strong><span>${esc(formatClock(bucket.timestamp_ms))}</span>`;
  artworkCacheTooltip.style.left = `${artworkCacheChartState.padding.left + ratio * artworkCacheChartState.plotW}px`;
  artworkCacheTooltip.classList.remove("d-none");
});

artworkCacheCanvas.addEventListener("mouseleave", () => {
  artworkCacheTooltip.classList.add("d-none");
  if (artworkCacheSnapshot) drawArtworkCacheChart(artworkCacheSnapshot, null);
});

document.getElementById("artworkCacheClientSelect").addEventListener("change", () => {
  if (!artworkCacheSnapshot) return;
  renderArtworkCacheRecent(artworkCacheSnapshot.events);
  drawArtworkCacheChart(artworkCacheSnapshot, null);
});

async function refreshMediaRoots() {
  const list = document.getElementById("mediaRootsList");
  try {
    const [roots, health] = await Promise.all([
      invoke("list_media_roots"),
      invoke("get_media_root_health"),
    ]);
    const healthByLabel = new Map(health.map(root => [root.label, root]));
    list.innerHTML = roots.map(r => {
      const status = healthByLabel.get(r.label);
      const protocol = status?.network_protocol;
      const permissionDenied = Boolean(status && !status.available && status.permission_denied);
      let reconnectButton = "";
      if (status && !status.available && protocol === "SMB" && !permissionDenied) {
        reconnectButton = `<button class="secondary" data-repair-smb="${esc(r.label)}"><i class="bi bi-tools"></i>Repair SMB</button>`;
      }
      if (permissionDenied) {
        reconnectButton = `<button class="secondary" data-grant-file-access><i class="bi bi-shield-lock"></i>Open macOS Settings</button>`;
      }
      const statusLabel = status
        ? (status.available ? "Connected" : (permissionDenied ? "Permission needed" : "Unavailable"))
        : "";
      const permissionHint = permissionDenied
        ? `<div class="muted compact-help">macOS is blocking reads here. Grant "SWARM Server" access under Privacy &amp; Security &rarr; Files and Folders (or Full Disk Access), then Rescan — macOS remembers it.</div>`
        : "";
      const assetLabels = { mixed: "Legacy mixed", movies: "Movies", shows: "TV shows", music: "Music", photos_videos: "Photos & videos" };
      const assetType = assetLabels[r.asset_type] || "Mixed";
      return `
      <div class="media-root-row">
        <div class="media-root-info">
          <div class="media-root-label">${esc(r.label)}<span class="media-root-asset">${esc(assetType)}</span>${protocol ? `<span class="media-root-protocol">${esc(protocol)}</span>` : ""}${status ? `<span class="media-root-status ${status.available ? "" : "media-root-status-unavailable"}">${statusLabel}</span>` : ""}</div>
          <div class="mono muted media-root-path">${esc(r.path)}</div>
          ${permissionHint}
        </div>
        <div class="media-root-actions">
          ${reconnectButton}
          <button class="danger" data-remove-root="${esc(r.label)}"><i class="bi bi-trash"></i>Remove</button>
        </div>
      </div>`;
    }).join("");
    list.querySelectorAll("[data-remove-root]").forEach(btn => {
      btn.addEventListener("click", async () => {
        try {
          const result = await invoke("remove_media_root", { label: btn.dataset.removeRoot });
          if (!result.media_roots || result.media_roots.length === 0) {
            // Issue #252: the last root can be removed. With none configured
            // there's nothing to show on the dashboard — return to the
            // "choose a media folder" onboarding view.
            showToast("Media root removed. Choose a folder to start over.", "success");
            show("onboardFolderView");
            return;
          }
          await refreshMediaRoots();
          describeRootChange(result);
        } catch (err) {
          showToast(String(err), "error");
        }
      });
    });
    list.querySelectorAll("[data-grant-file-access]").forEach(btn => {
      btn.addEventListener("click", async () => {
        try {
          await invoke("open_external_url", {
            url: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles",
          });
        } catch (err) {
          showToast(String(err), "error");
        }
      });
    });
    list.querySelectorAll("[data-repair-smb]").forEach(btn => {
      btn.addEventListener("click", async () => {
        btn.disabled = true;
        try {
          const result = await invoke("repair_smb_root", { label: btn.dataset.repairSmb });
          await Promise.all([refreshMediaRoots(), refreshMediaRootHealth()]);
          if (result.rescan) {
            describeRootChange(result);
          } else {
            showToast("SMB share repaired — the existing library is ready; no rescan was needed.", "success");
          }
        } catch (err) {
          showToast(String(err), "error");
          btn.disabled = false;
        }
      });
    });
  } catch (err) {
    list.innerHTML = `<p class="muted">Unable to load media roots.</p>`;
    showToast(String(err), "error");
  }
}

// Shows how a live add/remove actually landed — a rescan report if a core
// was already running (this change took effect immediately, no restart), or
// nothing if it's still first-run onboarding (there's no core yet to apply
// it to; the choice is just saved for when one starts).
function describeRootChange(result) {
  if (!result.rescan) return;
  const { added, updated, removed, unchanged } = result.rescan;
  showToast(`Applied — scanned now: +${added} added, ${updated} updated, ${removed} removed, ${unchanged} unchanged.`, "success");
}

// ---- Add media root modal (issue #252) -----------------------------------
// Browse-only: the user picks a folder and its asset type here — no label or
// free-text path field anymore. "Add root" lives in this modal beside a
// Cancel button.

const addRootModal = document.getElementById("addRootModalBackdrop");
let addRootPickedPath = null;

function openAddRootModal() {
  addRootPickedPath = null;
  document.getElementById("addRootChosenPath").textContent = "No folder chosen yet";
  document.getElementById("addRootChosenPath").classList.add("muted");
  document.getElementById("addRootAssetType").value = "";
  document.getElementById("addRootConfirmBtn").disabled = true;
  addRootModal.classList.remove("d-none");
  document.getElementById("addRootChooseBtn").focus();
}

function closeAddRootModal() {
  addRootModal.classList.add("d-none");
}

document.getElementById("openAddRootBtn").addEventListener("click", openAddRootModal);
document.getElementById("addRootModalClose").addEventListener("click", closeAddRootModal);
document.getElementById("addRootCancelBtn").addEventListener("click", closeAddRootModal);
addRootModal.addEventListener("click", event => {
  if (event.target === addRootModal) closeAddRootModal();
});

document.getElementById("addRootChooseBtn").addEventListener("click", async () => {
  const picked = await invoke("pick_folder_path").catch(() => null);
  if (!picked) return;
  addRootPickedPath = picked;
  const chosen = document.getElementById("addRootChosenPath");
  chosen.textContent = picked;
  chosen.classList.remove("muted");
  document.getElementById("addRootConfirmBtn").disabled = false;
});

document.getElementById("addRootConfirmBtn").addEventListener("click", async event => {
  if (!addRootPickedPath) return;
  if (!document.getElementById("addRootAssetType").value) {
    showToast("Choose an asset type first.", "warning");
    return;
  }
  const button = event.currentTarget;
  button.disabled = true;
  try {
    const result = await invoke("add_media_root", {
      label: "",
      path: addRootPickedPath,
      assetType: document.getElementById("addRootAssetType").value,
    });
    closeAddRootModal();
    await refreshMediaRoots();
    describeRootChange(result);
  } catch (err) {
    showToast(String(err), "error");
    button.disabled = false;
  }
});

// ---- SMB network roots -----------------------------------------------------

const networkRootModal = document.getElementById("networkRootModalBackdrop");

function openNetworkRootModal() {
  networkRootModal.classList.remove("d-none");
  document.getElementById("networkRootLabel").focus();
}

function closeNetworkRootModal() {
  networkRootModal.classList.add("d-none");
}

document.getElementById("connectNetworkRootBtn").addEventListener("click", openNetworkRootModal);
document.getElementById("onboardNetworkRootBtn").addEventListener("click", openNetworkRootModal);
document.getElementById("networkRootModalClose").addEventListener("click", closeNetworkRootModal);
document.getElementById("networkRootCancelBtn").addEventListener("click", closeNetworkRootModal);
networkRootModal.addEventListener("click", event => {
  if (event.target === networkRootModal) closeNetworkRootModal();
});

document.getElementById("networkRootConnectBtn").addEventListener("click", async event => {
  const button = event.currentTarget;
  const wasOnboarding = !document.getElementById("onboardFolderView").classList.contains("d-none");
  button.disabled = true;
  try {
    const result = await invoke("connect_smb_root", {
      label: document.getElementById("networkRootLabel").value,
      server: document.getElementById("networkRootServer").value,
      share: document.getElementById("networkRootShare").value,
      username: document.getElementById("networkRootUsername").value || null,
      assetType: document.getElementById("networkRootAssetType").value || null,
    });
    for (const id of ["networkRootLabel", "networkRootServer", "networkRootShare", "networkRootUsername", "networkRootAssetType"]) {
      document.getElementById(id).value = "";
    }
    closeNetworkRootModal();
    showToast("SMB share connected.", "success");
    if (wasOnboarding) {
      await enterDashboard();
    } else {
      await Promise.all([refreshMediaRoots(), refreshMediaRootHealth()]);
      describeRootChange(result);
    }
  } catch (err) {
    showToast(String(err), "error", { duration: 9000 });
  } finally {
    button.disabled = false;
  }
});
