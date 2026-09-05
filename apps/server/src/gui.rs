//! Tauri desktop shell for the SWARM media server (feature `gui`).
//!
//! Thin windowing over [`swarm_server::ServerCore`]: the webview UI (in
//! `ui/`) calls the commands below via `window.__TAURI__.core.invoke`.
//! First-run onboarding picks a media folder (persisted to
//! `<app data dir>/settings.json`) before a core can start; joining a swarm
//! is a separate, skippable step reachable again later from the Swarm card.
//! Adding/removing a media root after the core has started takes effect
//! immediately (see `AppState::apply_live_roots` and
//! `ServerCore::update_media_roots`) — the QUIC listener itself is never
//! torn down, only the shared `SharedRootResolver` both the core and its
//! `MediaService` hold is swapped and a scan run against the new set.

mod ai;
mod mcp;
mod reorganize;
mod settings;

use rand::RngCore;
use settings::{AiProviderSetting, MediaRootHealth, MediaRootSetting, RootAssetType, Settings};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use swarm_core::peer::MediaKind;
use swarm_media::roots::MediaRoot;
use swarm_media::scan::ScanProgressEvent;
use swarm_media::scrape::{BulkScrapeReport, ScrapeConfig, ScrapeIssue, ScrapeProgressEvent};
use swarm_server::{
    ScanState, ServerConfig, ServerCore, ServerError, ServerStatus, TokenStoreMode,
};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{Mutex, OnceCell};

struct AppState {
    core: OnceCell<Arc<ServerCore>>,
    /// Present only while the user-triggered consolidated library workflow
    /// is active. The separate cancel command flips this shared token.
    library_maintenance_cancel: Mutex<Option<Arc<AtomicBool>>>,
    // The OS releases this process-scoped power assertion when the state is
    // dropped, which only happens when the user actually quits the app (not
    // when the main window is hidden to the tray).
    _sleep_inhibitor: Option<keepawake::KeepAwake>,
    /// Test-only override for `app_data_dir()`. `mock_context()`'s
    /// identifier defaults to empty, so every test would otherwise resolve
    /// to the same shared OS path and corrupt each other's state when
    /// Rust's test runner runs them concurrently in the same process — this
    /// gives each test's own `AppState` instance a genuinely unique temp
    /// directory instead. Always `None` in production.
    test_data_dir: Option<PathBuf>,
    /// Test-only override for the QUIC/HTTP-media/HTTP-media-TLS bind
    /// addresses `core()` otherwise reads from
    /// `SWARM_PEER_BIND`/`SWARM_HTTP_MEDIA_BIND`/`SWARM_HTTP_MEDIA_TLS_BIND`
    /// (which default to fixed ports). Those env vars are process-global, so
    /// two tests that started a real `ServerCore` concurrently in the same
    /// test binary would race to bind the same ports; this gives each
    /// test's `AppState` its own unique triple instead. Always `None` in
    /// production.
    test_bind_override: Option<(std::net::SocketAddr, std::net::SocketAddr, std::net::SocketAddr)>,
    /// Issues from the most recent `run_scrape` call, for the AI tab's scan
    /// assist to offer help on — see `ai_scrape_assist`. Deliberately
    /// in-memory only (lost on restart): same "not a persisted queryable
    /// table" status the underlying `BulkScrapeReport::issues` already had
    /// before this feature existed, just kept around one call longer.
    last_scrape_issues: Mutex<Vec<ScrapeIssue>>,
    /// Reorganize plans proposed by `ai_reorganize_scan`, awaiting the
    /// user's approve/reject — see `reorganize.rs`. In-memory only: a plan
    /// is a short-lived review artifact, not library state, so it doesn't
    /// need to survive a restart any more than an unconfirmed dialog would.
    reorg_plans: Mutex<HashMap<u64, StoredReorgPlan>>,
    next_reorg_plan_id: AtomicU64,
}

struct StoredReorgPlan {
    plan: reorganize::ReorgPlan,
    root_path: PathBuf,
    status: &'static str,
    apply_outcome: Option<reorganize::ApplyOutcome>,
}

fn acquire_sleep_inhibitor() -> Option<keepawake::KeepAwake> {
    let combined = keepawake::Builder::default()
        // Let the display turn off, but keep the server and network stack
        // running through idle and explicit sleep requests (including lid
        // close where the operating system permits applications to block it).
        .idle(true)
        .sleep(true)
        .reason("Keep the SWARM media server available")
        .app_name("SWARM Server")
        .app_reverse_domain("app.swarm.server")
        .create();
    match combined {
        Ok(inhibitor) => Some(inhibitor),
        Err(error) => {
            eprintln!("SWARM could not block explicit system sleep: {error}");
            // Explicit sleep blocking is more restricted than ordinary idle
            // sleep on several platforms. Preserve the widely-supported idle
            // assertion when the stronger request is unavailable.
            keepawake::Builder::default()
                .idle(true)
                .reason("Keep the SWARM media server available")
                .app_name("SWARM Server")
                .app_reverse_domain("app.swarm.server")
                .create()
                .map_err(|fallback_error| {
                    eprintln!("SWARM could not prevent idle sleep: {fallback_error}");
                })
                .ok()
        }
    }
}

fn app_data_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    if let Some(state) = app.try_state::<AppState>() {
        if let Some(dir) = &state.test_data_dir {
            return Ok(dir.clone());
        }
    }
    app.path().app_data_dir().map_err(|e| e.to_string())
}

/// Installs the process-wide `tracing` subscriber. Nothing did this before
/// (every `tracing::info!/warn!/error!` call in this crate silently went
/// nowhere, with no subscriber ever attached) — this is what makes
/// `<app data dir>/logs/server.log` exist at all, which the closed-loop TV
/// UAT suite's failure-evidence bundles depend on. Mirrors
/// `apps/stun-server/src/main.rs`'s explicit (not auto-detected) subscriber
/// setup: same reasoning applies here — see that file's comment.
///
/// The returned `WorkerGuard` must stay alive for the life of the process
/// (dropping it stops flushing the non-blocking writer) but nothing else in
/// this file holds long-lived globals like this, so it's deliberately
/// leaked rather than threaded through `AppState` for a single `main`-scoped
/// value.
fn init_logging(app: &tauri::AppHandle) {
    let dir = match app_data_dir(app) {
        Ok(dir) => dir.join("logs"),
        Err(err) => {
            eprintln!("could not determine app data dir for logging: {err}");
            return;
        }
    };
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("could not create log directory {}: {err}", dir.display());
        return;
    }

    let file_appender = tracing_appender::rolling::daily(&dir, "server.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    Box::leak(Box::new(guard));

    let env_filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,sqlx=warn".into())
    };

    use std::io::IsTerminal;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(env_filter())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(non_blocking),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(std::io::stdout().is_terminal())
                .with_writer(std::io::stdout),
        )
        .init();

    tracing::info!(log_dir = %dir.display(), "logging initialized");
}

fn configured_rendezvous_url() -> Option<String> {
    std::env::var("SWARM_RENDEZVOUS_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            option_env!("SWARM_RENDEZVOUS_URL")
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty())
        })
}

impl AppState {
    /// Build the core from persisted settings on first use. Fails with the
    /// sentinel `"not_configured"` when no media folder has been chosen yet
    /// — the frontend checks for that exact string to show onboarding
    /// instead of a raw error.
    async fn core<R: tauri::Runtime>(&self, app: &tauri::AppHandle<R>) -> Result<Arc<ServerCore>, String> {
        self.core
            .get_or_try_init(|| async {
                let dir = app_data_dir(app)?;
                let mut settings = settings::load(&dir);
                if settings.media_roots.is_empty() {
                    return Err("not_configured".to_string());
                }
                if settings::populate_reconnect_urls(&mut settings) {
                    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
                }
                let recovery_settings_dir = dir.clone();
                let (default_bind, default_http_media_bind, default_http_media_tls_bind) = self
                    .test_bind_override
                    .unwrap_or_else(|| {
                        (
                            "0.0.0.0:8543".parse().unwrap(),
                            "0.0.0.0:8546".parse().unwrap(),
                            "0.0.0.0:8547".parse().unwrap(),
                        )
                    });
                let config = ServerConfig {
                    media_roots: to_media_roots(&settings.media_roots),
                    data_dir: dir,
                    // This remains an environment override for development and
                    // managed deployments; ordinary desktop users never need it.
                    // Test-only bind overrides win over the env var so
                    // concurrently-running tests never race on the same port.
                    bind: if self.test_bind_override.is_some() {
                        default_bind
                    } else {
                        std::env::var("SWARM_PEER_BIND")
                            .unwrap_or_else(|_| "0.0.0.0:8543".into())
                            .parse()
                            .expect("SWARM_PEER_BIND must be host:port")
                    },
                    // Same override convention as SWARM_PEER_BIND above.
                    // Always on (see http_media.rs), so — unlike mcp_port —
                    // this deliberately has no Settings/UI field yet: an env
                    // var is enough for the one real need (port conflicts on
                    // a dev machine) without exposing a toggle for a surface
                    // that isn't optional.
                    http_media_bind: if self.test_bind_override.is_some() {
                        default_http_media_bind
                    } else {
                        std::env::var("SWARM_HTTP_MEDIA_BIND")
                            .unwrap_or_else(|_| "0.0.0.0:8546".into())
                            .parse()
                            .expect("SWARM_HTTP_MEDIA_BIND must be host:port")
                    },
                    // Same "always on, no Settings/UI toggle" convention as
                    // http_media_bind above. TLS on this second port is what
                    // lets a paired HTTP-only device (Roku) reach the media
                    // server off-LAN through the relay — see http_media.rs
                    // and swarm_p2p::http_tls.
                    http_media_tls_bind: Some(if self.test_bind_override.is_some() {
                        default_http_media_tls_bind
                    } else {
                        std::env::var("SWARM_HTTP_MEDIA_TLS_BIND")
                            .unwrap_or_else(|_| "0.0.0.0:8547".into())
                            .parse()
                            .expect("SWARM_HTTP_MEDIA_TLS_BIND must be host:port")
                    }),
                    allowed_fingerprints: vec![],
                    // Real bug, found live: with PreferKeyring, a token saved
                    // successfully via the OS keychain has no file backup —
                    // `TokenStore::save` only falls back to the file when the
                    // keyring *write* itself fails. macOS Keychain access is
                    // tied to the app binary's code signature, so an
                    // unsigned/ad-hoc-resigned build (this app's normal dev
                    // state) can lose access to its own previously-saved
                    // entry after a rebuild; `restore_stun_link` then finds
                    // no token and discards the whole STUN link — even though
                    // base_url/device_id/swarms all survived fine in
                    // server-state.sqlite — forcing full STUN URL + join code
                    // re-entry. FileOnly's plain 0600 file isn't tied to code
                    // signing, so it survives every rebuild/restart; the
                    // token itself is already revocable-not-precious (the
                    // STUN server's own row is the real revocation
                    // authority), so this is the right trade-off here.
                    token_store_mode: TokenStoreMode::FileOnly,
                    managed_rendezvous_url: configured_rendezvous_url(),
                };
                let core = ServerCore::start(config).await.map_err(|e| e.to_string())?;
                core.set_streaming_upload_budget_enabled(settings.streaming_upload_budget_enabled);
                core.set_artwork_disk_cache_enabled(settings.artwork_disk_cache_enabled);
                core.set_local_transcription_enabled(settings.local_transcription_enabled);
                core.set_transcription_pause_while_streaming(settings.transcription_pause_while_streaming);
                core.set_transcription_skip_if_subtitles_exist(settings.transcription_skip_if_subtitles_exist);
                core.set_video_encoder_mode(
                    swarm_media::transcode::VideoEncoderMode::from_str_lenient(
                        &settings.video_encoder_mode,
                    ),
                );
                core.set_max_transcode_height(settings.max_transcode_height);
                core.set_hls_segment_seconds(settings.hls_segment_seconds);
                start_media_root_recovery(Arc::clone(&core), recovery_settings_dir.clone());
                start_auto_library_watch(Arc::clone(&core), recovery_settings_dir);
                if settings.mcp_enabled {
                    if let Some(access_token) = settings.mcp_access_token.filter(|token| !token.is_empty()) {
                        let mcp_core = Arc::clone(&core);
                        tokio::spawn(async move {
                            if let Err(err) = mcp::serve(mcp_core, settings.mcp_port, access_token).await {
                                tracing::error!(%err, "MCP server stopped");
                            }
                        });
                    } else {
                        tracing::error!("MCP server is enabled but has no access token; create one in the AI tab");
                    }
                }
                Ok(core)
            })
            .await
            .cloned()
    }

