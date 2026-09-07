const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

document.getElementById("hideToTrayBtn")?.addEventListener("click", async () => {
  try {
    await invoke("hide_to_tray");
  } catch (error) {
    showToast(`Could not hide SWARM: ${error}`, "error");
  }
});

function esc(v) {
  return String(v ?? "").replace(/[&<>"']/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
}

// ---- toast notifications ----------------------------------------------------
// Every success/warning/error message in the app surfaces as a toast rather
// than scattered inline text — see the doc comment on #toastStack in
// style.css. `type` picks the color/icon: "success" (green), "warning"
// (yellow), "error" (red, the default duration is longer since it's more
// likely worth reading in full before it disappears). Errors are never
// silently swallowed — every catch block in this app should route here.
const TOAST_ICONS = { success: "bi-check-circle-fill", warning: "bi-exclamation-triangle-fill", error: "bi-x-circle-fill", progress: "bi-arrow-repeat" };

function showToast(message, type = "success", opts = {}) {
  const stack = document.getElementById("toastStack");
  if (!stack) return; // toast fired before the DOM's ready (shouldn't happen) — never throw over a notification
  const duration = opts.duration ?? (type === "error" ? 7000 : 4500);
  const toast = document.createElement("div");
  toast.className = `toast toast-${type}`;
  toast.innerHTML =
    `<i class="bi ${TOAST_ICONS[type] || TOAST_ICONS.success} toast-icon"></i>` +
    `<span class="toast-message"></span>` +
    `<button class="toast-close" aria-label="Dismiss"><i class="bi bi-x"></i></button>`;
  toast.querySelector(".toast-message").textContent = message;
  const remove = () => {
    toast.classList.add("toast-out");
    setTimeout(() => toast.remove(), 150);
  };
  toast.querySelector(".toast-close").addEventListener("click", remove);
  stack.appendChild(toast);
  if (duration > 0) setTimeout(remove, duration);
  return toast;
}

function dismissToast(toast) {
  if (toast && toast.isConnected) toast.remove();
}

function stat(label, value, mono, infoId) {
  const clickable = infoId ? ` data-info="${infoId}" tabindex="0" role="button" class="stat stat-clickable"` : ` class="stat"`;
  const icon = infoId ? ` <i class="bi bi-info-circle info-affordance"></i>` : "";
  return `<div${clickable}><div class="label">${esc(label)}${icon}</div><div class="value${mono ? " mono" : ""}">${esc(value)}</div></div>`;
}

// ---- info modal ---------------------------------------------------------
// One shared "what am I looking at" popup for the whole app, opened by
// clicking (or Enter/Space-ing, for keyboard users) any element carrying
// data-info="<topicId>" — About tab's flow steps/feature tiles/badges,
// Details tab's stat tiles and card headers, AI tab's MCP heading and tool
// list. A single registry + single modal surface, same reasoning
// showToast() is one shared surface instead of bespoke status text per
// call site. Delegation (one listener on document), not a listener per
// element, since triggers live in both static markup (About, AI) and
// markup rebuilt on every refresh (Details' stat grid) — nothing needs to
// remember to re-wire anything after a re-render.
const INFO_TOPICS = {
  entries: {
    icon: "bi-collection-play", title: "Entries",
    body: "Every movie, episode, and track SWARM has found and catalogued across your media roots.",
  },
  "library-size": {
    icon: "bi-hdd-fill", title: "Library size",
    body: "The combined size on disk of every file in your library, across every media root.",
  },
  "upload-budget": {
    icon: "bi-speedometer2", title: "Streaming upload budget",
    body: "The share of your internet upload speed reserved for streaming. SWARM measures it automatically using a longer upload sample. Disable the limit if you want remote streams to use the full connection; local-network streams are never limited.",
  },
  "active-sessions": {
    icon: "bi-play-circle-fill", title: "Active playback sessions",
    body: "How many clients are streaming from this server right now.",
  },
  "streaming-bandwidth": {
    icon: "bi-graph-up", title: "Streaming bandwidth",
    body: "Total data actually being sent to every connected client combined, sampled every 5 seconds. The graph keeps the last 60 minutes of history so you can see how usage changes as more clients join.",
  },
  transcoding: {
    icon: "bi-cpu", title: "Transcoding",
    body: "When a client can't play a file directly, the server runs ffmpeg to convert it on the fly (resizing video, re-encoding audio, and burning in the right subtitle track). That work is CPU-heavy. This graph splits the last 60 minutes of CPU use into ffmpeg transcodes and the rest of the server process — which also covers local Whisper subtitle generation — so you can see how streaming and subtitle work translate into load on this machine. The tiles show how many transcodes and direct-play streams are active right now and whether subtitle generation is running. The controls below only take effect when a transcode is actually needed: 'Video encoder' picks between the hardware encoder (fast, near-free CPU on Apple Silicon) and the portable software one — leave it on Auto unless you're working around a driver issue; 'Max transcode resolution' caps output height to save CPU even when a client asks for more; 'HLS segment length' trades startup and rebuffer-recovery speed against a little overhead, and applies to new streams only.",
  },
  "artwork-cache": {
    icon: "bi-images", title: "Artwork cache",
    body: "For media on a slower network share, SWARM can copy artwork to this server the first time a client requests it. Later requests use the local copy. The graph distinguishes new cache fills from cache hits and can be filtered to one client. Cached files refresh after 30 days and are immediately superseded when scraping or a manual artwork change creates a new version. Expand “How the artwork cache works” to see its exact folder on this server.",
  },
  "device-fingerprint": {
    icon: "bi-fingerprint", title: "Device fingerprint",
    body: "A unique hash of a device's security certificate, used so two devices can verify they're really talking to each other and not an impostor.",
    link: "https://en.wikipedia.org/wiki/Public_key_fingerprint", linkLabel: "Learn about certificate fingerprints",
  },
  "media-roots": {
    icon: "bi-folder2-open", title: "Media roots",
    body:
      "The folders SWARM scans — add a local folder or an SMB share from a NAS, and pick an asset type (Movies, TV shows, Music, or Photos & videos) so SWARM knows what to expect there. You can run more than one root, but two roots can't point at the same or an overlapping location.\n\n" +
      "Organise each root the way Plex, Jellyfin, and Kodi do:\n" +
      "• Movies — \"Movie Name (Year)/Movie Name (Year).mkv\", with Featurettes/Trailers/Deleted Scenes folders beside it for extras.\n" +
      "• TV — \"Show Name (Year)/Season 01/Show Name - S01E02.mkv\"; \"S01E02-E03\" multi-episode files and a Specials season are recognised.\n" +
      "• Music — \"Artist/Album/01 Track Title.flac\"; CD1/CD2 disc folders are absorbed automatically.\n" +
      "• Subtitles — a .srt or .vtt next to the video (or in a Subs/ folder), named after it, e.g. \"Movie Name (Year).en.srt\".\n\n" +
      "Older installations may show a Legacy mixed root; new roots always require one specific asset type. The About tab repeats this under \"How to organise your media folders\".",
  },
  "tmdb-scraping": {
    icon: "bi-cloud-download", title: "TMDb scraping",
    body: "TMDb supplies posters, artwork, cast lists, and summaries for movies and TV. Create a free Developer API key at TMDb under Settings → API, then paste the v3 API key or v4 read token here. Music artwork and LRCLIB lyrics are fetched automatically during metadata scraping and do not require an API key.",
    link: "https://www.themoviedb.org/", linkLabel: "Visit TMDb",
  },
  "local-subtitles": {
    icon: "bi-badge-cc-fill", title: "Local subtitle generation",
    body: "SWARM can generate English subtitles locally with Whisper. The first run downloads and verifies a compact model of about 142 MB. Processing can take roughly as long as the video—or considerably longer on older CPUs—and uses sustained CPU. SWARM always pauses this work during library scans, and by default also pauses while anyone is streaming. Work is saved in ten-minute sections and resumes after disabling, closing, or restarting the app. Each generated subtitle is saved next to its source file, named after it with a \"-whisper-english-subtitles.vtt\" suffix, so it travels with the media. Use a movie or episode's Manage panel to generate a subtitle for just that one item, or turn on bulk generation here for the whole library — optionally skipping anything that already has a subtitle.",
    link: "https://github.com/ggerganov/whisper.cpp", linkLabel: "Learn about Whisper.cpp",
  },
  "software-update": {
    icon: "bi-arrow-repeat", title: "Software update",
    body: "New versions of SWARM Server publish automatically after each change passes tests. \"Notify me\" surfaces a message here when one is available and you choose when to install. \"Download automatically\" fetches it in the background and swaps it in place — playback is never interrupted and the new version runs the next time the server restarts. \"Check now\" works in any mode. Builds are signed with a self-signed certificate (not an Apple Developer ID); an in-place update keeps the macOS file-access grants you already gave, but a fresh install from a .dmg still needs one right-click → Open.",
    link: "", linkLabel: "",
  },
  "opensubtitles-downloads": {
    icon: "bi-cloud-arrow-down", title: "Subtitle downloads",
    body: "Use an OpenSubtitles.com API key to search for an existing subtitle for one movie or episode. SWARM downloads it only when you request it, converts it to a TV-compatible format, stores it on this server, and offers it alongside locally generated subtitles during playback. OpenSubtitles applies its own account and daily download limits.",
    link: "https://www.opensubtitles.com/consumers", linkLabel: "Get an OpenSubtitles API key",
  },
  "about-server": {
    icon: "bi-hdd-network-fill", title: "Your server",
    body: "Runs on your own computer, scans your media, and streams files directly to your devices — there's no cloud in between.",
  },
  "about-clients": {
    icon: "bi-tv-fill", title: "Your clients",
    body: "Fire TV today, with more platforms planned. A native app that connects straight to your server to browse and play your library.",
  },
  "about-secure": {
    icon: "bi-shield-lock-fill", title: "Secure by design",
    body: "Every device presents a certificate and proves who it is before it can connect — mutual verification over TLS 1.3, the same encryption standard used by online banking.",
    link: "https://en.wikipedia.org/wiki/Transport_Layer_Security", linkLabel: "Learn about TLS",
  },
  "about-no-cloud": {
    icon: "bi-cloud-slash-fill", title: "No cloud, ever",
    body: "Your files are never uploaded anywhere. Streaming happens directly between your own devices, so no third party ever stores or sees your media.",
  },
  "about-invite-only": {
    icon: "bi-key-fill", title: "Invite only",
    body: "New devices join with a short one-time code you generate yourself — there's no public sign-up, and you decide exactly who's allowed in.",
  },
  "about-merged-library": {
    icon: "bi-diagram-3-fill", title: "One library, everywhere",
    body: "Run more than one SWARM server? Every device in your swarm sees one combined library — the same file on two servers is merged automatically instead of showing up twice.",
  },
  "about-direct": {
    icon: "bi-wifi", title: "Direct device-to-device",
    body: "Every stream travels straight from your server to your client over a private connection — no third-party relay ever sits in the middle.",
    link: "https://en.wikipedia.org/wiki/Peer-to-peer", linkLabel: "Learn about peer-to-peer",
  },
  "mcp-protocol": {
    icon: "bi-stars", title: "What is MCP?",
    body: "The Model Context Protocol is an open standard that lets an AI assistant talk directly to outside tools and data. SWARM exposes a small, read-only MCP API so an assistant like Claude can look things up in your library on your behalf.",
    link: "https://modelcontextprotocol.io", linkLabel: "Read the MCP spec",
  },
  "try-asking": {
    icon: "bi-chat-square-text-fill", title: "Ask with an AI tool",
    body: "After you add this MCP Server to an AI tool, ask ordinary questions about your library. The tool chooses the read-only SWARM functions it needs and turns the results into a conversational answer.",
    links: [
      { href: "https://developers.openai.com/codex/", label: "Learn about Codex" },
      { href: "https://claude.ai/", label: "Open Claude" },
    ],
  },
  "tool-search-library": {
    icon: "bi-search", title: "search_library",
    body: "Finds entries in your library by title, kind, genre, rating, or liked status — the same filtering the Media tab's search box uses.",
  },
  "tool-get-entry-details": {
    icon: "bi-info-circle-fill", title: "get_entry_details",
    body: "Looks up everything known about one entry: its full synopsis, cast, rating, and genres.",
  },
  "tool-list-swarm-devices": {
    icon: "bi-diagram-3-fill", title: "list_swarm_devices",
    body: "Lists every device in your swarm and whether it's currently online — the same roster shown on the Swarm tab.",
  },
  "tool-list-client-errors": {
    icon: "bi-bell-fill", title: "list_client_errors",
    body: "Returns recent client-reported problems, like failed playback or an unreachable server — the same list shown on the Notifications tab.",
  },
  "approve-tv": {
    icon: "bi-shield-check", title: "Approve a TV",
    body: "Enter the short-lived code shown on the device here. This one box works no matter how it found this server — locally, over plain HTTP, or through the SWARM service.",
  },
  "lan-network": {
    icon: "bi-broadcast-pin", title: "Local network",
    body: "TVs on the same Wi-Fi or wired network discover this server automatically without a SWARM service. Approve one above, then trusted TVs reconnect directly afterward.",
    link: "https://en.wikipedia.org/wiki/Multicast_DNS", linkLabel: "Learn about mDNS",
  },
  "http-media-device": {
    icon: "bi-wifi", title: "Plain-HTTP devices",
    body: "Some devices can't use SWARM's peer protocol and pair over plain HTTP instead. Approve one above using the same code box; it gets its own access token here, separate from the local network list.",
  },
  "swarm-concept": {
    icon: "bi-diagram-3-fill", title: "Swarm",
    body: "A swarm is a private group of your own devices — servers and clients — that can find and stream from each other away from home. This server automatically creates and manages its own swarm; approve a TV above to add it.",
  },
};

function openInfoModal(topicId) {
  const topic = INFO_TOPICS[topicId];
  const backdrop = document.getElementById("infoModalBackdrop");
  if (!topic || !backdrop) return;
  document.getElementById("infoModalIcon").className = `bi ${topic.icon}`;
  document.getElementById("infoModalTitle").textContent = topic.title;
  document.getElementById("infoModalBody").textContent = topic.body;
  const links = topic.links || (topic.link ? [{ href: topic.link, label: topic.linkLabel || "Learn more" }] : []);
  const linksEl = document.getElementById("infoModalLinks");
  linksEl.replaceChildren(...links.map(item => {
    const link = document.createElement("a");
    link.className = "modal-link";
    link.href = item.href;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    link.innerHTML = `<span>${esc(item.label)}</span><i class="bi bi-box-arrow-up-right"></i>`;
    return link;
  }));
  backdrop.classList.remove("d-none");
  document.getElementById("infoModalClose").focus();
}

function closeInfoModal() {
  document.getElementById("infoModalBackdrop").classList.add("d-none");
}

document.getElementById("infoModalBackdrop").addEventListener("click", (e) => {
  if (e.target.id === "infoModalBackdrop") closeInfoModal();
});
document.getElementById("infoModalClose").addEventListener("click", closeInfoModal);

// A plain `<a target="_blank">` doesn't open the OS's default browser from
// inside this app's Tauri webview the way it would in a real browser tab —
// href/target/rel stay on the element for semantics (hover preview, right-
// click "copy link", screen readers) but the actual navigation is handed off
// to open_external_url (apps/server/src/gui.rs), a thin wrapper around the
// Tauri opener plugin, which is the one thing that actually knows how to ask
// the OS to open a URL in the user's real browser.
document.getElementById("infoModalLinks").addEventListener("click", async (e) => {
  const link = e.target.closest("a");
  if (!link) return;
  e.preventDefault();
  const url = link.href;
  try {
    await invoke("open_external_url", { url });
  } catch (err) {
    showToast(String(err), "error");
  }
});

document.addEventListener("click", (e) => {
  const trigger = e.target.closest("[data-info]");
  if (trigger) openInfoModal(trigger.dataset.info);
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") { closeInfoModal(); return; }
  if (e.key === "Enter" || e.key === " ") {
    const trigger = e.target.closest && e.target.closest("[data-info]");
    if (trigger && trigger === e.target) {
      e.preventDefault();
      openInfoModal(trigger.dataset.info);
    }
  }
});

function show(id) {
  for (const el of document.querySelectorAll("#onboardFolderView, #dashView")) {
    el.classList.toggle("d-none", el.id !== id);
  }
  // Pairs with index.html's inline `body { visibility: hidden; }` guard —
  // see that comment. Idempotent and cheap, so just doing it unconditionally
  // on every show() (not only the first) keeps this single call site as the
  // one place that owns "is it safe to see the page yet" instead of a
  // separate first-call flag to track.
  document.body.style.visibility = "visible";
  // The startup splash (index.html's #splashView) is only ever meant to
  // cover the boot() gap — remove it the first time we know which real view
  // to show, same idempotent reasoning as the visibility line above.
  document.getElementById("splashView")?.remove();
}

// "about" has no refresh*() dispatch below — its tab content is static
// (no invoke() calls, nothing that goes stale), unlike every other tab here.
const TABS = ["media", "details", "swarm", "notifications", "ai", "about"];

function showTab(name) {
  for (const tab of TABS) {
    document.getElementById(`tabPanel-${tab}`).classList.toggle("d-none", tab !== name);
    document.getElementById(`tabBtn-${tab}`).classList.toggle("tab-active", tab === name);
  }
  if (name === "details") refreshDetails();
  if (name === "swarm") refreshSwarm();
  if (name === "notifications") refreshNotifications();
  if (name === "media") refreshMedia();
  if (name === "ai") refreshAi();
}

let mediaRootHealthRefreshInFlight = false;
let mediaRootHealthTimer = null;

async function refreshMediaRootHealth() {
  if (mediaRootHealthRefreshInFlight) return;
  mediaRootHealthRefreshInFlight = true;
  const warning = document.getElementById("mediaRootWarning");
  const copy = document.getElementById("mediaRootWarningText");
  const grantBtn = document.getElementById("mediaRootWarningGrantBtn");
  try {
    const roots = await invoke("get_media_root_health");
    const unavailable = roots.filter(root => !root.available);
    warning.classList.toggle("d-none", unavailable.length === 0);
    const denied = unavailable.filter(root => root.permission_denied);
    grantBtn.classList.toggle("d-none", denied.length === 0);
    if (unavailable.length > 0) {
      const noun = unavailable.length === 1 ? "location is" : "locations are";
      const recoveryPronoun = unavailable.length === 1 ? "it is" : "they are";
      const paths = unavailable
        .map(root => `<span class="media-root-warning-path">${esc(root.path)}</span>`)
        .join(", ");
      if (denied.length === unavailable.length) {
        // Every failure is a macOS permission denial — reconnecting won't
        // help, so tell the user about the one-time grant instead (#196).
        copy.innerHTML = `macOS is blocking SWARM from reading ${unavailable.length === 1 ? "this media location" : "these media locations"}: ${paths}. Open macOS Settings &rarr; Privacy &amp; Security &rarr; Files and Folders (or Full Disk Access), turn on &ldquo;SWARM Server&rdquo;, then Rescan. macOS remembers this once.`;
      } else {
        const healing = unavailable.filter(root => root.auto_reconnect).length;
        const recovery = healing
          ? `SWARM is automatically trying to reconnect ${healing === unavailable.length ? recoveryPronoun : `${healing} network share${healing === 1 ? "" : "s"}`}.`
          : "Reconnect the drive or network share.";
        const permissionNote = denied.length
          ? " Some are blocked by macOS — use Open macOS Settings to grant access once."
          : "";
        copy.innerHTML = `${unavailable.length} configured media ${noun} not readable: ${paths}. ${recovery}${permissionNote} Playback, artwork, and subtitles will resume automatically after the share becomes available.`;
      }
    }
  } catch (_) {
    // Other status surfaces already report backend failures. This banner is
    // specifically for a confirmed inaccessible root.
  } finally {
    mediaRootHealthRefreshInFlight = false;
  }
}

document.getElementById("mediaRootWarningDetailsBtn").addEventListener("click", () => {
  showTab("details");
});

// The Full Disk Access pane covers network volumes, removable drives, and the
// protected user folders in one grant — the single place a user can approve
// SWARM's file access once, before or after macOS's own prompt (#196).
document.getElementById("mediaRootWarningGrantBtn").addEventListener("click", async () => {
  try {
    await invoke("open_external_url", {
      url: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles",
    });
  } catch (err) {
    showToast(String(err), "error");
  }
});

for (const tab of TABS) {
  document.getElementById(`tabBtn-${tab}`).addEventListener("click", () => showTab(tab));
}

// ---- onboarding: media folder ---------------------------------------------

document.getElementById("chooseFolderBtn").addEventListener("click", async () => {
  let progressToast;
  try {
    const assetType = document.getElementById("onboardRootAssetType").value;
    if (!assetType) {
      showToast("Choose an asset type first.", "warning");
      return;
    }
    const path = await invoke("choose_media_folder", { assetType });
    if (path) {
      progressToast = showToast("Starting the media server and scanning your folder…", "progress", { duration: 0 });
      await enterDashboard();
    }
  } catch (err) {
    showToast(String(err), "error");
  } finally {
    dismissToast(progressToast);
  }
});

// ---- boot -------------------------------------------------------------------

// Notification bubble on the Notifications tab (server notifications plus
// client-reported error count) — kept here rather than in notifications.js despite belonging
// conceptually to that tab's feature, purely so it sits next to
// enterDashboard() below, the other thing that touches it at boot.
async function refreshNotificationBadge() {
  const badge = document.getElementById("notificationBadge");
  if (!badge) return;
  try {
    const count = Number(await invoke("notification_count")) || 0;
    badge.textContent = count > 99 ? "99+" : String(count);
    badge.classList.toggle("d-none", count <= 0);
  } catch {
    // Best-effort background poll — a failed check isn't worth a toast every interval.
  }
}

async function enterDashboard() {
  show("dashView");
  showTab("media");
  refreshNotificationBadge();
  refreshMediaRootHealth();
  setInterval(refreshNotificationBadge, 30000);
  if (!mediaRootHealthTimer) {
    mediaRootHealthTimer = setInterval(refreshMediaRootHealth, 10000);
  }
}

async function boot() {
  const settings = await invoke("get_settings");
  if (!settings.media_roots || settings.media_roots.length === 0) {
    show("onboardFolderView");
    return;
  }
  await enterDashboard();
}

// Real bug, found live, twice: boot() (via enterDashboard() -> showTab())
// calls functions defined in details.js/swarm.js/media.js — every one of
// which loads *after* this file in index.html. Calling boot() unconditionally
// at this file's own top level, the way this used to work, is a genuine
// race, not a one-off fluke: each classic `<script>` tag gets a microtask
// checkpoint after it finishes running, and if invoke("get_settings")'s IPC
// round trip happens to resolve before the browser has fetched/parsed/run
// the remaining three script tags, boot()'s continuation calls a function
// that doesn't exist yet — first hit as `refreshErrorBadge` undefined, then
// again as `refreshDetails` undefined, both eventually caught by this same
// try/catch and misread as "settings didn't persist" (the catch's own
// fallback is to show onboarding) rather than what actually happened.
// DOMContentLoaded fixes the whole class at once, not just whichever
// function happened to race last: it only ever fires after every classic
// script in the document — all four files here — has finished executing,
// so nothing boot() reaches can possibly still be undefined by the time it
// runs. Regression test: apps/server/ui/test/boot_order.test.js.
document.addEventListener("DOMContentLoaded", () => {
  boot().catch(err => {
    showToast(String(err), "error");
    show("onboardFolderView");
  });
});
