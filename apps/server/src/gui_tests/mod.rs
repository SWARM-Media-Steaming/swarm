//! Backend UAT suite for the media server: real `#[tauri::command]` handlers
//! (defined in `gui.rs`, the parent module) invoked directly against a real,
//! isolated `AppState` — real SQLite, real filesystem, real `ServerCore` —
//! behind `tauri::test::mock_builder`'s mocked runtime.
//!
//! Two things this deliberately does NOT do, and why:
//! - Drive the real native UI. Tauri's official WebDriver story
//!   (`tauri-driver`) has no macOS backend, and a macOS Accessibility-API
//!   spike (AppleScript/System Events) proved unreliable enough (clicks not
//!   reaching the WKWebView's DOM handlers, non-deterministic tree
//!   enumeration) to not be worth pursuing further right now. This suite is
//!   backend/API coverage only — real user-visible UI flows still need
//!   either a human or a future, more reliable automation path.
//! - Call commands through Tauri's simulated IPC (`get_ipc_response`). Tauri
//!   2's ACL/capability manifest for app-level (non-plugin) commands isn't
//!   present under a bare `mock_context()`, and reconstructing it is Tauri
//!   framework plumbing, not this app's logic. Calling the `async fn`
//!   command handlers directly (via a real `AppHandle<MockRuntime>`)
//!   exercises the same business logic without fighting that layer.
//!
//! Read `swarm-media-server-uat-tests` (skill) before changing test logic
//! here — same standing "don't modify without explicit permission" policy
//! the project's other closed-loop suites (`swarm-e2e-suite-lockdown`) use.
//! Genuine infra bugs (a flaky helper, a real product bug the tests
//! surface) are fair game to fix without asking; the scenarios themselves
//! and their pass/fail criteria are not.

mod harness;

mod media_root_lifecycle;
mod library_scan;
mod tv_pairing;
mod notifications_and_errors;
mod metadata_editing;
mod mcp_tokens;
mod transcoding_settings;
mod ai_integration;
mod software_update;