    /// If a core is already running, live-apply a fresh root list (see
    /// `ServerCore::update_media_roots`) — `add_media_root`/`remove_media_root`
    /// take effect immediately instead of on next launch. A no-op, not an
    /// error, when no core exists yet (e.g. mid first-run onboarding, before
    /// any folder has ever been chosen): `OnceCell::get` never triggers
    /// initialization, so onboarding is unaffected.
    async fn apply_live_roots(
        &self,
        roots: &[MediaRootSetting],
    ) -> Result<Option<RescanResult>, String> {
        let Some(core) = self.core.get() else {
            return Ok(None);
        };
        let report = core
            .update_media_roots(to_media_roots(roots))
            .await
            .map_err(|e| e.to_string())?;
        Ok(Some(RescanResult {
            added: report.added,
            updated: report.updated,
            removed: report.removed,
            unchanged: report.unchanged,
            incomplete_roots: report.incomplete_roots,
            validation_issues: report.validation_issues,
        }))
    }
}

/// Network mounts can remain present under `/Volumes` while every read
/// fails. Poll the same real-read health check used by the UI, ask macOS to
/// remount known SMB roots with saved credentials, and retry only a scan that
/// overlapped the outage or had failed. A healthy catalog does not need a
/// filesystem walk merely because the same mount came back. Attempts are
/// throttled so an offline NAS cannot produce a reconnect storm.
const MEDIA_ROOT_RECONNECT_RETRY_INTERVAL: Duration = Duration::from_secs(90);

fn start_media_root_recovery(core: Arc<ServerCore>, settings_dir: PathBuf) {
    tokio::spawn(async move {
        let mut unavailable = HashSet::<String>::new();
        let mut needs_recovery_rescan = HashSet::<String>::new();
        let mut last_attempt = HashMap::<String, Instant>::new();
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        // Default `Burst` replays every tick missed while a slow health check
        // or reconnect ran long, firing the whole backlog back-to-back
        // instead of settling back onto the 10s cadence — the same thrash
        // pattern #230 found on the auto-library-watch interval below.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let mut persisted = settings::load(&settings_dir);
            if settings::populate_reconnect_urls(&mut persisted) {
                if let Err(error) = settings::save(&settings_dir, &persisted) {
                    tracing::warn!(%error, "could not save detected network-share reconnect URL");
                }
            }
            let roots = persisted.media_roots;
            let health = tokio::task::spawn_blocking({
                let roots = roots.clone();
                move || settings::media_root_health(&roots)
            })
            .await
            .unwrap_or_default();
            let configured_labels = roots
                .iter()
                .map(|root| root.label.as_str())
                .collect::<HashSet<_>>();
            let configured_reconnects = roots
                .iter()
                .map(reconnect_attempt_key)
                .collect::<HashSet<_>>();
            unavailable.retain(|label| configured_labels.contains(label.as_str()));
            needs_recovery_rescan.retain(|label| configured_labels.contains(label.as_str()));
            last_attempt.retain(|key, _| configured_reconnects.contains(key));
            let mut recovered = Vec::<String>::new();
            let mut recovered_needing_rescan = Vec::<String>::new();
            let scan_needs_retry = matches!(
                core.scan_status(),
                ScanState::Scanning | ScanState::Failed(_)
            );
            for (root, status) in roots.iter().zip(health) {
                let reconnect_key = reconnect_attempt_key(root);
                if status.available {
                    if unavailable.remove(&root.label) {
                        tracing::info!(root = %root.label, path = %root.path, "media root recovered");
                        recovered.push(root.label.clone());
                        if needs_recovery_rescan.remove(&root.label) {
                            recovered_needing_rescan.push(root.label.clone());
                        }
                    }
                    continue;
                }
                if scan_needs_retry {
                    needs_recovery_rescan.insert(root.label.clone());
                }
                if unavailable.insert(root.label.clone()) {
                    // A macOS TCC denial is not an outage: reconnecting or
                    // waiting never clears it, so point the user straight at
                    // the one-time System Settings grant instead (#196).
                    let (title, message) = if status.permission_denied {
                        (
                            "SWARM needs permission to read a media location",
                            format!(
                                "macOS is blocking SWARM Server from reading \"{}\" at {}.\n\nGrant access once: open System Settings \u{2192} Privacy & Security \u{2192} Files and Folders (or Full Disk Access), turn on \"SWARM Server\", then press Rescan. macOS remembers this choice.",
                                root.label, root.path,
                            ),
                        )
                    } else {
                        let retry_message = if status.auto_reconnect {
                            "SWARM will ask macOS to reconnect it automatically. It will retry a scan only if one overlapped this outage."
                        } else {
                            "Reconnect the storage, then use Rescan so the library is synchronized."
                        };
                        (
                            "Media storage unavailable",
                            format!(
                                "The media root \"{}\" at {} failed a real directory/file read: {}\n\n{}",
                                root.label,
                                root.path,
                                status.error.as_deref().unwrap_or("unknown I/O error"),
                                retry_message,
                            ),
                        )
                    };
                    if let Err(error) = core
                        .library
                        .record_server_notification("error", title, &message)
                        .await
                    {
                        tracing::warn!(%error, "could not save media-root failure notification");
                    }
                }
                let should_attempt = !status.permission_denied
                    && status.auto_reconnect
                    && last_attempt
                        .get(&reconnect_key)
                        .is_none_or(|last| last.elapsed() >= MEDIA_ROOT_RECONNECT_RETRY_INTERVAL);
                if should_attempt {
                    // Multiple configured roots commonly live under the same
                    // SMB share. Reconnect the shared URL once per interval;
                    // parallel Finder requests for that identical URL can
                    // deadlock macOS's credential/mount agent and leave every
                    // root unavailable indefinitely (#131).
                    last_attempt.insert(reconnect_key, Instant::now());
                    let reconnect_root = root.clone();
                    tokio::task::spawn_blocking(move || {
                        match settings::reconnect_network_root(&reconnect_root) {
                            Ok(true) => {
                                tracing::info!(root = %reconnect_root.label, "network-share reconnect became readable")
                            }
                            Ok(false) => {}
                            Err(error) => {
                                tracing::warn!(root = %reconnect_root.label, %error, "network-share reconnect request failed")
                            }
                        }
                    });
                }
            }
            if !recovered.is_empty() {
                if recovered_needing_rescan.is_empty() {
                    let message = format!(
                        "Recovered: {}\n\nThe existing library remains valid; no rescan was needed.",
                        recovered.join(", "),
                    );
                    if let Err(error) = core
                        .library
                        .record_server_notification("success", "Media storage recovered", &message)
                        .await
                    {
                        tracing::warn!(%error, "could not save media-root recovery notification");
                    }
                    continue;
                }
                match core
                    .rescan_roots_by_label(&recovered_needing_rescan, false)
                    .await
                {
                    Ok(report) => {
                        let message = format!(
                            "Recovered: {}\nRescanned after the interrupted scan: {}\n\nResult: +{} added, {} updated, {} removed, {} unchanged.",
                            recovered.join(", "),
                            recovered_needing_rescan.join(", "),
                            report.added,
                            report.updated,
                            report.removed,
                            report.unchanged,
                        );
                        if let Err(error) = core
                            .library
                            .record_server_notification(
                                "success",
                                "Media storage recovered",
                                &message,
                            )
                            .await
                        {
                            tracing::warn!(%error, "could not save media-root recovery notification");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "media-root recovery rescan failed");
                        let message = format!(
                            "Recovered: {}\n\nThe interrupted library scan could not be retried: {error}",
                            recovered.join(", "),
                        );
                        if let Err(save_error) = core
                            .library
                            .record_server_notification(
                                "error",
                                "Media storage recovered, but rescan failed",
                                &message,
                            )
                            .await
                        {
                            tracing::warn!(%save_error, "could not save recovery-rescan failure notification");
                        }
                    }
                }
            }
        }
    });
}

fn reconnect_attempt_key(root: &MediaRootSetting) -> String {
    root.reconnect_url
        .clone()
        .unwrap_or_else(|| format!("path:{}", root.path))
}

/// Base cadence of the idle-time watcher below, which re-walks media roots
/// looking for added/removed/updated files. Short enough that a change is
/// noticed without the user having to press Rescan, long enough that a large
/// network share isn't re-walked so often it competes with playback/
/// transcoding for I/O — the same trade-off `ROSTER_SYNC_INTERVAL` and the
/// 10s root-health poll make for their own much cheaper checks. A root whose
/// walk keeps coming back incomplete is rescanned on a multiple of this
/// interval instead (see `auto_library_watch_backoff_ticks`).
const AUTO_LIBRARY_WATCH_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Ceiling on the exponential back-off applied to a media root whose walk
/// keeps returning an incomplete snapshot — 32 watch ticks ≈ 8 hours at the
/// 15-minute base cadence. A share that flaps for a whole day then costs one
/// walk every 8 hours instead of ~96 walks, each of which was moving the
/// catalog thumbprint and forcing every client into a full manifest reload
/// (#222).
const MAX_AUTO_LIBRARY_WATCH_BACKOFF_TICKS: u32 = 32;

/// Watch ticks to skip before auto-rescanning a root again, given how many
/// consecutive auto-scans of it have come back incomplete. `0` → scan every
/// tick as before; then 1, 2, 4, 8, 16, capped at
/// [`MAX_AUTO_LIBRARY_WATCH_BACKOFF_TICKS`].
fn auto_library_watch_backoff_ticks(consecutive_incomplete: u32) -> u32 {
    match consecutive_incomplete {
        0 => 0,
        n => (1u32 << (n - 1).min(5)).min(MAX_AUTO_LIBRARY_WATCH_BACKOFF_TICKS),
    }
}

#[cfg(test)]
mod auto_library_watch_backoff_tests {
    use super::{auto_library_watch_backoff_ticks, MAX_AUTO_LIBRARY_WATCH_BACKOFF_TICKS};

    #[test]
    fn clean_scan_keeps_the_every_tick_cadence() {
        assert_eq!(auto_library_watch_backoff_ticks(0), 0);
    }

    #[test]
    fn incomplete_scans_back_off_exponentially_then_cap() {
        assert_eq!(auto_library_watch_backoff_ticks(1), 1);
        assert_eq!(auto_library_watch_backoff_ticks(2), 2);
        assert_eq!(auto_library_watch_backoff_ticks(3), 4);
        assert_eq!(auto_library_watch_backoff_ticks(4), 8);
        assert_eq!(auto_library_watch_backoff_ticks(5), 16);
        assert_eq!(auto_library_watch_backoff_ticks(6), 32);
        assert_eq!(
            auto_library_watch_backoff_ticks(50),
            MAX_AUTO_LIBRARY_WATCH_BACKOFF_TICKS
        );
    }
}

#[derive(Default)]
struct RootWatchBackoff {
    /// Consecutive auto-scans of this root whose snapshot was incomplete.
    consecutive_incomplete: u32,
    /// Remaining watch ticks to skip before this root is scanned again.
    ticks_until_due: u32,
}

/// Issue #37: periodically reconcile every media root against the library
/// (reusing the exact scan/diff machinery `rescan` already exposes to the
/// UI) and, whenever that finds new or changed files, automatically trigger
/// metadata scraping and record a notification — no user action required.
/// Scraping itself already skips movies/shows when no TMDb key is
/// configured and always attempts music via MusicBrainz (no key needed
/// there); see `run_bulk_scrape`'s per-kind gating, unchanged here.
///
/// Roots are scanned individually (`rescan_roots_by_label`) rather than in
/// one flat `rescan(None)` so a single continuously-changing network share
/// can be backed off an exponentially longer interval when its walk keeps
/// coming back incomplete, instead of re-walking it — and re-churning every
/// client's catalog — every 15 minutes (#222). A clean scan clears the
/// back-off immediately.
fn start_auto_library_watch(core: Arc<ServerCore>, settings_dir: PathBuf) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(AUTO_LIBRARY_WATCH_INTERVAL);
        // Default `Burst` replays every tick missed while a scan ran past
        // this interval's period, firing the whole backlog back-to-back —
        // observed in the wild pinning the server in a near-continuous
        // rescan loop for ~50 minutes after one long-running scan (#230).
        // `Skip` just resumes the normal cadence from now instead.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The first tick fires immediately; skip it since `ServerCore::start`
        // already kicked off an initial scan of its own.
        interval.tick().await;
        let mut backoff: HashMap<String, RootWatchBackoff> = HashMap::new();
        loop {
            interval.tick().await;
            let settings = settings::load(&settings_dir);
            if !settings.auto_library_watch_enabled {
                continue;
            }
            // The live resolver set is authoritative for what
            // `rescan_roots_by_label` will accept — a settings.json edited
            // out from under a running core may not match.
            let configured: Vec<String> = core
                .media_roots
                .roots()
                .into_iter()
                .map(|root| root.label)
                .collect();
            if configured.is_empty() {
                continue;
            }
            backoff.retain(|label, _| configured.iter().any(|l| l == label));

            // A root is due this tick unless it is still serving out a
            // back-off from a run of incomplete scans.
            let mut due = Vec::new();
            for label in &configured {
                let state = backoff.entry(label.clone()).or_default();
                if state.ticks_until_due == 0 {
                    due.push(label.clone());
                } else {
                    state.ticks_until_due -= 1;
                }
            }
            if due.is_empty() {
                continue;
            }

            let report = match core.rescan_roots_by_label(&due, false).await {
                Ok(report) => report,
                Err(error) => {
                    tracing::warn!(%error, "automatic library scan failed");
                    continue;
                }
            };

            for label in &due {
                let state = backoff.entry(label.clone()).or_default();
                if report.incomplete_roots.iter().any(|l| l == label) {
                    state.consecutive_incomplete = state.consecutive_incomplete.saturating_add(1);
                    state.ticks_until_due =
                        auto_library_watch_backoff_ticks(state.consecutive_incomplete);
                    tracing::info!(
                        root = %label,
                        consecutive_incomplete = state.consecutive_incomplete,
                        skip_ticks = state.ticks_until_due,
                        "media root scan keeps returning an incomplete snapshot; backing off auto-rescan"
                    );
                } else {
                    *state = RootWatchBackoff::default();
                }
            }

            if report.added + report.updated + report.removed == 0 {
                continue;
            }
            let message = format!(
                "+{} added, {} updated, {} removed, {} unchanged.",
                report.added, report.updated, report.removed, report.unchanged,
            );
            if let Err(error) = core
                .library
                .record_server_notification("success", "Library updated", &message)
                .await
            {
                tracing::warn!(%error, "could not save library-change notification");
            }
            if report.added + report.updated == 0 {
                continue;
            }
            let tmdb_api_key = settings.tmdb_api_key.clone();
            let scrape_result = core
                .run_scrape(
                    ScrapeConfig {
                        tmdb_api_key,
                        ..Default::default()
                    },
                    None,
                    false,
                )
                .await;
            if matches!(scrape_result, Err(ServerError::ScrapeInProgress)) {
                // A manual scrape happened to be running; not an error, and
                // the next watch tick will pick up anything still unscraped.
                continue;
            }
            record_scrape_result_notification(
                &core,
                &scrape_result,
                "Automatic scrape finished with issues",
                "Automatic scrape failed",
            )
            .await;
        }
    });
}

