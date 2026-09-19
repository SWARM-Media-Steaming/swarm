#!/usr/bin/env node
//
// The Swarm tab must tell people when remote access is broken *and something
// they use is affected* -- and stay silent otherwise. Before this existed the
// link was attempted once at startup and a failure was invisible: paired TVs
// showed the server offline for days while every screen here looked healthy.
// The first fix warned about every unreachable service, which then alarmed a
// user whose only unreachable service was a stale address nothing used. So the
// rule under test is: badge, warning and toast key off `needs_attention`
// (link down AND a paired device affected); an unused outage is one quiet line.
//
// Loads the real index.html and scripts in jsdom (same approach as
// boot_order.test.js) with an invoke() stub whose link status the test
// controls, then drives refreshSwarmLinkStatus() -- the function the 10-second
// poll calls -- through the states.
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
  dependents: [], needs_attention: false,
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
    case "retry_swarm_link":
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

  // The reported situation: a dead saved address and nothing paired through
  // SWARM. Present from the very first poll, i.e. at boot.
  const outageStart = Math.floor(Date.now() / 1000) - 3 * 3600;
  linkStatus = {
    ...baseStatus,
    state: "unreachable",
    base_url: "http://192.168.0.235:8080/<b id=\"injected\">x</b>",
    last_error: "<img id=\"injected-img\" src=x onerror=alert(1)>",
    failing_since: outageStart,
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

  expect(typeof refreshSwarmLinkStatus === "function", "Expected swarm.js to define refreshSwarmLinkStatus().");
  const warnings = () => toasts("warning");
  const visibleText = () => box().textContent.replace(/\s+/g, " ").trim();

  // 1. An outage nobody is affected by is NOT a problem: no badge, no toast,
  //    no warning, and no jargon in what is said.
  expect(badge().classList.contains("d-none"), "An unused outage must not put a badge on the Swarm tab.");
  expect(warnings().length === 0, `An unused outage must not toast; got: ${warnings().join(" | ")}`);
  expect(!box().querySelector(".link-status-warn"), "An unused outage must not render the warning block.");
  expect(visibleText().includes("Nothing is using it, so nothing is affected"), `Expected a plain reassurance, got: ${visibleText()}`);
  expect(!/rendezvous|stun|swarm service/i.test(box().querySelector(".muted").textContent), "The headline must not use infrastructure jargon.");
  // The technical detail is still there for whoever wants it, and escaped.
  expect(box().textContent.includes("<b id=\"injected\">"), "Expected the saved address to be shown as literal text under Details.");
  expect(!box().querySelector("#injected, #injected-img"), "The address/error text was parsed as HTML -- it must go through esc().");
  expect(box().textContent.includes("Not connected for 3 h"), `Expected the outage length under Details, got: ${visibleText()}`);

  // 2. Someone IS affected: now it is a warning, it names them, and it says
  //    what still works.
  linkStatus = { ...linkStatus, dependents: ["Michael's TV", "<i id=\"injected-name\">Den</i>"], needs_attention: true };
  await refreshSwarmLinkStatus();
  expect(!badge().classList.contains("d-none"), "Expected the Swarm tab badge once a paired device is affected.");
  expect(!!box().querySelector(".link-status-warn"), "Expected the warning block once a paired device is affected.");
  expect(visibleText().includes("Remote access is offline"), `Expected the plain headline, got: ${visibleText()}`);
  expect(visibleText().includes("Michael's TV and <i id=\"injected-name\">Den</i> can't connect from outside your network"), `Expected the affected devices by name (escaped), got: ${visibleText()}`);
  expect(!box().querySelector("#injected-name"), "A device name was parsed as HTML -- it must go through esc().");
  expect(visibleText().includes("TVs at home still work"), "Expected the warning to say what still works.");
  expect(warnings().filter((t) => t.includes("Remote access is offline")).length === 1, "Expected exactly one warning toast when the outage starts to matter.");

  // 3. Polling the same state must not repeat the toast.
  await refreshSwarmLinkStatus();
  await refreshSwarmLinkStatus();
  expect(warnings().filter((t) => t.includes("Remote access is offline")).length === 1, "Polling the same state must not repeat the warning toast.");

  // 4. "Try again now" reaches the backend.
  invokeCalls.length = 0;
  document.getElementById("retrySwarmLinkBtn").click();
  await tick(60);
  expect(invokeCalls.includes("retry_swarm_link"), "Expected Try again now to invoke retry_swarm_link.");

  // 5. Recovery clears the badge and says so.
  linkStatus = { ...baseStatus, state: "connected", signaling: true, connected_since: 1, dependents: ["Michael's TV"] };
  await refreshSwarmLinkStatus();
  expect(badge().classList.contains("d-none"), "Expected the badge to hide once connected.");
  expect(visibleText().includes("Remote access is on"), "Expected the block to confirm remote access is on.");
  expect(toasts("success").some((t) => t.includes("Remote access is back")), "Expected a success toast on recovery.");

  // 6. Turning it off calls the backend and returns the tab to "not set up".
  linkStatus = { ...baseStatus, state: "unreachable", base_url: "http://192.168.0.235:8080", last_error: "connection refused", failing_since: Math.floor(Date.now() / 1000) - 5, dependents: ["Michael's TV"], needs_attention: true };
  await refreshSwarmLinkStatus();
  const forget = document.getElementById("forgetSwarmLinkBtn");
  expect(!!forget, "Expected a Turn off remote access button under Details.");
  if (forget) {
    invokeCalls.length = 0;
    forget.click();
    await tick(60);
    expect(invokeCalls.includes("forget_swarm_link"), "Expected the button to invoke forget_swarm_link.");
  }

  // 7. Nothing configured is healthy and must stay quiet.
  await refreshSwarmLinkStatus();
  expect(badge().classList.contains("d-none"), "Expected no badge when remote access is not set up.");
  expect(box().classList.contains("d-none"), "Expected no status block when remote access is not set up.");

  dom.window.close();

  if (failures.length > 0) {
    console.error("FAIL: swarm_link_status.test.js\n  " + failures.join("\n  "));
    process.exitCode = 1;
  } else {
    console.log("PASS: swarm_link_status.test.js -- an unused outage is silent; an affecting one is named, escaped, toasted once, and clears on recovery.");
  }
}

main().catch((err) => {
  console.error("FAIL: swarm_link_status.test.js threw while running the test itself:", err);
  process.exitCode = 1;
});
