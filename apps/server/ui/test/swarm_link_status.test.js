#!/usr/bin/env node
//
// The Swarm tab must say so when the media server can't reach the SWARM
// service. Before this existed the link was attempted once at startup and a
// failure was silent: SWARM-paired TVs showed the server offline for days
// while every screen here looked healthy.
//
// Loads the real index.html and scripts in jsdom (same approach as
// boot_order.test.js) with an invoke() stub whose link status the test
// controls, then drives refreshSwarmLinkStatus() -- the same function the
// 10-second poll calls -- through unreachable -> connected -> unreachable ->
// not linked.
//
// Run: cd apps/server/ui/test && npm install && npm test

const { JSDOM } = require("jsdom");
const fs = require("fs");
const path = require("path");

const UI_DIR = path.join(__dirname, "..");
const invokeCalls = [];

const baseStatus = {
  state: "not_linked", base_url: null, last_error: null,
  failing_since: null, connected_since: null, attempts: 0, signaling: false,
};
let linkStatus = { ...baseStatus };

function invokeStub(command) {
  invokeCalls.push(command);
  switch (command) {
    case "get_settings":
      return {
        media_roots: [{ label: "test", path: "/tmp/swarm-link-status-test" }],
        has_tmdb_key: false,
        streaming_upload_budget_enabled: true,
        artwork_disk_cache_enabled: true,
        local_transcription_enabled: false,
        transcription_pause_while_streaming: true,
        transcription_skip_if_subtitles_exist: false,
        mcp_port: 7890,
        mcp_access_token: null,
        auto_library_watch_enabled: true,
      };
    case "get_swarm_link_status":
      return linkStatus;
    case "forget_swarm_link":
      linkStatus = { ...baseStatus };
      return null;
    case "get_swarm_link":
      return null;
    case "notification_count":
    case "client_error_count":
      return 0;
    default:
      // Most other commands feed list renderers on tabs this test never
      // opens; an empty list is the safest neutral answer.
      return [];
  }
}

class FakeIntersectionObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const tick = (ms = 30) => new Promise((resolve) => setTimeout(resolve, ms));

async function main() {
  const html = fs.readFileSync(path.join(UI_DIR, "index.html"), "utf8");
  const failures = [];
  const expect = (condition, message) => { if (!condition) failures.push(message); };

  // Untrusted text: an address and an error message come from saved state and
  // the network, so they must be shown as text, never parsed as markup.
  linkStatus = {
    ...baseStatus,
    state: "unreachable",
    base_url: "http://192.168.0.235:8080/<b id=\"injected\">x</b>",
    last_error: "<img id=\"injected-img\" src=x onerror=alert(1)>",
    failing_since: Math.floor(Date.now() / 1000) - 3 * 3600,
    attempts: 42,
  };

  const dom = new JSDOM(html, {
    url: `file://${path.join(UI_DIR, "index.html")}`,
    runScripts: "dangerously",
    resources: "usable",
    pretendToBeVisual: true,
    beforeParse(window) {
      window.__TAURI__ = {
        core: { invoke: (command, args) => Promise.resolve(invokeStub(command, args)) },
        event: { listen: () => Promise.resolve(() => {}) },
      };
      window.IntersectionObserver = FakeIntersectionObserver;
      window.HTMLCanvasElement.prototype.getContext = () => ({
        setTransform() {}, clearRect() {}, beginPath() {}, moveTo() {}, lineTo() {},
        stroke() {}, fill() {}, closePath() {}, arc() {}, fillText() {},
      });
      window.navigator.clipboard = { writeText: () => Promise.resolve() };
    },
  });
  dom.window.console.error = () => {};
  await tick(250); // boot() -> enterDashboard() -> first poll

  const { document, refreshSwarmLinkStatus } = dom.window;
  const badge = () => document.getElementById("swarmLinkBadge");
  const box = () => document.getElementById("swarmLinkStatus");
  const toasts = (type) => [...document.querySelectorAll(`.toast-${type} .toast-message`)].map((el) => el.textContent);

  // 1. Unreachable at startup is reported, loudly and once.
  expect(typeof refreshSwarmLinkStatus === "function", "Expected swarm.js to define refreshSwarmLinkStatus().");
  expect(!badge().classList.contains("d-none"), "Expected the Swarm tab badge to show while the SWARM service is unreachable.");
  expect(!box().classList.contains("d-none"), "Expected the status block to show while unreachable.");
  expect(box().textContent.includes("SWARM service unreachable"), "Expected the status block to say the service is unreachable.");
  expect(box().textContent.includes("SWARM-paired TVs show this server as offline"), "Expected the block to say what the outage means for TVs.");
  expect(box().textContent.includes("<b id=\"injected\">"), "Expected the saved address to be shown as literal text.");
  expect(!box().querySelector("#injected, #injected-img"), "The address/error text was parsed as HTML -- it must go through esc().");
  expect(box().textContent.includes("Unreachable for 3 h"), `Expected the outage length to be shown, got: ${box().textContent}`);
  expect(toasts("warning").filter((t) => t.includes("Can't reach the SWARM service")).length === 1,
    "Expected exactly one warning toast for the initial outage.");

  // 2. A poll that finds the same state must not toast again.
  await refreshSwarmLinkStatus();
  await refreshSwarmLinkStatus();
  expect(toasts("warning").filter((t) => t.includes("Can't reach the SWARM service")).length === 1,
    "Polling the same unreachable state must not repeat the warning toast.");

  // 3. Recovery clears the badge and says so.
  linkStatus = { ...baseStatus, state: "connected", signaling: true, connected_since: 1 };
  await refreshSwarmLinkStatus();
  expect(badge().classList.contains("d-none"), "Expected the badge to hide once connected.");
  expect(box().textContent.includes("Connected to the SWARM service"), "Expected the block to confirm the connection.");
  expect(toasts("success").some((t) => t.includes("Reconnected to the SWARM service")), "Expected a success toast on recovery.");

  // 4. Forget calls the backend and returns the tab to "not linked".
  linkStatus = { ...baseStatus, state: "unreachable", base_url: "http://192.168.0.235:8080", last_error: "connection refused", failing_since: Math.floor(Date.now() / 1000) - 5 };
  await refreshSwarmLinkStatus();
  const forget = document.getElementById("forgetSwarmLinkBtn");
  expect(!!forget, "Expected a Forget button while the service is unreachable.");
  if (forget) {
    invokeCalls.length = 0;
    forget.click();
    await tick(60);
    expect(invokeCalls.includes("forget_swarm_link"), "Expected the Forget button to invoke forget_swarm_link.");
  }

  // 5. Nothing configured is healthy and must stay quiet.
  await refreshSwarmLinkStatus();
  expect(badge().classList.contains("d-none"), "Expected no badge when no SWARM service is configured.");
  expect(box().classList.contains("d-none"), "Expected no status block when no SWARM service is configured.");

  dom.window.close();

  if (failures.length > 0) {
    console.error("FAIL: swarm_link_status.test.js\n  " + failures.join("\n  "));
    process.exitCode = 1;
  } else {
    console.log("PASS: swarm_link_status.test.js -- an unreachable SWARM service is shown once, escaped, and clears on recovery.");
  }
}

main().catch((err) => {
  console.error("FAIL: swarm_link_status.test.js threw while running the test itself:", err);
  process.exitCode = 1;
});