/// Shared by [`start_auto_library_watch`] and the manual `run_scrape`
/// command: a clean run (no issues) intentionally records nothing — the
/// caller already sees the result directly (UI refresh or this function's
/// own "Library updated" notification), so a notification here is reserved
/// for something the user should actually look at.
async fn record_scrape_result_notification(
    core: &ServerCore,
    result: &Result<BulkScrapeReport, ServerError>,
    issues_title: &str,
    failure_title: &str,
) {
    match result {
        Ok(report) if !report.issues.is_empty() => {
            let mut message = format!(
                "Matched: {}\nNot found: {}\nFailed: {}\nSkipped: {}",
                report.matched, report.not_found, report.failed, report.skipped,
            );
            message.push_str("\n\nIssues:\n");
            message.push_str(&group_scrape_issues_by_kind(core, &report.issues).await);
            let level = if report.failed > 0 {
                "error"
            } else {
                "warning"
            };
            if let Err(error) = core
                .library
                .record_server_notification(level, issues_title, message.trim_end())
                .await
            {
                tracing::warn!(%error, "could not save scrape issues notification");
            }
        }
        Err(error) => {
            if let Err(save_error) = core
                .library
                .record_server_notification("error", failure_title, &error.to_string())
                .await
            {
                tracing::warn!(%save_error, "could not save scrape failure notification");
            }
        }
        _ => {}
    }
}

/// Renders a scrape report's issue list grouped under `Movies` / `Shows` /
/// `Music` (and `Other` for anything whose catalog entry can't be resolved),
/// each heading carrying its own count, so "scrape finished with issues" is
/// scannable at a glance instead of one flat list. The kind comes from the
/// catalog entry, not the issue itself — `ScrapeIssue` only carries the
/// entry key.
async fn group_scrape_issues_by_kind(
    core: &ServerCore,
    issues: &[swarm_media::scrape::ScrapeIssue],
) -> String {
    let mut movies = Vec::new();
    let mut shows = Vec::new();
    let mut music = Vec::new();
    let mut other = Vec::new();
    for issue in issues {
        let line = format!("  • {} — {}", issue.title, issue.reason);
        match core.library.get(&issue.entry_key).await {
            Ok(Some(entry)) => match entry.kind {
                MediaKind::Movie => movies.push(line),
                MediaKind::Episode => shows.push(line),
                MediaKind::Track => music.push(line),
            },
            _ => other.push(line),
        }
    }
    let mut out = String::new();
    for (label, lines) in [
        ("Movies", &movies),
        ("Shows", &shows),
        ("Music", &music),
        ("Other", &other),
    ] {
        if lines.is_empty() {
            continue;
        }
        out.push_str(&format!("{} ({}):\n{}\n\n", label, lines.len(), lines.join("\n")));
    }
    out.trim_end().to_string()
}

/// Rejects a new root `path` that names the same filesystem location as an
/// already-configured root, or is nested inside/around one — e.g. a
/// dedicated mount added for one show's folder on top of an existing
/// umbrella root that already contains it. Scanning both would catalog the
/// same physical files twice (once per root's `{label}/` prefix), silently
/// duplicating every entry under the overlap in the catalog even though
/// there is exactly one copy of each file on disk (see
/// `swarm_media::roots::paths_overlap`'s doc comment) — caught here, at the
/// point a new root is actually added, rather than only self-healing on a
/// later scan.
fn reject_overlapping_root(existing: &[MediaRootSetting], new_path: &str) -> Result<(), String> {
    let new_path = PathBuf::from(new_path);
    for root in existing {
        let root_path = PathBuf::from(&root.path);
        if !swarm_media::roots::paths_overlap(&root_path, &new_path) {
            continue;
        }
        let same = root_path == new_path
            || matches!(
                (std::fs::canonicalize(&root_path), std::fs::canonicalize(&new_path)),
                (Ok(a), Ok(b)) if a == b
            );
        return Err(if same {
            format!(
                "this folder is already added as media root \"{}\"",
                root.label
            )
        } else {
            format!(
                "this folder overlaps with the existing root \"{}\" ({}) — scanning both would \
                 catalog the same files twice",
                root.label, root.path
            )
        });
    }
    Ok(())
}

fn to_media_roots(settings: &[MediaRootSetting]) -> Vec<MediaRoot> {
    settings
        .iter()
        .map(|r| MediaRoot {
            label: r.label.clone(),
            path: PathBuf::from(&r.path),
        })
        .collect()
}

#[derive(serde::Serialize)]
struct SettingsView {
    media_roots: Vec<MediaRootSetting>,
    has_tmdb_key: bool,
    has_opensubtitles_key: bool,
    streaming_upload_budget_enabled: bool,
    artwork_disk_cache_enabled: bool,
    local_transcription_enabled: bool,
    transcription_pause_while_streaming: bool,
    transcription_skip_if_subtitles_exist: bool,
    mcp_enabled: bool,
    mcp_port: u16,
    mcp_access_token: Option<String>,
    auto_library_watch_enabled: bool,
    video_encoder_mode: String,
    max_transcode_height: u32,
    hls_segment_seconds: u32,
    auto_update: String,
    app_version: String,
    ai_providers: Vec<AiProviderView>,
    ai_scan_assist_enabled: bool,
    ai_reorganize_enabled: bool,
}

#[derive(serde::Serialize)]
struct AiProviderView {
    id: String,
    label: String,
    enabled: bool,
    model: String,
    has_api_key: bool,
}

fn ai_provider_views(providers: &[AiProviderSetting]) -> Vec<AiProviderView> {
    providers
        .iter()
        .map(|p| AiProviderView {
            id: p.id.clone(),
            label: ai::AiProviderKind::from_id(&p.id)
                .map(|k| k.label().to_string())
                .unwrap_or_else(|| p.id.clone()),
            enabled: p.enabled,
            model: p.model.clone(),
            has_api_key: p.api_key.is_some(),
        })
        .collect()
}

#[tauri::command]
async fn get_settings<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<SettingsView, String> {
    let settings = settings::load(&app_data_dir(&app)?);
    Ok(SettingsView {
        media_roots: settings.media_roots,
        has_tmdb_key: settings.tmdb_api_key.is_some(),
        has_opensubtitles_key: settings.opensubtitles_api_key.is_some(),
        streaming_upload_budget_enabled: settings.streaming_upload_budget_enabled,
        artwork_disk_cache_enabled: settings.artwork_disk_cache_enabled,
        local_transcription_enabled: settings.local_transcription_enabled,
        transcription_pause_while_streaming: settings.transcription_pause_while_streaming,
        transcription_skip_if_subtitles_exist: settings.transcription_skip_if_subtitles_exist,
        mcp_enabled: settings.mcp_enabled,
        mcp_port: settings.mcp_port,
        mcp_access_token: settings.mcp_access_token,
        auto_library_watch_enabled: settings.auto_library_watch_enabled,
        video_encoder_mode: settings.video_encoder_mode,
        max_transcode_height: settings.max_transcode_height,
        hls_segment_seconds: settings.hls_segment_seconds,
        auto_update: settings.auto_update,
        app_version: app.package_info().version.to_string(),
        ai_providers: ai_provider_views(&settings.ai_providers),
        ai_scan_assist_enabled: settings.ai_scan_assist_enabled,
        ai_reorganize_enabled: settings.ai_reorganize_enabled,
    })
}

#[tauri::command]
async fn set_video_encoder_mode<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    mode: String,
) -> Result<(), String> {
    let parsed = swarm_media::transcode::VideoEncoderMode::from_str_lenient(&mode);
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.video_encoder_mode = parsed.as_str().to_string();
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    if let Some(core) = state.core.get() {
        core.set_video_encoder_mode(parsed);
    }
    Ok(())
}

#[tauri::command]
async fn set_max_transcode_height<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    height: u32,
) -> Result<(), String> {
    // The UI offers 0 (source), 1080, 2160; accept anything and let the
    // transcoder treat 0 as "no cap".
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.max_transcode_height = height;
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    if let Some(core) = state.core.get() {
        core.set_max_transcode_height(height);
    }
    Ok(())
}

#[tauri::command]
async fn set_hls_segment_seconds<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    seconds: u32,
) -> Result<(), String> {
    let seconds = seconds.max(2);
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.hls_segment_seconds = seconds;
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    if let Some(core) = state.core.get() {
        core.set_hls_segment_seconds(seconds);
    }
    Ok(())
}

/// Takes effect on the auto-watcher's next tick (at most
/// `AUTO_LIBRARY_WATCH_INTERVAL` later) — it re-reads settings itself each
/// time, same as the media-root recovery loop already does, so there is no
/// live core state to push this into immediately.
#[tauri::command]
async fn set_auto_library_watch_enabled<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    enabled: bool,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.auto_library_watch_enabled = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

// ----- Software update ---------------------------------------------------

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UpdateSummary {
    current_version: String,
    version: String,
    notes: String,
    pub_date: String,
}

impl UpdateSummary {
    fn from_update(update: &tauri_plugin_updater::Update) -> Self {
        UpdateSummary {
            current_version: update.current_version.clone(),
            version: update.version.clone(),
            notes: update.body.clone().unwrap_or_default(),
            pub_date: update.date.map(|date| date.to_string()).unwrap_or_default(),
        }
    }
}

async fn pending_update<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<Option<tauri_plugin_updater::Update>, String> {
    use tauri_plugin_updater::UpdaterExt;
    app.updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn strip_quarantine() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bundle) = exe.ancestors().nth(3) {
            let _ = std::process::Command::new("/usr/bin/xattr")
                .args(["-dr", "com.apple.quarantine"])
                .arg(bundle)
                .status();
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn strip_quarantine() {}

#[tauri::command]
async fn set_auto_update<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    mode: String,
) -> Result<(), String> {
    if !matches!(mode.as_str(), "off" | "notify" | "auto") {
        return Err(format!("unknown update mode: {mode}"));
    }
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.auto_update = mode;
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_for_update<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Option<UpdateSummary>, String> {
    Ok(pending_update(&app)
        .await?
        .map(|update| UpdateSummary::from_update(&update)))
}

/// Downloads and applies the update, then relaunches. The caller is expected
/// to warn the user first — this drops any live playback connection.
#[tauri::command]
async fn install_update<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    let Some(update) = pending_update(&app).await? else {
        return Err("No update is available.".into());
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    strip_quarantine();
    app.restart()
}

/// Honour `auto_update` once at startup, off the main thread. `"auto"`
/// downloads now and installs on the next quit (never mid-session — the
/// server holds live connections); `"notify"` emits `update-available`.
fn spawn_startup_update_check<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let mode = match app_data_dir(&app) {
            Ok(dir) => settings::load(&dir).auto_update,
            Err(_) => return,
        };
        if mode == "off" {
            return;
        }
        let Ok(Some(update)) = pending_update(&app).await else {
            return;
        };
        if mode == "auto" {
            // Swap the bundle in place now but do NOT restart: the running
            // server keeps its mapped binary and any live playback, and the
            // new version takes effect the next time the user quits and
            // relaunches. `notify` mode still gets the last word if the user
            // opens the window before quitting.
            let _ = update.download_and_install(|_, _| {}, || {}).await;
            strip_quarantine();
            let _ = app.emit("update-staged", UpdateSummary::from_update(&update));
        } else {
            let _ = app.emit("update-available", UpdateSummary::from_update(&update));
        }
    });
}

