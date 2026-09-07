//! Shared setup for the backend UAT suite: a real `AppState` behind a mocked
//! Tauri runtime, with its own isolated temp data dir and its own unique
//! QUIC/HTTP bind ports (see `AppState::test_data_dir`/`test_bind_override`
//! in `gui.rs`) so tests can safely run concurrently in the same process —
//! Rust's default test harness runs `#[test]`s on multiple threads, and
//! `SWARM_PEER_BIND`/`SWARM_HTTP_MEDIA_BIND` are process-global env vars, so
//! per-test isolation has to happen through `AppState` fields, not env vars.

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Mutex;

use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
use tauri::Manager;

use crate::AppState;

/// A running test instance: the mock Tauri app (keeps the isolated temp dir
/// alive for the harness's lifetime via `_data_dir`) plus a ready-to-use
/// `AppHandle<MockRuntime>` for calling `#[tauri::command]` functions
/// directly, bypassing Tauri's IPC/ACL layer entirely (see `mod.rs`'s doc
/// comment for why).
pub struct TestApp {
    pub app: tauri::App<MockRuntime>,
    _data_dir: tempfile::TempDir,
}

impl TestApp {
    pub fn handle(&self) -> tauri::AppHandle<MockRuntime> {
        self.app.handle().clone()
    }
}

/// Ports above the ephemeral range's usual OS-assigned floor and far from
/// SWARM's real defaults (8543/8546), so a concurrently-running dev instance
/// of the app never collides with a test run.
static NEXT_PORT: AtomicU16 = AtomicU16::new(23000);
static PORT_LOCK: Mutex<()> = Mutex::new(());

fn next_port_pair() -> (std::net::SocketAddr, std::net::SocketAddr, std::net::SocketAddr) {
    let _guard = PORT_LOCK.lock().unwrap();
    let peer_port = NEXT_PORT.fetch_add(3, Ordering::Relaxed);
    let http_port = peer_port + 1;
    let http_tls_port = peer_port + 2;
    (
        format!("127.0.0.1:{peer_port}").parse().unwrap(),
        format!("127.0.0.1:{http_port}").parse().unwrap(),
        format!("127.0.0.1:{http_tls_port}").parse().unwrap(),
    )
}

/// A bare app: real isolated data dir, no media root configured yet. Enough
/// for settings-only commands (`add_media_root`, `list_media_roots`, MCP
/// token settings, ...) that never touch `AppState::core` — those write
/// straight to `settings.json` under the test data dir and never bind a
/// network port.
pub fn test_app() -> TestApp {
    let data_dir = tempfile::tempdir().expect("create temp data dir");
    let (bind, http_media_bind, http_media_tls_bind) = next_port_pair();
    let app = mock_builder()
        .manage(AppState {
            core: tokio::sync::OnceCell::new(),
            library_maintenance_cancel: tokio::sync::Mutex::new(None),
            _sleep_inhibitor: None,
            test_data_dir: Some(data_dir.path().to_path_buf()),
            test_bind_override: Some((bind, http_media_bind, http_media_tls_bind)),
            last_scrape_issues: tokio::sync::Mutex::new(Vec::new()),
            reorg_plans: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            next_reorg_plan_id: std::sync::atomic::AtomicU64::new(1),
        })
        .build(mock_context(noop_assets()))
        .expect("build mock tauri app");
    TestApp {
        app,
        _data_dir: data_dir,
    }
}

/// A real, empty, on-disk directory suitable as a media root path — real
/// filesystem, just no media files in it unless the caller adds some.
pub fn empty_media_root_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temp media root dir")
}

/// A test app with one real, configured media root, with a real
/// `ServerCore` already started and its startup background scan (see
/// `ServerCore::start`'s doc comment: the initial scan runs in the
/// background, not awaited by `start()` itself) already settled. Without
/// waiting for it here, that background scan and a test's own explicit
/// `rescan()` call race for `ServerCore`'s internal scan lock — whichever
/// wins first is unpredictable, and a `rescan()` racing ahead of the
/// still-pending startup scan misses reconciliation work the startup scan
/// then finishes moments later, after the test already inspected the
/// (wrong) result. Settling it here once means every subsequent `rescan()`
/// call in a test is the only scan running, with a deterministic result.
/// Returns the (still just-scanned) root directory so the caller can drop
/// fixture files into it before triggering a further scan.
pub async fn test_app_with_media_root() -> (TestApp, tempfile::TempDir) {
    let test_app = test_app();
    let app = test_app.handle();
    let root_dir = empty_media_root_dir();
    crate::add_media_root(
        app.clone(),
        app.state(),
        "Movies".to_string(),
        root_dir.path().to_string_lossy().to_string(),
        Some("movies".to_string()),
    )
    .await
    .expect("add_media_root should succeed in test_app_with_media_root");
    let core = app
        .state::<AppState>()
        .core(&app)
        .await
        .expect("core should start against a real, configured media root");
    core.wait_for_scan()
        .await
        .expect("the startup background scan should complete cleanly");
    (test_app, root_dir)
}
