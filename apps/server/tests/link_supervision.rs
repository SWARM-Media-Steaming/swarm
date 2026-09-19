//! The media server's link to the SWARM service must recover on its own and
//! must say so when it cannot.
//!
//! The incident these tests come from: a server started (or moved) while the
//! SWARM service was unreachable. The link was attempted once at startup,
//! failed with a single log line, and was never retried — so every
//! SWARM-paired TV showed the server offline for days while everything local
//! looked healthy and the UI said nothing.
//!
//! The service runs on its own thread and runtime so a test can kill it the
//! way an outage does: every socket to it closes at once, live signaling
//! sessions included. Aborting an in-process `axum::serve` task would leave
//! its connection tasks running and prove nothing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use stun_server::config::Config as StunConfig;
use stun_server::email::Mailer;
use stun_server::hub::Hub;
use stun_server::routes::build_router;
use stun_server::security::BruteForceBlocker;
use stun_server::state::AppState;
use swarm_core::rest::{DeviceRegistration, DeviceType};
use swarm_media::roots::MediaRoot;
use swarm_server::link::{SwarmLinkState, SwarmLinkStatus};
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};

// Full-process fixtures (mDNS, QUIC, HTTP, SQLite) that compete when run
// concurrently — same reason `stun_roster_sync.rs` serializes its cases. This
// also protects the process-wide timing variables set below.
static INTEGRATION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Makes the supervisor's retries take milliseconds instead of seconds. Read
/// once per `ServerCore::start`, and only set while holding the test lock.
fn fast_timing() {
    std::env::set_var("SWARM_LINK_RETRY_INITIAL_MS", "100");
    std::env::set_var("SWARM_LINK_RETRY_MAX_MS", "200");
    std::env::set_var("SWARM_LINK_CHECK_MS", "100");
    std::env::set_var("SWARM_LINK_ATTEMPT_TIMEOUT_MS", "3000");
    std::env::set_var("SWARM_LINK_STARTUP_WAIT_MS", "1500");
    // Off unless a test opts in: dormancy slows retries, which would make the
    // recovery tests wait on it.
    std::env::set_var("SWARM_LINK_DORMANT_AFTER_MS", "3600000");
    std::env::set_var("SWARM_LINK_DORMANT_RETRY_MS", "600000");
}

fn tv(name: &str, fingerprint_byte: &str) -> DeviceRegistration {
    DeviceRegistration {
        name: name.into(),
        device_type: DeviceType::Client,
        machine_id: format!("link-test-{name}"),
        cert_fingerprint: fingerprint_byte.repeat(32),
        platform: "android-tv".into(),
        app_version: "test".into(),
        metadata: Default::default(),
    }
}

/// An address nothing is listening on yet, but that a service can be started
/// on later.
fn reserve_addr() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

/// A SWARM service running on its own runtime. Dropping it (or calling
/// `stop`) tears the runtime down, closing every connection.
struct RunningService {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RunningService {
    fn start(addr: SocketAddr) -> Self {
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let db_path = std::env::temp_dir().join(format!(
                    "swarm-link-supervision-stun-{}.sqlite",
                    stun_server::security::new_id()
                ));
                let db = stun_server::db::connect(db_path.to_str().unwrap())
                    .await
                    .unwrap();
                let config = StunConfig {
                    database_path: db_path.display().to_string(),
                    http_bind: addr,
                    // Advertised like a real service does; the server only
                    // resolves the address, it does not contact the reflector.
                    reflector_ports: vec![443, 3478],
                    public_url: "http://test.invalid".into(),
                    session_ttl_secs: 3600,
                    join_code_ttl_secs: 900,
                    activation_ttl_secs: 600,
                    managed_swarm_lease_secs: 2_592_000,
                    managed_swarm_max_clients: 20,
                    smtp: None,
                };
                let state = Arc::new(AppState {
                    db,
                    hub: Hub::new(),
                    config,
                    blocker: BruteForceBlocker::new(),
                    activation_allocations: stun_server::security::AllocationLimiter::new(
                        20,
                        Duration::from_secs(3600),
                    ),
                    managed_swarm_allocations: stun_server::security::AllocationLimiter::new(
                        5,
                        Duration::from_secs(3600),
                    ),
                    mailer: Mailer::from_config(None),
                });
                let router = build_router(state, None);
                let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
                ready_tx.send(()).unwrap();
                let serve = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                );
                tokio::select! {
                    _ = serve => {}
                    _ = stop_rx => {}
                }
            });
            // Dropping the runtime here aborts every remaining task, which is
            // what closes the live WebSocket sessions.
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        Self {
            stop: Some(stop_tx),
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

impl Drop for RunningService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn test_config(name: &str, managed_url: Option<String>) -> ServerConfig {
    let base = std::env::temp_dir().join(format!(
        "swarm-link-supervision-{name}-{}",
        stun_server::security::new_id()
    ));
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    std::fs::create_dir_all(base.join("data")).unwrap();
    ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".to_string(),
            path: media_root,
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir: base.join("data"),
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: managed_url,
    }
}