/// Does not initialize `ServerCore`, so the warning can render even when a
/// disconnected file share is the very thing preventing normal dashboard
/// work from completing.
#[tauri::command]
async fn get_media_root_health<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<Vec<MediaRootHealth>, String> {
    let roots = settings::load(&app_data_dir(&app)?).media_roots;
    tokio::task::spawn_blocking(move || settings::media_root_health(&roots))
        .await
        .map_err(|error| error.to_string())
}

async fn pick_folder<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<Option<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    Ok(rx.await.map_err(|e| e.to_string())?.map(|p| p.to_string()))
}

/// Opens the native folder picker and persists the choice as the app's
/// *first* media root, labeled `"local"` — the first-run onboarding path.
/// Does not affect an already-running core — see the module docs.
#[tauri::command]
async fn choose_media_folder<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<Option<String>, String> {
    let Some(path) = pick_folder(&app).await? else {
        return Ok(None);
    };
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.media_roots = vec![MediaRootSetting {
        label: "local".to_string(),
        path: path.clone(),
        reconnect_url: settings::discover_reconnect_url(&path),
        asset_type: RootAssetType::Mixed,
    }];
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    Ok(Some(path))
}

/// Same native folder picker as [`choose_media_folder`], but only returns
/// the chosen path — no persistence. Used by the "add another root" flow
/// (Details tab), which needs the user to also supply a label before
/// `add_media_root` actually saves anything.
#[tauri::command]
async fn pick_folder_path<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<Option<String>, String> {
    pick_folder(&app).await
}

/// Native file picker, filtered to common image types — for the "upload
/// artwork" flow (Media tab), paired with [`read_file_bytes`].
#[tauri::command]
async fn pick_file_path<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<Option<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Images", &["jpg", "jpeg", "png", "webp"])
        .pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    Ok(rx.await.map_err(|e| e.to_string())?.map(|p| p.to_string()))
}

/// Reads a file's raw bytes for upload — paired with [`pick_file_path`],
/// which is how the frontend obtains a path it's allowed to read (the
/// webview otherwise has no filesystem access of its own).
#[tauri::command]
async fn read_file_bytes(path: String) -> Result<Vec<u8>, String> {
    tokio::fs::read(&path).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_media_roots<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<Vec<MediaRootSetting>, String> {
    Ok(settings::load(&app_data_dir(&app)?).media_roots)
}

#[derive(serde::Serialize)]
#[cfg_attr(test, derive(Debug))]
struct MediaRootsResult {
    media_roots: Vec<MediaRootSetting>,
    /// Present when a core was already running and the change was applied
    /// live; absent during first-run onboarding, before any core exists.
    rescan: Option<RescanResult>,
}

/// A filesystem-safe, unique label derived from a folder's own name — used
/// when a root is added through the browse-only "Add media root" modal
/// (issue #252), which no longer asks the user to type one. Falls back to
/// `"media"` for a pathologically nameless path, and disambiguates a
/// collision with an existing root by appending `-2`, `-3`, …
fn derive_unique_label(existing: &[MediaRootSetting], path: &str) -> String {
    let base = std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .map(|name| {
            name.chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                .collect::<String>()
        })
        .map(|name| name.trim_matches('-').to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "media".to_string());
    if !existing.iter().any(|r| r.label == base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !existing.iter().any(|r| r.label == *candidate))
        .expect("an unbounded counter always yields a free label")
}

/// Adds an additional root (a local folder or a mounted NAS share) alongside
/// whatever's already configured. `label` may be empty — the browse-only
/// Add-media-root modal (issue #252) supplies only the chosen folder and its
/// asset type, and the label is derived from the folder name here. Applied
/// live to an already-running core — see the module docs.
#[tauri::command]
async fn add_media_root<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    label: String,
    path: String,
    asset_type: Option<String>,
) -> Result<MediaRootsResult, String> {
    let label = label.trim().to_string();
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("choose a folder first".to_string());
    }
    let asset_type = RootAssetType::parse(asset_type.as_deref())?;
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    // Issue #252: a second root at the same (or an overlapping) location is
    // forbidden — scanning both would catalog the same files twice.
    reject_overlapping_root(&settings.media_roots, &path)?;
    let label = if label.is_empty() {
        derive_unique_label(&settings.media_roots, &path)
    } else {
        if settings.media_roots.iter().any(|r| r.label == label) {
            return Err(format!("a root labeled \"{label}\" already exists"));
        }
        label
    };
    let reconnect_url = settings::discover_reconnect_url(&path);
    tracing::info!(%label, %path, asset_type = asset_type.label(), "adding media root");
    settings.media_roots.push(MediaRootSetting {
        label,
        path,
        reconnect_url,
        asset_type,
    });
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    let rescan = state.apply_live_roots(&settings.media_roots).await?;
    Ok(MediaRootsResult {
        media_roots: settings.media_roots,
        rescan,
    })
}

/// Connects an SMB share and adds the resulting OS mount as a media root.
/// Credential entry/storage stays in macOS rather than passing a password
/// through the webview or persisting one in SWARM settings.
#[tauri::command]
async fn connect_smb_root<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    label: String,
    server: String,
    share: String,
    username: Option<String>,
    asset_type: Option<String>,
) -> Result<MediaRootsResult, String> {
    let label = label.trim().to_string();
    let server = server.trim().to_string();
    let share = share.trim().to_string();
    let username = username.map(|value| value.trim().to_string());
    let asset_type = RootAssetType::parse(asset_type.as_deref())?;
    let dir = app_data_dir(&app)?;
    if settings::load(&dir)
        .media_roots
        .iter()
        .any(|root| root.label == label)
    {
        return Err(format!("a root labeled \"{label}\" already exists"));
    }

    let mounted = tokio::task::spawn_blocking({
        let label = label.clone();
        let server = server.clone();
        let share = share.clone();
        move || settings::connect_smb_share(&label, &server, &share, username.as_deref())
    })
    .await
    .map_err(|error| error.to_string())??;

    let mut persisted = settings::load(&dir);
    if persisted.media_roots.iter().any(|root| root.label == label) {
        return Err(format!("a root labeled \"{label}\" already exists"));
    }
    reject_overlapping_root(&persisted.media_roots, &mounted.path)?;
    persisted.media_roots.push(MediaRootSetting {
        label,
        path: mounted.path,
        reconnect_url: Some(mounted.reconnect_url),
        asset_type,
    });
    settings::save(&dir, &persisted).map_err(|error| error.to_string())?;
    let rescan = state.apply_live_roots(&persisted.media_roots).await?;
    Ok(MediaRootsResult {
        media_roots: persisted.media_roots,
        rescan,
    })
}

/// User-triggered stale-SMB recovery. The background loop only asks macOS to
/// reopen a share; this explicit action is allowed to force-unmount the exact
/// `/Volumes/<name>` SMB mount first. It retries scanning only when the
/// current/last scan was interrupted; an otherwise-valid catalog is reused.
#[tauri::command]
async fn repair_smb_root<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    label: String,
) -> Result<MediaRootsResult, String> {
    let dir = app_data_dir(&app)?;
    let persisted = settings::load(&dir);
    let root = persisted
        .media_roots
        .iter()
        .find(|root| root.label == label)
        .cloned()
        .ok_or_else(|| format!("no media root labeled \"{label}\" exists"))?;
    let needs_rescan = state.core.get().is_some_and(|core| {
        matches!(
            core.scan_status(),
            ScanState::Scanning | ScanState::Failed(_)
        )
    });
    tokio::task::spawn_blocking(move || settings::repair_smb_root(&root))
        .await
        .map_err(|error| error.to_string())??;
    let rescan = if needs_rescan {
        let Some(core) = state.core.get() else {
            return Err("media server stopped during SMB repair".to_string());
        };
        let report = core
            // User pressed the button: pause transcription like any other
            // explicit rescan, unlike the automatic background watch/recovery
            // callers above.
            .rescan_roots_by_label(&[label], true)
            .await
            .map_err(|error| error.to_string())?;
        Some(RescanResult {
            added: report.added,
            updated: report.updated,
            removed: report.removed,
            unchanged: report.unchanged,
            incomplete_roots: report.incomplete_roots,
            validation_issues: report.validation_issues,
        })
    } else {
        None
    };
    Ok(MediaRootsResult {
        media_roots: persisted.media_roots,
        rescan,
    })
}

/// Removes a configured root by label. Removing the last one is allowed
/// (issue #252): settings is left with no roots and the desktop UI drops
/// back to the "choose a media folder" onboarding view. A live core keeps
/// serving its existing catalog until the next launch — `RootResolver`
/// requires at least one root, so an empty set is never handed to it.
#[tauri::command]
async fn remove_media_root<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    label: String,
) -> Result<MediaRootsResult, String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    let before = settings.media_roots.len();
    settings.media_roots.retain(|r| r.label != label);
    if settings.media_roots.len() == before {
        return Err(format!("no media root labeled \"{label}\" exists"));
    }
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    let rescan = if settings.media_roots.is_empty() {
        None
    } else {
        state.apply_live_roots(&settings.media_roots).await?
    };
    Ok(MediaRootsResult {
        media_roots: settings.media_roots,
        rescan,
    })
}

#[tauri::command]
async fn set_tmdb_api_key<R: tauri::Runtime>(app: tauri::AppHandle<R>, key: String) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings: Settings = settings::load(&dir);
    settings.tmdb_api_key = if key.trim().is_empty() {
        None
    } else {
        Some(key.trim().to_string())
    };
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_opensubtitles_api_key<R: tauri::Runtime>(app: tauri::AppHandle<R>, key: String) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.opensubtitles_api_key = if key.trim().is_empty() {
        None
    } else {
        Some(key.trim().to_string())
    };
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct SubtitleDownloadResult {
    language: String,
    label: String,
}

#[tauri::command]
async fn download_subtitle<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
    language: String,
) -> Result<SubtitleDownloadResult, String> {
    let settings = settings::load(&app_data_dir(&app)?);
    let api_key = settings
        .opensubtitles_api_key
        .as_deref()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| "Add an OpenSubtitles API key in Details first.".to_string())?;
    let track = state
        .core(&app)
        .await?
        .download_subtitle(api_key, &entry_key, &language)
        .await?;
    Ok(SubtitleDownloadResult {
        language: track.language,
        label: track.label,
    })
}

#[tauri::command]
async fn set_streaming_upload_budget_enabled<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.streaming_upload_budget_enabled = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    if let Some(core) = state.core.get() {
        core.set_streaming_upload_budget_enabled(enabled);
    }
    Ok(())
}

#[tauri::command]
async fn set_artwork_disk_cache_enabled<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.artwork_disk_cache_enabled = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    if let Some(core) = state.core.get() {
        core.set_artwork_disk_cache_enabled(enabled);
    }
    Ok(())
}

#[tauri::command]
async fn set_local_transcription_enabled<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.local_transcription_enabled = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    let core = state.core(&app).await?;
    core.set_local_transcription_enabled(enabled);
    Ok(())
}

#[tauri::command]
async fn set_transcription_pause_while_streaming<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.transcription_pause_while_streaming = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    let core = state.core(&app).await?;
    core.set_transcription_pause_while_streaming(enabled);
    Ok(())
}

#[tauri::command]
async fn get_transcription_status<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<swarm_server::transcription::TranscriptionStatus, String> {
    state
        .core(&app)
        .await?
        .transcription_status()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_transcription_skip_if_subtitles_exist<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.transcription_skip_if_subtitles_exist = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    let core = state.core(&app).await?;
    core.set_transcription_skip_if_subtitles_exist(enabled);
    Ok(())
}

/// Targeted, per-item generation trigger. Turns on background generation if
/// it was off, since a user asking for subtitles on this one item clearly
/// wants Whisper to actually run rather than sit queued and idle.
#[tauri::command]
async fn generate_subtitles_for_entry<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    let core = state.core(&app).await?;
    if !settings.local_transcription_enabled {
        settings.local_transcription_enabled = true;
        settings::save(&dir, &settings).map_err(|e| e.to_string())?;
        core.set_local_transcription_enabled(true);
    }
    core.generate_subtitles_for_entry(&entry_key).await
}

/// Both take effect on next launch/restart, not live — see `mcp.rs`'s doc
/// comment and `AppState::core`, which only ever starts the MCP listener
/// once, the same time it starts `ServerCore` itself.
#[tauri::command]
async fn set_mcp_enabled<R: tauri::Runtime>(app: tauri::AppHandle<R>, enabled: bool) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings: Settings = settings::load(&dir);
    settings.mcp_enabled = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_mcp_port<R: tauri::Runtime>(app: tauri::AppHandle<R>, port: u16) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings: Settings = settings::load(&dir);
    settings.mcp_port = port;
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn generate_mcp_access_token<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<String, String> {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = format!("swarm_mcp_{}", hex::encode(bytes));
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.mcp_access_token = Some(token.clone());
    settings::save(&dir, &settings).map_err(|e| e.to_string())?;
    Ok(token)
}

fn find_ai_provider_mut<'a>(settings: &'a mut Settings, id: &str) -> Result<&'a mut AiProviderSetting, String> {
    settings
        .ai_providers
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("unknown AI provider \"{id}\""))
}

#[tauri::command]
async fn set_ai_provider_enabled<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    find_ai_provider_mut(&mut settings, &id)?.enabled = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_ai_provider_model<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    model: String,
) -> Result<(), String> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("model name cannot be empty".to_string());
    }
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    find_ai_provider_mut(&mut settings, &id)?.model = model;
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_ai_provider_api_key<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    key: String,
) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    find_ai_provider_mut(&mut settings, &id)?.api_key = if key.trim().is_empty() {
        None
    } else {
        Some(key.trim().to_string())
    };
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_ai_scan_assist_enabled<R: tauri::Runtime>(app: tauri::AppHandle<R>, enabled: bool) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.ai_scan_assist_enabled = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_ai_reorganize_enabled<R: tauri::Runtime>(app: tauri::AppHandle<R>, enabled: bool) -> Result<(), String> {
    let dir = app_data_dir(&app)?;
    let mut settings = settings::load(&dir);
    settings.ai_reorganize_enabled = enabled;
    settings::save(&dir, &settings).map_err(|e| e.to_string())
}

/// Detects each provider's locally-installed CLI and its sign-in state, for
/// the AI tab's "Enabled AI tools" panel (issue #252). Shelling out to three
/// CLIs takes a beat, so this is its own command rather than folded into
/// `get_settings`.
#[tauri::command]
async fn detect_ai_tools() -> Result<Vec<ai::AiToolInfo>, String> {
    tokio::task::spawn_blocking(ai::detect_all)
        .await
        .map_err(|e| e.to_string())
}

/// Confirms one provider's CLI is installed and signed in, so the AI tab can
/// show "detected" without waiting for a real feature to fail first. No API
/// key involved — detection uses the CLI's own auth state (issue #252).
#[tauri::command]
async fn test_ai_provider(id: String) -> Result<String, String> {
    let kind = ai::AiProviderKind::from_id(&id).ok_or_else(|| format!("unknown AI provider \"{id}\""))?;
    let info = tokio::task::spawn_blocking(move || ai::detect_provider(kind))
        .await
        .map_err(|e| e.to_string())?;
    if !info.installed {
        return Err(format!("{} is not installed on this machine.", info.cli_label));
    }
    if !info.signed_in {
        return Err(format!("{} is installed but not signed in — run its login command.", info.cli_label));
    }
    Ok(format!(
        "{} detected{}.",
        info.cli_label,
        if info.version.is_empty() { String::new() } else { format!(" ({})", info.version) }
    ))
}

/// Finds the first enabled provider (settings order: Claude, Codex, Grok)
/// and builds a client for it — its installed CLI when there's no saved API
/// key (the issue #252 default), or the direct HTTP API when a key is
/// present. The advanced AI features don't let a user pick per call, since
/// there is normally only one enabled anyway.
fn ai_client_from_settings(settings: &Settings) -> Result<ai::AiClient, String> {
    let provider = settings
        .ai_providers
        .iter()
        .find(|p| p.enabled)
        .ok_or_else(|| "Enable an AI provider in the AI tab first.".to_string())?;
    let kind = ai::AiProviderKind::from_id(&provider.id)
        .ok_or_else(|| format!("unknown AI provider \"{}\"", provider.id))?;
    match provider.api_key.clone().filter(|key| !key.is_empty()) {
        Some(key) => Ok(ai::AiClient::new(kind, key, provider.model.clone())),
        None => ai::AiClient::cli(kind, provider.model.clone()),
    }
}

#[tauri::command]
async fn get_status<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<ServerStatus, String> {
    state
        .core(&app)
        .await?
        .status()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_bandwidth_history<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<swarm_media::bandwidth::BandwidthSample>, String> {
    Ok(state.core(&app).await?.bandwidth_history())
}

#[tauri::command]
async fn get_transcoding_history<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<swarm_server::transcode_activity::TranscodeActivitySample>, String> {
    Ok(state.core(&app).await?.transcode_activity_history())
}

#[tauri::command]
async fn get_artwork_cache_snapshot<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<swarm_media::artwork_cache::ArtworkCacheSnapshot, String> {
    Ok(state.core(&app).await?.artwork_cache_snapshot().await)
}

#[derive(serde::Serialize)]
#[cfg_attr(test, derive(Debug))]
struct RescanResult {
    added: u64,
    updated: u64,
    removed: u64,
    unchanged: u64,
    /// Labels of roots that were changing on disk during the walk, so their
    /// contents were not reconciled this pass (#222). Empty on a clean scan.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    incomplete_roots: Vec<String>,
    /// Best-effort, bounded Plex-conformance problems found this scan
    /// (issue #247). Empty when every scanned file conforms.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    validation_issues: Vec<swarm_media::plex::PlexValidationIssue>,
}

/// `scan-progress` event name emitted to the webview during [`rescan`] —
/// best-effort bounded updates with payload shape
/// `swarm_media::scan::ScanProgressEvent`.
/// Same forwarding-task pattern as [`run_scrape`]'s `scrape-progress` — real
/// bug this fixes: a rescan over a slow network mount gave no indication
/// anything was happening until it finished, confirmed live against a real
/// ~3,700-file remote share.
const SCAN_PROGRESS_EVENT: &str = "scan-progress";

#[tauri::command]
async fn rescan<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<RescanResult, String> {
    let core = state.core(&app).await?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let emitter = app.clone();
    let forward = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let _ = emitter.emit(SCAN_PROGRESS_EVENT, event);
        }
    });
    let result = core.rescan(Some(tx)).await.map_err(|e| e.to_string());
    let _ = forward.await;
    let report = result?;
    if !report.incomplete_roots.is_empty() {
        let message = format!(
            "These media roots were changing on disk during the scan, so their contents were left untouched this pass: {}.\n\nRescan again once they have settled.",
            report.incomplete_roots.join(", "),
        );
        if let Err(error) = core
            .library
            .record_server_notification("warning", "Some media roots were busy during the scan", &message)
            .await
        {
            tracing::warn!(%error, "could not save incomplete-rescan notification");
        }
    }
    // Plex-conformance problems found this scan (issue #247). Deterministic
    // and non-fatal — the affected files were still scanned as best as
    // possible; this just tells the user which ones Plex would also
    // struggle with and how to bring them into a Plex-compatible shape.
    if !report.validation_issues.is_empty() {
        let shown = report.validation_issues.len().min(20);
        let mut message = format!(
            "{} file(s) don't fully match a Plex-supported layout. They were still scanned; \
             fixing them keeps the library portable to and from Plex.\n",
            report.validation_issues.len()
        );
        for issue in &report.validation_issues[..shown] {
            message.push_str(&format!(
                "\n• {}\n  problem: {}\n  expected: {}\n  fix: {}\n",
                issue.current_path, issue.problem, issue.expected, issue.recommended_fix
            ));
        }
        if report.validation_issues.len() > shown {
            message.push_str(&format!(
                "\n…and {} more. Use the AI tab's \"Reorganize\" scan for the full list.",
                report.validation_issues.len() - shown
            ));
        }
        if let Err(error) = core
            .library
            .record_server_notification("warning", "Some media isn't in a Plex-compatible layout", &message)
            .await
        {
            tracing::warn!(%error, "could not save media-validation notification");
        }
    }
    Ok(RescanResult {
        added: report.added,
        updated: report.updated,
        removed: report.removed,
        unchanged: report.unchanged,
        incomplete_roots: report.incomplete_roots,
        validation_issues: report.validation_issues,
    })
}

/// Re-derives every entry's classification from its already-stored path —
/// repairs entries a `classify()` bug already misfiled (wrong kind/show/
/// season/episode) without needing the underlying file to change, which a
/// plain Rescan can't do (it only re-classifies new/modified files — see
/// `Library::reclassify_all`'s doc comment). Clears stale scrape data for
/// whatever it actually corrects; run a normal Scrape metadata afterward to
/// pick those back up under their now-correct classification.
#[tauri::command]
async fn reclassify_library<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<swarm_media::store::ReclassifyReport, String> {
    let core = state.core(&app).await?;
    core.library
        .reclassify_all(&core.media_roots)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, serde::Serialize)]
struct ApplySummaryView {
    applied: u32,
    skipped: u32,
    errors: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct ReorgPlanView {
    id: u64,
    root_label: String,
    items: Vec<reorganize::ReorgItem>,
    ai_assisted_count: u32,
    conflict_count: u32,
    /// Deterministic Plex-conformance problems found in the root (issue
    /// #247) — surfaced to the AI tab alongside the proposed moves.
    validation: Vec<swarm_media::plex::PlexValidationIssue>,
    status: String,
    apply_summary: Option<ApplySummaryView>,
}

fn reorg_plan_view(
    id: u64,
    plan: &reorganize::ReorgPlan,
    status: &str,
    outcome: Option<&reorganize::ApplyOutcome>,
) -> ReorgPlanView {
    ReorgPlanView {
        id,
        root_label: plan.root_label.clone(),
        items: plan.items.clone(),
        ai_assisted_count: plan.ai_assisted_count,
        conflict_count: plan.conflict_count,
        validation: plan.validation.clone(),
        status: status.to_string(),
        apply_summary: outcome.map(|o| ApplySummaryView {
            applied: o.applied,
            skipped: o.skipped,
            errors: o.errors.clone(),
        }),
    }
}

fn resolve_media_root(settings: &Settings, root_label: &str) -> Result<PathBuf, String> {
    settings
        .media_roots
        .iter()
        .find(|r| r.label == root_label)
        .map(|r| PathBuf::from(&r.path))
        .ok_or_else(|| format!("unknown media root \"{root_label}\""))
}

/// Scans one configured media root and proposes a rename/move plan — pure
/// computation, nothing on disk changes until `approve_ai_reorg_plan` runs.
/// Gated by `ai_reorganize_enabled`; an AI provider is used only for the
/// long tail of filenames `classify` can't place at all (see
/// `reorganize::scan_root`) — if none is configured, the scan still runs,
/// just without that fallback.
#[tauri::command]
async fn ai_reorganize_scan<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    root_label: String,
) -> Result<ReorgPlanView, String> {
    let dir = app_data_dir(&app)?;
    let settings = settings::load(&dir);
    if !settings.ai_reorganize_enabled {
        return Err("Enable \"AI reorganize\" on the AI tab first.".to_string());
    }
    let root_path = resolve_media_root(&settings, &root_label)?;
    let ai_client = ai_client_from_settings(&settings).ok();
    let plan = reorganize::scan_root(&root_label, &root_path, ai_client.as_ref())
        .await
        .map_err(|e| e.to_string())?;

    let id = state.next_reorg_plan_id.fetch_add(1, Ordering::Relaxed);
    let view = reorg_plan_view(id, &plan, "proposed", None);
    state.reorg_plans.lock().await.insert(
        id,
        StoredReorgPlan {
            plan,
            root_path,
            status: "proposed",
            apply_outcome: None,
        },
    );
    Ok(view)
}

#[tauri::command]
async fn list_ai_reorg_plans(state: tauri::State<'_, AppState>) -> Result<Vec<ReorgPlanView>, String> {
    let plans = state.reorg_plans.lock().await;
    let mut views: Vec<ReorgPlanView> = plans
        .iter()
        .map(|(id, stored)| reorg_plan_view(*id, &stored.plan, stored.status, stored.apply_outcome.as_ref()))
        .collect();
    views.sort_by_key(|v| v.id);
    Ok(views)
}

/// Applies every non-conflicting item in a still-`proposed` plan (see
/// `reorganize::apply_plan` — renames only, never a delete, never an
/// overwrite), records a notification either way, and kicks off a rescan so
/// the library picks up the new layout. Filesystem work runs on a blocking
/// thread since `apply_plan` is synchronous I/O over potentially many files.
#[tauri::command]
async fn approve_ai_reorg_plan<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    id: u64,
) -> Result<ReorgPlanView, String> {
    let (root_path, items) = {
        let plans = state.reorg_plans.lock().await;
        let stored = plans.get(&id).ok_or_else(|| "reorganize plan not found".to_string())?;
        if stored.status != "proposed" {
            return Err(format!("this plan is already {}", stored.status));
        }
        (stored.root_path.clone(), stored.plan.items.clone())
    };
    let outcome = tokio::task::spawn_blocking(move || reorganize::apply_plan(&root_path, &items))
        .await
        .map_err(|e| e.to_string())?;

    let view = {
        let mut plans = state.reorg_plans.lock().await;
        let stored = plans.get_mut(&id).ok_or_else(|| "reorganize plan not found".to_string())?;
        stored.status = "applied";
        stored.apply_outcome = Some(outcome);
        reorg_plan_view(id, &stored.plan, stored.status, stored.apply_outcome.as_ref())
    };

    let core = state.core(&app).await?;
    let summary = view.apply_summary.as_ref();
    let level = if summary.is_some_and(|s| !s.errors.is_empty()) {
        "warning"
    } else {
        "success"
    };
    let message = format!(
        "Reorganized \"{}\": {} file(s) moved, {} skipped.",
        view.root_label,
        summary.map(|s| s.applied).unwrap_or(0),
        summary.map(|s| s.skipped).unwrap_or(0),
    );
    if let Err(error) = core
        .library
        .record_server_notification(level, "AI reorganize finished", &message)
        .await
    {
        tracing::warn!(%error, "could not save reorganize notification");
    }
    let _ = core.rescan(None).await;
    Ok(view)
}