async fn wait_for(
    core: &ServerCore,
    what: &str,
    predicate: impl Fn(&SwarmLinkStatus) -> bool,
) -> SwarmLinkStatus {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let status = core.swarm_link_status();
        if predicate(&status) {
            return status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; last status: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn health(core: &ServerCore) -> (serde_json::Value, String) {
    let body = reqwest::get(format!("http://{}/health", core.http_media_addr))
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    (serde_json::from_str(&body).unwrap(), body)
}

/// The incident itself: the service is down when the server starts, and comes
/// up later. The server must connect without being restarted.
#[tokio::test]
async fn a_service_that_is_down_at_startup_is_connected_to_once_it_appears() {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    fast_timing();
    let addr = reserve_addr();
    let url = format!("http://{addr}");

    let core = ServerCore::start(test_config("late-service", Some(url.clone())))
        .await
        .unwrap();

    // Down: reported, not silent.
    let down = wait_for(&core, "the outage to be reported", |s| {
        s.state == SwarmLinkState::Unreachable && s.attempts >= 2
    })
    .await;
    assert_eq!(down.base_url.as_deref(), Some(url.as_str()));
    assert!(down.last_error.is_some(), "an outage must say why: {down:?}");
    assert!(down.failing_since.is_some());
    assert!(!down.signaling);
    // Nothing is paired through SWARM, so this outage affects no one and must
    // not be presented as a problem.
    assert!(down.dependents.is_empty());
    assert!(!down.needs_attention, "an unused link must not ask for attention: {down:?}");

    // The health endpoint tells a monitor the same thing, without leaking the
    // service address or error text to an unauthenticated caller.
    let (json, body) = health(&core).await;
    assert_eq!(json["ok"], true);
    assert_eq!(json["swarm_link"]["state"], "unreachable");
    assert_eq!(json["swarm_link"]["signaling"], false);
    assert_eq!(json["swarm_link"]["needs_attention"], false);
    assert!(!body.contains(&url), "health leaked the service address: {body}");
    assert!(!body.contains("base_url") && !body.contains("last_error"), "{body}");

    // The service appears. No restart, no user action.
    let _service = RunningService::start(addr);
    let up = wait_for(&core, "the link to come up on its own", |s| {
        s.state == SwarmLinkState::Connected
    })
    .await;
    assert!(up.signaling);
    assert_eq!(up.attempts, 0);
    assert_eq!(up.last_error, None);
    assert_eq!(up.failing_since, None);
    assert!(up.connected_since.is_some());
    assert!(core.stun_link().await.is_some());

    let (json, _) = health(&core).await;
    assert_eq!(json["swarm_link"]["state"], "connected");
    assert_eq!(json["swarm_link"]["signaling"], true);
}

/// The service dies while the server is connected (live sessions closed, as
/// in an outage or the service host restarting), then comes back. Both the
/// loss and the recovery must be noticed.
#[tokio::test]
async fn a_lost_signaling_session_is_noticed_and_reestablished() {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    fast_timing();
    let addr = reserve_addr();
    let mut service = RunningService::start(addr);

    let core = ServerCore::start(test_config("dropped", Some(format!("http://{addr}"))))
        .await
        .unwrap();
    wait_for(&core, "the initial connection", |s| {
        s.state == SwarmLinkState::Connected
    })
    .await;

    service.stop();
    let lost = wait_for(&core, "the lost session to be reported", |s| {
        s.state == SwarmLinkState::Unreachable
    })
    .await;
    assert!(!lost.signaling);
    assert!(lost.failing_since.is_some());

    let _service = RunningService::start(addr);
    let back = wait_for(&core, "the link to be re-established", |s| {
        s.state == SwarmLinkState::Connected
    })
    .await;
    assert!(back.signaling);
    assert_eq!(back.attempts, 0);
}

/// The state this machine was actually in: no address configured anywhere, a
/// saved managed identity whose address no longer answers. Reporting it and
/// being able to forget it are the way out.
#[tokio::test]
async fn a_stale_saved_address_is_reported_and_can_be_forgotten() {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    fast_timing();
    let dead = reserve_addr();
    let dead_url = format!("http://{dead}");

    let config = test_config("stale", None);
    seed_managed_identity(&config.data_dir, &dead_url).await;
    let core = ServerCore::start(config).await.unwrap();

    let stuck = wait_for(&core, "the stale address to be reported", |s| {
        s.state == SwarmLinkState::Unreachable
    })
    .await;
    assert_eq!(stuck.base_url.as_deref(), Some(dead_url.as_str()));
    // The reported incident: a dead saved address and no TV using SWARM. The
    // user must not be warned about something that affects nothing.
    assert!(stuck.dependents.is_empty());
    assert!(!stuck.needs_attention, "a stale address nobody uses must stay quiet: {stuck:?}");

    core.forget_swarm_link().await.unwrap();
    let cleared = core.swarm_link_status();
    assert_eq!(cleared.state, SwarmLinkState::NotLinked);
    assert_eq!(cleared.base_url, None);

    // Several supervisor passes later it must stay forgotten, not quietly
    // redial the address it was told to drop.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(core.swarm_link_status().state, SwarmLinkState::NotLinked);
    assert!(core.stun_link().await.is_none());
}

/// A LAN-only install has nothing to link to. That is a healthy state, and
/// must not be reported or retried as a failure.
#[tokio::test]
async fn a_server_with_no_swarm_service_is_simply_not_linked() {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    fast_timing();
    let core = ServerCore::start(test_config("lan-only", None)).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    let status = core.swarm_link_status();
    assert_eq!(status.state, SwarmLinkState::NotLinked);
    assert_eq!(status.attempts, 0);
    assert_eq!(status.last_error, None);

    let (json, _) = health(&core).await;
    assert_eq!(json["ok"], true);
    assert_eq!(json["swarm_link"]["state"], "not_linked");
}

/// The other side of the rule: when a TV *is* paired through SWARM, an outage
/// affects it, so the user is told, and told who.
#[tokio::test]
async fn an_outage_is_flagged_when_a_tv_depends_on_the_link() {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    fast_timing();
    let addr = reserve_addr();
    let url = format!("http://{addr}");
    let mut service = RunningService::start(addr);

    let core = ServerCore::start(test_config("dependents", Some(url.clone())))
        .await
        .unwrap();
    wait_for(&core, "the initial connection", |s| {
        s.state == SwarmLinkState::Connected
    })
    .await;

    // A TV joins through SWARM activation, the way a real one does.
    let tv_api = swarm_stun_client::StunClient::new(url);
    let activation = tv_api
        .create_activation(tv("Family Room TV", "44"), None)
        .await
        .unwrap();
    core.lookup_activation(&activation.code).await.unwrap();
    core.approve_activation(&activation.activation_id).await.unwrap();
    let known = wait_for(&core, "the TV to be recorded as a dependent", |s| {
        s.dependents == ["Family Room TV"]
    })
    .await;
    assert!(!known.needs_attention, "a healthy link never asks for attention");

    // The service goes away: now someone is affected, and it says who.
    service.stop();
    let hurt = wait_for(&core, "the outage to need attention", |s| {
        s.state == SwarmLinkState::Unreachable && s.needs_attention
    })
    .await;
    assert_eq!(hurt.dependents, ["Family Room TV"]);
    let (json, body) = health(&core).await;
    assert_eq!(json["swarm_link"]["needs_attention"], true);
    assert!(!body.contains("Family Room TV"), "health leaked a device name: {body}");

    // Back again (a fresh service that has never heard of the TV): recovered,
    // and no longer asking for attention.
    let _service = RunningService::start(addr);
    let back = wait_for(&core, "recovery", |s| s.state == SwarmLinkState::Connected).await;
    assert!(!back.needs_attention);
}

/// A LAN IP or loopback is a snapshot of one machine on one network. Using it
/// for a run is fine; remembering it as the service's home is how an installed
/// app ended up dialing a developer's old address forever.
#[tokio::test]
async fn a_snapshot_address_is_used_for_the_run_but_not_remembered() {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    fast_timing();
    let addr = reserve_addr();
    let _service = RunningService::start(addr);

    let config = test_config("snapshot", Some(format!("http://{addr}")));
    let dev_run = ServerCore::start(config.clone()).await.unwrap();
    wait_for(&dev_run, "the dev run to connect", |s| {
        s.state == SwarmLinkState::Connected
    })
    .await;
    let dev_swarm = dev_run.stun_link().await.unwrap().swarms[0].id.clone();

    // The same data directory, started the way an installed app is: no
    // environment address. It must not inherit the dev run's.
    let mut installed = config.clone();
    installed.managed_rendezvous_url = None;
    let installed_run = ServerCore::start(installed).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let status = installed_run.swarm_link_status();
    assert_eq!(status.state, SwarmLinkState::NotLinked, "{status:?}");
    assert_eq!(status.attempts, 0);
    assert!(installed_run.stun_link().await.is_none());

    // The identity that matters (which swarm this server owns) *was* kept, so
    // the next dev run renews the same swarm instead of creating another.
    let next_dev_run = ServerCore::start(config).await.unwrap();
    wait_for(&next_dev_run, "the next dev run to connect", |s| {
        s.state == SwarmLinkState::Connected
    })
    .await;
    assert_eq!(next_dev_run.stun_link().await.unwrap().swarms[0].id, dev_swarm);
}

/// An unused link that has been down for a long time stops chattering, yet
/// still comes back by itself; and "Try again now" skips the wait.
#[tokio::test]
async fn an_unused_link_goes_dormant_and_try_again_skips_the_wait() {
    let _guard = INTEGRATION_TEST_LOCK.lock().await;
    fast_timing();
    std::env::set_var("SWARM_LINK_DORMANT_AFTER_MS", "1000");
    std::env::set_var("SWARM_LINK_DORMANT_RETRY_MS", "30000");
    let addr = reserve_addr();
    let core = ServerCore::start(test_config("dormant", Some(format!("http://{addr}"))))
        .await
        .unwrap();
    wait_for(&core, "the outage", |s| s.attempts >= 2).await;

    // Past the dormancy threshold the retry interval jumps to 30s, so the
    // attempt counter stops moving.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let before = core.swarm_link_status().attempts;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let after = core.swarm_link_status().attempts;
    assert!(
        after <= before + 1,
        "a dormant link kept retrying quickly: {before} -> {after}"
    );
    assert!(!core.swarm_link_status().needs_attention);

    // The service returns. Left alone the next attempt is ~30s away; the
    // button must not wait for it.
    let _service = RunningService::start(addr);
    let started = tokio::time::Instant::now();
    core.retry_swarm_link_now();
    wait_for(&core, "\"Try again now\" to connect", |s| {
        s.state == SwarmLinkState::Connected
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "retry took {:?}; it waited out the dormant interval",
        started.elapsed()
    );
}

/// Writes the rows a previous run would have left behind: a managed identity
/// pointing at `base_url`, and the owner claim that goes with it. Uses the
/// on-disk shapes directly because the state module is private, and running a
/// first core to create them would leave that core's supervisor alive and
/// re-creating the state this test deletes.
async fn seed_managed_identity(data_dir: &std::path::Path, base_url: &str) {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
    use std::str::FromStr;

    let options = SqliteConnectOptions::from_str(&format!(
        "sqlite://{}",
        data_dir.join("server-state.sqlite").to_str().unwrap()
    ))
    .unwrap()
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS managed_swarm_identity (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            base_url TEXT NOT NULL,
            swarm_id TEXT NOT NULL UNIQUE,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO managed_swarm_identity (id, base_url, swarm_id, created_at, updated_at) \
         VALUES (1, ?, ?, 0, 0)",
    )
    .bind(base_url)
    .bind("ab".repeat(32))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    swarm_stun_client::TokenStore::file_only(data_dir.join("managed-swarm-claim"))
        .save(&"cd".repeat(32))
        .unwrap();
}