#[tauri::command]
async fn reject_ai_reorg_plan(state: tauri::State<'_, AppState>, id: u64) -> Result<(), String> {
    let mut plans = state.reorg_plans.lock().await;
    let stored = plans.get_mut(&id).ok_or_else(|| "reorganize plan not found".to_string())?;
    stored.status = "rejected";
    Ok(())
}

#[derive(serde::Serialize)]
struct EntrySummary {
    entry_key: String,
    kind: String,
    title: String,
    relative_path: String,
    size: u64,
    scraped_title: Option<String>,
    episode_title: Option<String>,
    genres: Vec<String>,
    has_artwork: bool,
    // Grouping/detail fields for the Media tab's hierarchical browse view
    // (artist/album/track for music, show/season/episode for TV) — see
    // `apps/server/ui/media.js`'s `groupTracks`/`groupEpisodes`. All
    // path-derived (never scraper output), per `classify.rs`'s grouping-key
    // invariant.
    artist: Option<String>,
    album: Option<String>,
    track_number: Option<u32>,
    show_title: Option<String>,
    season: Option<u32>,
    episode: Option<u32>,
    year: Option<u32>,
    duration_secs: Option<f64>,
    cast: Vec<swarm_media::store::CastMember>,
    overview: Option<String>,
    rating: Option<String>,
    community_rating: Option<f64>,
    community_rating_votes: Option<u64>,
    like_count: u32,
}

#[tauri::command]
async fn list_entries<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<EntrySummary>, String> {
    let core = state.core(&app).await?;
    let entries = core.library.list().await.map_err(|e| e.to_string())?;
    let like_counts = core
        .library
        .like_counts()
        .await
        .map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .map(|entry| EntrySummary {
            like_count: like_counts.get(&entry.entry_key).copied().unwrap_or(0),
            entry_key: entry.entry_key,
            kind: format!("{:?}", entry.kind).to_lowercase(),
            title: entry.title,
            relative_path: entry.relative_path,
            size: entry.size,
            scraped_title: entry.scraped_title,
            episode_title: entry.episode_title,
            genres: entry.genres,
            has_artwork: entry.artwork_version > 0,
            artist: entry.artist,
            album: entry.album,
            track_number: entry.track_number,
            show_title: entry.show_title,
            season: entry.season,
            episode: entry.episode,
            year: entry.year,
            duration_secs: entry.duration_secs,
            cast: entry.cast,
            overview: entry.overview,
            rating: entry.rating,
            community_rating: entry.community_rating,
            community_rating_votes: entry.community_rating_votes,
        })
        .collect())
}

/// Permanently remove one catalog entry and its server-managed files. The
/// destructive filesystem work lives in `ServerCore`, where it can share
/// scan serialization and be exercised independently of the webview.
#[tauri::command]
async fn delete_asset<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
) -> Result<swarm_server::DeleteAssetReport, String> {
    let core = state.core(&app).await?;
    core.delete_asset(&entry_key)
        .await
        .map_err(|error| error.to_string())
}

/// Per-asset artwork/subtitle/lyrics presence and TheIntroDB scrape state
/// for the Media detail page's "metadata & artwork" completeness checklist,
/// plus the file counts the delete-asset confirmation modal spells out
/// before anything is removed. Everything else the checklist shows (title,
/// year, cast, rating, …) is already on the `EntrySummary` the webview
/// holds, so it is not repeated here.
#[derive(serde::Serialize)]
struct AssetDetail {
    artwork_present: Vec<String>,
    subtitle_languages: Vec<String>,
    has_lyrics: bool,
    /// Whether the scraper has ever recorded a TheIntroDB lookup for this
    /// asset, and how many skip markers it cached. `introdb_checked` false
    /// means "not scraped yet"; true with a zero count means TheIntroDB was
    /// queried and had no accepted data for the asset.
    introdb_checked: bool,
    introdb_segment_count: usize,
    delete_unshared_artwork_count: usize,
    delete_shared_artwork_count: usize,
    delete_subtitle_count: usize,
}

#[tauri::command]
async fn get_asset_detail<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
) -> Result<AssetDetail, String> {
    use swarm_media::store::ArtworkKind;
    let core = state.core(&app).await?;
    let mut artwork_present = Vec::new();
    for kind in [
        ArtworkKind::Poster,
        ArtworkKind::SeasonPoster,
        ArtworkKind::Backdrop,
        ArtworkKind::Cover,
        ArtworkKind::ArtistPhoto,
    ] {
        if core
            .library
            .artwork(&entry_key, kind)
            .await
            .map_err(|e| e.to_string())?
            .is_some()
        {
            artwork_present.push(kind.route_segment().to_string());
        }
    }
    let subtitle_tracks = core
        .library
        .subtitle_tracks(&entry_key)
        .await
        .map_err(|e| e.to_string())?;
    let mut subtitle_languages: Vec<String> =
        subtitle_tracks.iter().map(|t| t.language.clone()).collect();
    subtitle_languages.sort();
    subtitle_languages.dedup();
    let has_lyrics = core
        .library
        .track_lyrics(&entry_key)
        .await
        .map_err(|e| e.to_string())?
        .is_some();
    let introdb_segments = core
        .library
        .introdb_segments_for(&entry_key)
        .await
        .map_err(|e| e.to_string())?;
    let introdb_checked = introdb_segments.is_some();
    let introdb_segment_count = introdb_segments.map_or(0, |s| s.len());
    let manifest = core
        .library
        .asset_deletion_manifest(&entry_key)
        .await
        .map_err(|e| e.to_string())?;
    let (delete_unshared_artwork_count, delete_shared_artwork_count) = manifest
        .as_ref()
        .map(|m| {
            (
                m.unshared_artwork_paths.len(),
                m.artwork_paths.len() - m.unshared_artwork_paths.len(),
            )
        })
        .unwrap_or((0, 0));
    Ok(AssetDetail {
        artwork_present,
        subtitle_languages,
        has_lyrics,
        introdb_checked,
        introdb_segment_count,
        delete_unshared_artwork_count,
        delete_shared_artwork_count,
        delete_subtitle_count: subtitle_tracks.len(),
    })
}

/// Every distinct genre/category value currently in use anywhere in the
/// library — backs the Media tab's category picker, see
/// `Library::distinct_genres`'s doc comment for why genres double as
/// categories rather than this being a separate concept.
#[tauri::command]
async fn list_categories<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let core = state.core(&app).await?;
    core.library
        .distinct_genres()
        .await
        .map_err(|e| e.to_string())
}

/// Raw bytes for one entry's artwork slot, for the Media tab's browse view to
/// render as an `<img>` — this GUI runs in the same process as the media
/// server but `/art/{entry_key}/{kind}` is only reachable over the P2P QUIC
/// peer protocol (see `docs/PROTOCOL.md`), which a webview can't speak
/// directly, so this reads the file straight off disk instead. `Ok(None)`
/// means no artwork of that kind was ever scraped/uploaded — not an error.
#[tauri::command]
async fn get_artwork_bytes<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
    kind: String,
) -> Result<Option<Vec<u8>>, String> {
    let core = state.core(&app).await?;
    let artwork_kind = swarm_media::store::ArtworkKind::parse(&kind)
        .ok_or_else(|| format!("unknown artwork kind \"{kind}\""))?;
    let Some((relative_path, _version)) = core
        .library
        .artwork(&entry_key, artwork_kind)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let path = core.media_roots.resolve(&relative_path);
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// `scrape-progress` event name emitted to the webview during
/// [`run_scrape`] — one per entry, payload shape is `ScrapeProgressEvent`.
/// The frontend listens via `window.__TAURI__.event.listen`.
const SCRAPE_PROGRESS_EVENT: &str = "scrape-progress";

const LIBRARY_MAINTENANCE_PROGRESS_EVENT: &str = "library-maintenance-progress";

#[derive(Clone, serde::Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
enum LibraryMaintenanceProgressEvent {
    Scanning {
        progress: ScanProgressEvent,
    },
    Scraping {
        progress: Option<ScrapeProgressEvent>,
    },
    FixingClassifications,
}

#[derive(serde::Serialize)]
struct LibraryMaintenanceResult {
    scan: RescanResult,
    scrape: BulkScrapeReport,
    classifications: swarm_media::store::ReclassifyReport,
}

/// Performs the complete browse-page maintenance sequence as one operation:
/// scan, scrape, then classification repair. `force` selects whether the
/// scrape replaces existing metadata or fills only missing fields.
#[tauri::command]
async fn run_library_maintenance<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    force: bool,
) -> Result<LibraryMaintenanceResult, String> {
    let cancel = {
        let mut active = state.library_maintenance_cancel.lock().await;
        if active.is_some() {
            return Err("library maintenance is already running".to_string());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *active = Some(Arc::clone(&cancel));
        cancel
    };

    let result = async {
        let core = state.core(&app).await?;

        let (scan_tx, mut scan_rx) = tokio::sync::mpsc::channel(64);
        let scan_emitter = app.clone();
        let scan_forward = tokio::spawn(async move {
            while let Some(progress) = scan_rx.recv().await {
                let _ = scan_emitter.emit(
                    LIBRARY_MAINTENANCE_PROGRESS_EVENT,
                    LibraryMaintenanceProgressEvent::Scanning { progress },
                );
            }
        });
        let scan_result = core
            .rescan_cancellable(Some(scan_tx), Arc::clone(&cancel))
            .await;
        let _ = scan_forward.await;
        if cancel.load(Ordering::Acquire) {
            return Err("cancelled".to_string());
        }
        let scan = scan_result.map_err(|error| error.to_string())?;

        let _ = app.emit(
            LIBRARY_MAINTENANCE_PROGRESS_EVENT,
            LibraryMaintenanceProgressEvent::Scraping { progress: None },
        );
        let tmdb_api_key = settings::load(&app_data_dir(&app)?).tmdb_api_key;
        let (scrape_tx, mut scrape_rx) = tokio::sync::mpsc::unbounded_channel();
        let scrape_emitter = app.clone();
        let scrape_forward = tokio::spawn(async move {
            while let Some(progress) = scrape_rx.recv().await {
                let _ = scrape_emitter.emit(
                    LIBRARY_MAINTENANCE_PROGRESS_EVENT,
                    LibraryMaintenanceProgressEvent::Scraping {
                        progress: Some(progress),
                    },
                );
            }
        });
        let scrape_result = core
            .run_scrape_cancellable(
                ScrapeConfig {
                    tmdb_api_key,
                    ..Default::default()
                },
                Some(scrape_tx),
                force,
                Arc::clone(&cancel),
            )
            .await;
        let _ = scrape_forward.await;
        if cancel.load(Ordering::Acquire) {
            return Err("cancelled".to_string());
        }
        if let Ok(report) = &scrape_result {
            *state.last_scrape_issues.lock().await = report.issues.clone();
        }
        record_scrape_result_notification(
            &core,
            &scrape_result,
            "Metadata scrape finished with issues",
            "Metadata scrape failed",
        )
        .await;
        let scrape = scrape_result.map_err(|error| error.to_string())?;

        let _ = app.emit(
            LIBRARY_MAINTENANCE_PROGRESS_EVENT,
            LibraryMaintenanceProgressEvent::FixingClassifications,
        );
        if cancel.load(Ordering::Acquire) {
            return Err("cancelled".to_string());
        }
        let classifications = core
            .library
            .reclassify_all(&core.media_roots)
            .await
            .map_err(|error| error.to_string())?;
        if cancel.load(Ordering::Acquire) {
            return Err("cancelled".to_string());
        }

        Ok(LibraryMaintenanceResult {
            scan: RescanResult {
                added: scan.added,
                updated: scan.updated,
                removed: scan.removed,
                unchanged: scan.unchanged,
                incomplete_roots: scan.incomplete_roots,
                validation_issues: scan.validation_issues,
            },
            scrape,
            classifications,
        })
    }
    .await;

    let mut active = state.library_maintenance_cancel.lock().await;
    if active
        .as_ref()
        .is_some_and(|active| Arc::ptr_eq(active, &cancel))
    {
        *active = None;
    }
    result
}

#[tauri::command]
async fn cancel_library_maintenance(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    let active = state.library_maintenance_cancel.lock().await;
    if let Some(cancel) = active.as_ref() {
        cancel.store(true, Ordering::Release);
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
async fn run_scrape<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    force: bool,
) -> Result<BulkScrapeReport, String> {
    let core = state.core(&app).await?;
    let tmdb_api_key = settings::load(&app_data_dir(&app)?).tmdb_api_key;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let emitter = app.clone();
    // Forwarding task: relays each progress event to the webview as it
    // arrives, independent of `run_scrape` itself, which is only awaited
    // for its final report below.
    let forward = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let _ = emitter.emit(SCRAPE_PROGRESS_EVENT, event);
        }
    });
    let result = core
        .run_scrape(
            ScrapeConfig {
                tmdb_api_key,
                ..Default::default()
            },
            Some(tx),
            force,
        )
        .await;
    // Dropping the last sender (above) closes the channel, so `forward`
    // exits its loop on its own — awaiting it here just makes sure every
    // already-queued event is actually emitted before this command returns
    // its final report, so the frontend can't see "done" before the last
    // per-entry update.
    let _ = forward.await;
    if let Ok(report) = &result {
        *state.last_scrape_issues.lock().await = report.issues.clone();
    }
    record_scrape_result_notification(
        &core,
        &result,
        "Metadata scrape finished with issues",
        "Metadata scrape failed",
    )
    .await;
    result.map_err(|e| e.to_string())
}

/// Pinpoint rescrape of one entry, optionally against a manual TMDb id/URL
/// override (music entries ignore `tmdb_url` — no TMDb concept there).
#[tauri::command]
async fn rescrape_entry<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
    tmdb_url: Option<String>,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    let tmdb_api_key = settings::load(&app_data_dir(&app)?).tmdb_api_key;
    let config = ScrapeConfig {
        tmdb_api_key,
        ..Default::default()
    };
    let tmdb_override = tmdb_url
        .filter(|u| !u.trim().is_empty())
        .map(swarm_media::scrape::TmdbOverride::Url);
    core.rescrape_entry(&entry_key, config, tmdb_override)
        .await
        .map_err(|e| e.to_string())
}

/// Entries the most recent `run_scrape`/library-maintenance pass couldn't
/// match, for the AI tab's "Ask AI" affordance — see `ai_scrape_assist`.
/// Empty if no scrape has run yet this session.
#[tauri::command]
async fn list_scrape_issues(state: tauri::State<'_, AppState>) -> Result<Vec<ScrapeIssue>, String> {
    Ok(state.last_scrape_issues.lock().await.clone())
}

#[derive(serde::Deserialize)]
struct AiTitleGuess {
    title: String,
    #[serde(default)]
    year: Option<u32>,
}

#[derive(Debug, serde::Serialize)]
struct AiScrapeSuggestion {
    suggested_title: String,
    suggested_year: Option<u32>,
    tmdb_id: u64,
    tmdb_title: String,
    tmdb_url: String,
    poster_url: Option<String>,
}

/// Asks the configured AI provider to guess a clean title/year for an entry
/// the ordinary scrape pass couldn't match on TMDb, then retries the TMDb
/// search with that guess. Never writes anything itself — it only returns a
/// suggestion; the frontend applies it (if the user accepts) through the
/// existing `rescrape_entry` command, exactly like a manual TMDb URL fix.
#[tauri::command]
async fn ai_scrape_assist<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
) -> Result<AiScrapeSuggestion, String> {
    let dir = app_data_dir(&app)?;
    let settings = settings::load(&dir);
    if !settings.ai_scan_assist_enabled {
        return Err("Enable \"AI scan & scrape assist\" on the AI tab first.".to_string());
    }
    let client = ai_client_from_settings(&settings)?;
    let tmdb_api_key = settings
        .tmdb_api_key
        .clone()
        .ok_or_else(|| "Add a TMDb API key on the Details tab before using scan assist.".to_string())?;

    let core = state.core(&app).await?;
    let entry = core
        .library
        .get(&entry_key)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "entry not found".to_string())?;
    if entry.kind == MediaKind::Track {
        return Err("AI scan assist only helps with movies and TV episodes right now.".to_string());
    }

    let filename = entry.relative_path.rsplit('/').next().unwrap_or(&entry.relative_path);
    let system = "You help identify movies and TV shows from messy filenames for a personal media \
        server. Reply with ONLY a JSON object, no prose, no code fences.";
    let user = format!(
        "Filename: \"{filename}\"\n\
        Parsed title guess: \"{}\"\n\
        Parsed year guess: {}\n\
        Media kind: {}\n\n\
        Reply with a JSON object exactly like {{\"title\": \"<best-guess canonical title>\", \
        \"year\": <release year or null>}}.",
        entry.title,
        entry.year.map(|y| y.to_string()).unwrap_or_else(|| "unknown".to_string()),
        if entry.kind == MediaKind::Episode { "TV show" } else { "movie" },
    );
    let reply = client.complete(system, &user).await.map_err(|e| e.to_string())?;
    let guess: AiTitleGuess =
        ai::parse_json_object(&reply).ok_or_else(|| format!("AI reply was not valid JSON: {reply}"))?;
    if guess.title.trim().is_empty() {
        return Err("AI could not suggest a title for this file.".to_string());
    }

    let tmdb = swarm_media::scrape::tmdb::TmdbClient::new(tmdb_api_key);
    let scraped = if entry.kind == MediaKind::Episode {
        tmdb.search_and_fetch_tv(guess.title.trim()).await
    } else {
        tmdb.search_and_fetch_movie(guess.title.trim(), guess.year).await
    }
    .map_err(|e| format!("AI suggested \"{}\" but the TMDb lookup failed: {e}", guess.title))?;

    let tmdb_url = format!(
        "https://www.themoviedb.org/{}/{}",
        if entry.kind == MediaKind::Episode { "tv" } else { "movie" },
        scraped.tmdb_id
    );
    Ok(AiScrapeSuggestion {
        suggested_title: guess.title,
        suggested_year: guess.year,
        tmdb_id: scraped.tmdb_id,
        tmdb_title: scraped.title,
        tmdb_url,
        poster_url: scraped.poster_url,
    })
}

/// Manually override an entry's display title, genre/category list,
/// synopsis, and/or content rating. `None` (omitted from the JS call) leaves
/// that field untouched — see `Library::set_manual_metadata`/
/// `Library::set_overview`/`Library::set_rating`. Never affects grouping
/// (artist/album/show/season/episode), which stays path-derived.
#[tauri::command]
async fn set_manual_metadata<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
    title: Option<String>,
    genres: Option<Vec<String>>,
    overview: Option<String>,
    rating: Option<String>,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    core.library
        .set_manual_metadata(&entry_key, title.as_deref(), genres.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    if let Some(overview) = overview {
        core.library
            .set_overview(&entry_key, &overview)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(rating) = rating {
        core.library
            .set_rating(&entry_key, &rating)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Manually move an entry to a different asset type ("movie"/"episode"/
/// "track") — see `Library::set_manual_kind`'s doc comment for why this
/// exists (a music video sitting under `movies/` or `shows/` as an .mkv,
/// indistinguishable from a real movie/episode by path or extension alone).
/// `artist`/`album` matter only when `kind` is "track"; `show_title` only
/// when it's "episode" — both ignored otherwise.
#[tauri::command]
async fn set_manual_kind<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
    kind: String,
    artist: Option<String>,
    album: Option<String>,
    show_title: Option<String>,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    let kind = match kind.as_str() {
        "movie" => MediaKind::Movie,
        "episode" => MediaKind::Episode,
        "track" => MediaKind::Track,
        other => return Err(format!("unknown asset kind \"{other}\"")),
    };
    core.library
        .set_manual_kind(
            &entry_key,
            kind,
            artist.as_deref(),
            album.as_deref(),
            show_title.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())
}

/// Manually uploaded artwork bytes for one entry/kind — the same on-disk
/// convention (`images/` sibling folder) and `Library::set_artwork` call the
/// scraper itself uses, just sourced from a file picker instead of a
/// download. `extension` (no leading dot, e.g. `"png"`) comes from whatever
/// file the user picked, since manually uploaded art isn't always a jpg like
/// every scraped image is today.
#[tauri::command]
async fn upload_artwork<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
    kind: String,
    extension: String,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    let artwork_kind = swarm_media::store::ArtworkKind::parse(&kind)
        .ok_or_else(|| format!("unknown artwork kind \"{kind}\""))?;
    let extension = extension.trim().trim_start_matches('.').to_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        return Err(format!("unsupported image extension \"{extension}\""));
    }
    let entry = core
        .library
        .get(&entry_key)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such entry")?;
    let filename = format!("manual-{}.{extension}", artwork_kind.route_segment());
    let relative = swarm_media::scrape::artwork::save_artwork(
        &core.media_roots,
        &entry.relative_path,
        &filename,
        &bytes,
    )
    .await
    .map_err(|e| e.to_string())?;
    core.library
        .set_artwork(&entry_key, artwork_kind, &relative)
        .await
        .map_err(|e| e.to_string())
}

/// Manually uploaded artwork shared across a whole client-side-computed
/// group — an artist (every track by that artist, across every album) or an
/// album (every track in it) — rather than one entry, since neither has an
/// `entry_key` of its own (the Netflix-style hierarchy is grouped entirely
/// client-side over the flat entry list; see the plan's "hierarchy is
/// grouped client-side" decision). The frontend already knows exactly which
/// entries belong to the group (it just built the grouping to render the
/// page), so it passes their keys directly rather than this command
/// re-deriving artist/album matches itself. Same on-disk convention as
/// [upload_artwork] and the scraper's own [`crate`]-external
/// `scrape_one_album_group` (`swarm_media::scrape::runner`): the file is
/// saved once, beside the *first* entry, and every entry in the group
/// stores that same shared `relative_path` — safe regardless of whether the
/// group's tracks span multiple folders, since artwork is always resolved
/// root-relative (`get_artwork_bytes` above), never relative to the
/// referencing entry's own folder.
#[tauri::command]
async fn upload_group_artwork<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_keys: Vec<String>,
    kind: String,
    extension: String,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    let artwork_kind = swarm_media::store::ArtworkKind::parse(&kind)
        .ok_or_else(|| format!("unknown artwork kind \"{kind}\""))?;
    let extension = extension.trim().trim_start_matches('.').to_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        return Err(format!("unsupported image extension \"{extension}\""));
    }
    let Some(first_key) = entry_keys.first() else {
        return Err("no entries to attach artwork to".to_string());
    };
    let first = core
        .library
        .get(first_key)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such entry")?;
    let filename = format!("manual-{}.{extension}", artwork_kind.route_segment());
    let relative = swarm_media::scrape::artwork::save_artwork(
        &core.media_roots,
        &first.relative_path,
        &filename,
        &bytes,
    )
    .await
    .map_err(|e| e.to_string())?;
    for entry_key in &entry_keys {
        core.library
            .set_artwork(entry_key, artwork_kind, &relative)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Reverts a bad scrape (wrong TMDb/MusicBrainz match, bad manual edit) —
/// clears the scraped title/genres/cast and every artwork slot, and puts the
/// entry back into `missing_scrape` so a plain (non-force) bulk scrape or a
/// pinpoint rescrape will pick it up fresh. Also best-effort deletes the
/// now-orphaned artwork files from disk; a deletion failure (e.g. the file
/// was already gone, or a flaky network mount) never fails the command
/// itself — the database state is what actually matters here.
#[tauri::command]
async fn clear_scraped_metadata<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    entry_key: String,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    let cleared_paths = core
        .library
        .clear_scrape_result(&entry_key)
        .await
        .map_err(|e| e.to_string())?;
    for relative_path in cleared_paths {
        let path = core.media_roots.resolve(&relative_path);
        let _ = tokio::fs::remove_file(&path).await;
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct SwarmSummaryView {
    id: String,
    name: String,
}

#[derive(serde::Serialize)]
struct SwarmLinkView {
    base_url: String,
    swarms: Vec<SwarmSummaryView>,
    allowed_peer_count: usize,
}

#[tauri::command]
async fn get_swarm_link<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Option<SwarmLinkView>, String> {
    let core = state.core(&app).await?;
    let Some(link) = core.stun_link().await else {
        return Ok(None);
    };
    Ok(Some(SwarmLinkView {
        base_url: link.base_url,
        swarms: link
            .swarms
            .into_iter()
            .map(|s| SwarmSummaryView {
                id: s.id,
                name: s.name,
            })
            .collect(),
        allowed_peer_count: core.allowed.len(),
    }))
}

/// Accepts the short-lived code displayed by a TV discovered on the LAN.
/// This approval path is entirely local and independent of the SWARM service.
#[tauri::command]
async fn approve_lan_pairing<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    code: String,
) -> Result<swarm_server::lan::LanPairingApproval, String> {
    let core = state.core(&app).await?;
    let approval = core
        .approve_lan_pairing(&code)
        .await
        .map_err(|error| error.to_string())?;
    if let Err(error) = core
        .library
        .record_server_notification(
            "success",
            "TV approved over LAN",
            &format!(
                "{} was approved. The TV will connect automatically.",
                approval.name
            ),
        )
        .await
    {
        tracing::warn!(%error, "could not save LAN pairing notification");
    }
    Ok(approval)
}

#[tauri::command]
async fn list_local_peers<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<swarm_server::LocalPeerRecord>, String> {
    let core = state.core(&app).await?;
    core.local_peers().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn revoke_local_peer<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    fingerprint: String,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    core.revoke_local_peer(&fingerprint)
        .await
        .map_err(|e| e.to_string())
}

/// Accepts the short-lived code displayed by an HTTP-only (Roku-class)
/// device — the plain-HTTP counterpart of `approve_lan_pairing` above, see
/// `http_media.rs`'s module doc comment for how the two flows differ.
#[tauri::command]
async fn approve_http_media_pairing<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    code: String,
) -> Result<String, String> {
    let core = state.core(&app).await?;
    let (name, _token) = core
        .approve_http_media_pairing(&code)
        .await
        .map_err(|error| error.to_string())?;
    if let Err(error) = core
        .library
        .record_server_notification(
            "success",
            "Device approved",
            &format!("{name} was approved. It will connect automatically."),
        )
        .await
    {
        tracing::warn!(%error, "could not save HTTP media pairing notification");
    }
    // The raw token is handed to the device itself via its next /pair/poll
    // (see http_media.rs) — this command deliberately returns only the
    // name, not the token, since nothing in the desktop UI needs to display
    // or retype it.
    Ok(name)
}

#[tauri::command]
async fn list_http_media_devices<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<swarm_server::HttpMediaDeviceRecord>, String> {
    let core = state.core(&app).await?;
    core.http_media_devices().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn revoke_http_media_device<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    token_hash: String,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    core.revoke_http_media_device(&token_hash)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn lookup_tv_activation<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    code: String,
) -> Result<swarm_core::rest::ActivationPreview, String> {
    let core = state.core(&app).await?;
    core.lookup_activation(&code)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn approve_tv_activation<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    activation_id: String,
) -> Result<swarm_core::rest::ActivationStatusResponse, String> {
    let core = state.core(&app).await?;
    core.approve_activation(&activation_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn resync_swarm<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<usize, String> {
    let core = state.core(&app).await?;
    core.resync().await.map_err(|e| e.to_string())
}

/// Leave one joined swarm, keeping the STUN link (and other memberships)
/// intact — see `ServerCore::leave_swarm`.
#[tauri::command]
async fn leave_swarm<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    core.leave_swarm(&swarm_id).await.map_err(|e| e.to_string())
}

/// One joined swarm's full device roster (type, online status, fingerprint,
/// last-seen, free-form metadata) — see `ServerCore::swarm_devices`. Passcode
/// *generation* isn't exposed here: the STUN server only allows the swarm's
/// owning user (a session-cookie browser login) to mint join codes, and this
/// device only ever holds a Bearer access token — that's a real auth-model
/// gap, not an oversight, and codes are generated from the STUN server's own
/// admin page today (see `get_swarm_link`'s `base_url`).
#[tauri::command]
async fn get_swarm_devices<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    swarm_id: String,
) -> Result<swarm_core::rest::SwarmDevicesResponse, String> {
    let core = state.core(&app).await?;
    core.swarm_devices(&swarm_id)
        .await
        .map_err(|e| e.to_string())
}

/// Wire shape for the swarm page's Errors panel — a plain struct rather than
/// reusing `swarm_media::store::ClientErrorRecord` directly so the frontend
/// gets stable camelCase field names independent of the Rust struct's own
/// naming, same pattern as `EntrySummary` above.
#[derive(serde::Serialize)]
struct ClientErrorView {
    id: i64,
    device_id: String,
    device_name: String,
    entry_key: Option<String>,
    asset_title: Option<String>,
    kind: Option<String>,
    message: String,
    context: Option<String>,
    occurred_at_ms: i64,
    received_at_ms: i64,
    resolution_comments: Option<String>,
    resolved_at_ms: Option<i64>,
    dismissed_at_ms: Option<i64>,
}

impl From<swarm_media::store::ClientErrorRecord> for ClientErrorView {
    fn from(r: swarm_media::store::ClientErrorRecord) -> Self {
        Self {
            id: r.id,
            device_id: r.device_id,
            device_name: r.device_name,
            entry_key: r.entry_key,
            asset_title: r.asset_title,
            kind: r.kind,
            message: r.message,
            context: r.context,
            occurred_at_ms: r.occurred_at_ms,
            received_at_ms: r.received_at_ms,
            resolution_comments: r.resolution_comments,
            resolved_at_ms: r.resolved_at_ms,
            dismissed_at_ms: r.dismissed_at_ms,
        }
    }
}

/// Client-reported errors (playback failures, etc. — see
/// `swarm_core::peer::ClientErrorReport`), newest first, for the swarm
/// page's Errors panel.
#[tauri::command]
async fn list_client_errors<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ClientErrorView>, String> {
    let core = state.core(&app).await?;
    let errors = core
        .library
        .list_client_errors()
        .await
        .map_err(|e| e.to_string())?;
    Ok(errors.into_iter().map(ClientErrorView::from).collect())
}

/// Backs the swarm page's notification bubble — polled separately from
/// [`list_client_errors`] so the badge can refresh cheaply without pulling
/// every error's full body down each time.
#[tauri::command]
async fn client_error_count<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<u64, String> {
    let core = state.core(&app).await?;
    core.library
        .client_error_count()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_client_error<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    id: i64,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    core.library
        .delete_client_error(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn resolve_client_error<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    id: i64,
    comments: Option<String>,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    let resolved = core
        .library
        .resolve_client_error(id, comments.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    if resolved {
        Ok(())
    } else {
        Err("That client problem was already resolved or no longer exists.".into())
    }
}

#[tauri::command]
async fn clear_client_errors<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    core.library
        .clear_client_errors()
        .await
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct ServerNotificationView {
    id: i64,
    level: String,
    title: String,
    message: String,
    created_at_ms: i64,
}

impl From<swarm_media::store::ServerNotificationRecord> for ServerNotificationView {
    fn from(notification: swarm_media::store::ServerNotificationRecord) -> Self {
        Self {
            id: notification.id,
            level: notification.level,
            title: notification.title,
            message: notification.message,
            created_at_ms: notification.created_at_ms,
        }
    }
}

#[tauri::command]
async fn list_server_notifications<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ServerNotificationView>, String> {
    let core = state.core(&app).await?;
    core.library
        .list_server_notifications()
        .await
        .map(|notifications| {
            notifications
                .into_iter()
                .map(ServerNotificationView::from)
                .collect()
        })
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn notification_count<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<u64, String> {
    let core = state.core(&app).await?;
    let server = core
        .library
        .server_notification_count()
        .await
        .map_err(|error| error.to_string())?;
    let client = core
        .library
        .client_error_count()
        .await
        .map_err(|error| error.to_string())?;
    Ok(server + client)
}

#[tauri::command]
async fn delete_server_notification<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    id: i64,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    core.library
        .delete_server_notification(id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn clear_server_notifications<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let core = state.core(&app).await?;
    core.library
        .clear_server_notifications()
        .await
        .map_err(|error| error.to_string())
}

// The dashboard's info modal (apps/server/ui/app.js's INFO_TOPICS) links out to
// external resources (protocol/standard explainers) for the curious — a plain
// `<a target="_blank">` doesn't open the OS's default browser from inside a
// Tauri webview the way it would in a real browser tab; it's either silently
// swallowed or, at best, opens a second app window pointed at an external
// site, neither of which is what "learn more" should do. The opener plugin's
// `open_url` is the actual supported way to hand a URL to the OS. Called from
// a plain app command (not invoked as `plugin:opener|open_url` directly from
// JS) so no extra capability/permission entry is needed — same reasoning
// `choose_media_folder` below wraps the dialog plugin instead of exposing it
// to JS directly.
#[tauri::command]
fn open_external_url<R: tauri::Runtime>(app: tauri::AppHandle<R>, url: String) -> Result<(), String> {
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

const MAIN_WINDOW_LABEL: &str = "main";

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Hiding the desktop window deliberately leaves [`AppState`] and its
/// [`ServerCore`] alive. Playback, LAN discovery, and remote connections keep
/// running until the user chooses Quit from the tray menu.
#[tauri::command]
fn hide_to_tray<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    app.get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "main window is unavailable".to_string())?
        .hide()
        .map_err(|error| error.to_string())
}

fn install_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "tray-show", "Show SWARM", true, None::<&str>)?;
    let running = MenuItem::with_id(
        app,
        "tray-running",
        "Media server keeps this computer awake",
        false,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "tray-quit", "Quit SWARM", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &running, &separator, &quit])?;

    let mut tray = TrayIconBuilder::with_id("swarm-server")
        .menu(&menu)
        .tooltip("SWARM Media Server")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray-show" => show_main_window(app),
            "tray-quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.build(app)?;
    Ok(())
}

fn main() {
    let app = tauri::Builder::default()
        // Must be registered first: a second launch simply reveals the
        // already-running window instead of starting a competing server.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            init_logging(app.handle());
            install_tray(app)?;
            spawn_startup_update_check(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == MAIN_WINDOW_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .manage(AppState {
            core: OnceCell::new(),
            library_maintenance_cancel: Mutex::new(None),
            _sleep_inhibitor: acquire_sleep_inhibitor(),
            test_data_dir: None,
            test_bind_override: None,
            last_scrape_issues: Mutex::new(Vec::new()),
            reorg_plans: Mutex::new(HashMap::new()),
            next_reorg_plan_id: AtomicU64::new(1),
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_media_root_health,
            hide_to_tray,
            open_external_url,
            choose_media_folder,
            pick_folder_path,
            pick_file_path,
            read_file_bytes,
            list_media_roots,
            add_media_root,
            connect_smb_root,
            repair_smb_root,
            remove_media_root,
            set_tmdb_api_key,
            set_opensubtitles_api_key,
            download_subtitle,
            set_streaming_upload_budget_enabled,
            set_artwork_disk_cache_enabled,
            set_auto_library_watch_enabled,
            set_auto_update,
            check_for_update,
            install_update,
            set_video_encoder_mode,
            set_max_transcode_height,
            set_hls_segment_seconds,
            set_local_transcription_enabled,
            set_transcription_pause_while_streaming,
            set_transcription_skip_if_subtitles_exist,
            generate_subtitles_for_entry,
            get_transcription_status,
            set_mcp_enabled,
            set_mcp_port,
            generate_mcp_access_token,
            get_status,
            get_bandwidth_history,
            get_transcoding_history,
            get_artwork_cache_snapshot,
            rescan,
            reclassify_library,
            list_entries,
            delete_asset,
            get_asset_detail,
            list_categories,
            get_artwork_bytes,
            run_library_maintenance,
            cancel_library_maintenance,
            run_scrape,
            rescrape_entry,
            set_manual_metadata,
            set_manual_kind,
            upload_artwork,
            upload_group_artwork,
            clear_scraped_metadata,
            get_swarm_link,
            approve_lan_pairing,
            list_local_peers,
            revoke_local_peer,
            approve_http_media_pairing,
            list_http_media_devices,
            revoke_http_media_device,
            lookup_tv_activation,
            approve_tv_activation,
            resync_swarm,
            leave_swarm,
            get_swarm_devices,
            list_client_errors,
            client_error_count,
            delete_client_error,
            resolve_client_error,
            clear_client_errors,
            list_server_notifications,
            notification_count,
            delete_server_notification,
            clear_server_notifications,
            set_ai_provider_enabled,
            set_ai_provider_model,
            set_ai_provider_api_key,
            set_ai_scan_assist_enabled,
            set_ai_reorganize_enabled,
            test_ai_provider,
            detect_ai_tools,
            list_scrape_issues,
            ai_scrape_assist,
            ai_reorganize_scan,
            list_ai_reorg_plans,
            approve_ai_reorg_plan,
            reject_ai_reorg_plan,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build SWARM Server");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            show_main_window(app);
        }
    });
}

/// Server-side UAT coverage: real `#[tauri::command]` handlers invoked
/// directly against a real, isolated `AppState`/SQLite/filesystem behind a
/// mocked Tauri runtime (`tauri::test::mock_builder`/`mock_context`) —
/// see `gui_tests/harness.rs` for why this shape, not the real native UI
/// (the platform's WebDriver story has no macOS backend) and not Tauri's
/// simulated webview IPC/ACL pipeline (that's Tauri's own framework code,
/// not this app's logic). Read `swarm-media-server-uat-tests` (skill)
/// before changing test logic here — same standing policy as the TV suites.
#[cfg(test)]
#[path = "gui_tests/mod.rs"]
mod gui_tests;
