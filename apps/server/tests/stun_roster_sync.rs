//! Proves the STUN roster → `AllowedPeers` wiring end to end: two real
//! `ServerCore`s register against a real (in-process) STUN server with join
//! codes, each syncs the other into its allowed-peer set, an actual QUIC
//! connection succeeds once (and only once) that sync has happened, and
//! revoking a device on the STUN server locks it back out after a resync.
//!
//! This is the "servers only stream to clients in the same swarm" and
//! "multiple servers registered per client" requirement made concrete.

use std::net::SocketAddr;
use std::sync::Arc;
use stun_server::config::Config as StunConfig;
use stun_server::email::Mailer;
use stun_server::hub::Hub;
use stun_server::routes::build_router;
use stun_server::security::BruteForceBlocker;
use stun_server::state::AppState;
use swarm_core::peer::PeerRequest;
use swarm_core::rest::{ActivationStatus, DeviceRegistration, DeviceType};
use swarm_media::roots::MediaRoot;
use swarm_p2p::endpoint::{connect, send_request};
use swarm_p2p::identity::DeviceIdentity;
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};
use swarm_stun_client::StunClient;

// These are full-process integration tests: every case starts mDNS, QUIC,
// pairing, HTTP, and SQLite fixtures that continue background work for the
// duration of the test binary. Running the cases concurrently makes those OS
// fixtures compete and has produced unrelated AddrInUse/network/SQLite-open
// failures. Serialize this file while leaving the rest of the workspace's
// tests parallel.
static INTEGRATION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn spawn_stun_server() -> String {
    let db_path = std::env::temp_dir().join(format!(
        "swarm-roster-sync-stun-{}.sqlite",
        stun_server::security::new_id()
    ));
    let db = stun_server::db::connect(db_path.to_str().unwrap())
        .await
        .unwrap();
    let config = StunConfig {
        database_path: db_path.display().to_string(),
        http_bind: "127.0.0.1:0".parse().unwrap(),
        reflector_ports: vec![],
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
            std::time::Duration::from_secs(3600),
        ),
        managed_swarm_allocations: stun_server::security::AllocationLimiter::new(
            5,
            std::time::Duration::from_secs(3600),
        ),
        mailer: Mailer::from_config(None),
    });
    let router = build_router(state, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

/// Minimal cookie-session client for the STUN web API — just enough to
/// register an account, create a swarm, mint join codes, and revoke a
/// device, mirroring `apps/stun-server/tests/e2e.rs`'s `Browser` helper.
struct Browser {
    client: reqwest::Client,
    base: String,
    session: String,
    csrf: String,
}

impl Browser {
    async fn login_fresh_account(base: &str, email: &str) -> Self {
        let client = reqwest::Client::new();
        let password = "correct horse battery";
        client
            .post(format!("{base}/api/v1/auth/register"))
            .json(&serde_json::json!({"email": email, "password": password}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let response = client
            .post(format!("{base}/api/v1/auth/login"))
            .json(&serde_json::json!({"email": email, "password": password}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let mut session = String::new();
        let mut csrf = String::new();
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            let raw = value.to_str().unwrap();
            let (pair, _) = raw.split_once(';').unwrap_or((raw, ""));
            let (name, val) = pair.split_once('=').unwrap();
            match name {
                "swarm_session" => session = val.to_string(),
                "swarm_csrf" => csrf = val.to_string(),
                _ => {}
            }
        }
        Self {
            client,
            base: base.to_string(),
            session,
            csrf,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .header(
                "cookie",
                format!("swarm_session={}; swarm_csrf={}", self.session, self.csrf),
            )
            .header("x-swarm-csrf", &self.csrf)
    }

    async fn create_swarm(&self, name: &str) -> String {
        let body: serde_json::Value = self
            .request(reqwest::Method::POST, "/api/v1/swarms")
            .json(&serde_json::json!({"name": name}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        body["id"].as_str().unwrap().to_string()
    }

    async fn create_code(&self, swarm_id: &str) -> String {
        let body: serde_json::Value = self
            .request(
                reqwest::Method::POST,
                &format!("/api/v1/swarms/{swarm_id}/codes"),
            )
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        body["code"].as_str().unwrap().to_string()
    }

    async fn revoke_device(&self, device_id: &str) {
        self.request(
            reqwest::Method::DELETE,
            &format!("/api/v1/devices/{device_id}"),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    }
}

async fn spawn_media_server(tag: &str) -> Arc<ServerCore> {
    let base = std::env::temp_dir().join(format!("swarm-roster-sync-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let config = ServerConfig {
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
        // Real OS keyring behavior isn't something to assert on in an
        // automated test — see swarm_stun_client::TokenStore::file_only.
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    };
    let core = ServerCore::start(config).await.unwrap();
    core
}

/// TLS 1.3 client-cert rejection doesn't always surface as a `connect()`
/// error: the client can finish its side of the handshake before the server
/// reacts to a bad/absent client cert, so the rejection often only shows up
/// on the *first use* of the connection. Treat either a connect-time error
/// or a first-request error as "not authorized" — same pattern as
/// `apps/server/tests/lan_direct_play.rs`'s `unpinned_client_is_refused_at_tls`.
async fn can_use_connection(
    addr: SocketAddr,
    dialer: &DeviceIdentity,
    target_fingerprint: &str,
) -> bool {
    let connection = match connect(addr, dialer, target_fingerprint).await {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    let request = PeerRequest {
        path: "/catalog/thumbprint".into(),
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    };
    match send_request(&connection, &request).await {
        Ok((header, _)) => header.status == 200,
        Err(_) => false,
    }
}

#[tokio::test]
async fn roster_sync_grants_and_revokes_quic_access() {
    let _test_guard = INTEGRATION_TEST_LOCK.lock().await;
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "roster-sync@example.com").await;
    let swarm_id = browser.create_swarm("Home").await;

    let server_one = spawn_media_server("one").await;
    let server_two = spawn_media_server("two").await;

    // server_one joins alone: nobody else in the swarm yet.
    let code_one = browser.create_code(&swarm_id).await;
    server_one
        .register_with_stun(&stun_base, &code_one, "Server One")
        .await
        .unwrap();
    assert!(server_one.allowed.is_empty());

    // Before server_two ever registers, its identity is a stranger to
    // server_one — it must not be able to use a connection to it.
    assert!(
        !can_use_connection(
            server_one.listen_addr,
            &server_two.identity,
            &server_one.identity.fingerprint
        )
        .await,
        "an unregistered identity must not be able to connect"
    );

    // server_two joins the same swarm. Its own registration sync already
    // sees server_one (which was already a member when it joined).
    let code_two = browser.create_code(&swarm_id).await;
    server_two
        .register_with_stun(&stun_base, &code_two, "Server Two")
        .await
        .unwrap();
    assert!(server_two
        .allowed
        .contains(&server_one.identity.fingerprint));

    // server_one hasn't resynced yet — still doesn't know about server_two,
    // and it still can't use a connection using server_two's real identity.
    assert!(!server_one
        .allowed
        .contains(&server_two.identity.fingerprint));
    assert!(
        !can_use_connection(
            server_one.listen_addr,
            &server_two.identity,
            &server_one.identity.fingerprint
        )
        .await,
        "roster hasn't synced yet; must still be refused"
    );

    // Resync (the GUI "Resync" button / periodic tick) picks up the new member.
    let count = server_one.resync().await.unwrap();
    assert_eq!(count, 1);
    assert!(server_one
        .allowed
        .contains(&server_two.identity.fingerprint));

    // Now the same identity can actually use a connection.
    assert!(
        can_use_connection(
            server_one.listen_addr,
            &server_two.identity,
            &server_one.identity.fingerprint
        )
        .await,
        "server_two must now be able to connect to server_one"
    );

    // Revoke server_two on the STUN server; after a resync, server_one must
    // lock it back out — revocation actually propagates, not just adds.
    let device_id = server_two.stun_link().await.unwrap().device_id;
    browser.revoke_device(&device_id).await;
    server_one.resync().await.unwrap();
    assert!(!server_one
        .allowed
        .contains(&server_two.identity.fingerprint));
    assert!(
        !can_use_connection(
            server_one.listen_addr,
            &server_two.identity,
            &server_one.identity.fingerprint
        )
        .await,
        "a revoked device must be refused after resync"
    );
}

#[tokio::test]
async fn restart_restores_the_stun_link_and_allowed_peers() {
    let _test_guard = INTEGRATION_TEST_LOCK.lock().await;
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "restart@example.com").await;
    let swarm_id = browser.create_swarm("Home").await;

    let peer = spawn_media_server("restart-peer").await;

    let base =
        std::env::temp_dir().join(format!("swarm-roster-sync-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let config = ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".to_string(),
            path: media_root.clone(),
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir: base.join("data"),
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    };
    let first_run = ServerCore::start(config.clone()).await.unwrap();
    let code = browser.create_code(&swarm_id).await;
    first_run
        .register_with_stun(&stun_base, &code, "Restartable")
        .await
        .unwrap();
    let fingerprint_before_restart = first_run.identity.fingerprint.clone();
    drop(first_run); // simulate process exit; nothing further touches this handle

    // A fresh peer joins the same swarm while our device is "offline".
    let peer_code = browser.create_code(&swarm_id).await;
    peer.register_with_stun(&stun_base, &peer_code, "Peer")
        .await
        .unwrap();

    // "Restart": start a new ServerCore over the same data_dir. It must
    // restore the identity (same fingerprint), the STUN link, and — after
    // its restore-time sync — the peer that joined while it was down.
    let second_run = ServerCore::start(config).await.unwrap();
    assert_eq!(second_run.identity.fingerprint, fingerprint_before_restart);
    assert!(second_run.stun_link().await.is_some());
    // restore_stun_link's initial sync races the test; resync deterministically.
    second_run.resync().await.unwrap();
    assert!(second_run.allowed.contains(&peer.identity.fingerprint));
}

/// Regression for a real desktop upgrade: an installation already linked
/// through the old account/join-code flow had a valid device token, so startup
/// skipped managed provisioning. Entering a TV activation code then failed
/// with 403 because that legacy device did not own a managed swarm.
#[tokio::test]
async fn configured_managed_swarm_migrates_an_existing_manual_link_before_tv_approval() {
    let _test_guard = INTEGRATION_TEST_LOCK.lock().await;
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "managed-migration@example.com").await;
    let legacy_swarm_id = browser.create_swarm("Legacy Home").await;

    let base = std::env::temp_dir().join(format!(
        "swarm-managed-migration-{}",
        stun_server::security::new_id()
    ));
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let config = ServerConfig {
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
        managed_rendezvous_url: None,
    };

    let legacy_run = ServerCore::start(config.clone()).await.unwrap();
    let legacy_code = browser.create_code(&legacy_swarm_id).await;
    legacy_run
        .register_with_stun(&stun_base, &legacy_code, "Existing Server")
        .await
        .unwrap();
    assert_eq!(
        legacy_run.stun_link().await.unwrap().swarms[0].id,
        legacy_swarm_id
    );

    let mut migrated_config = config;
    migrated_config.managed_rendezvous_url = Some(stun_base.clone());
    let migrated_run = ServerCore::start(migrated_config).await.unwrap();
    let managed_link = migrated_run.stun_link().await.unwrap();
    assert_ne!(managed_link.swarms[0].id, legacy_swarm_id);

    let tv_api = StunClient::new(stun_base);
    let activation = tv_api
        .create_activation(
            DeviceRegistration {
                name: "Family Room TV".into(),
                device_type: DeviceType::Client,
                machine_id: "migration-tv".into(),
                cert_fingerprint: "44".repeat(32),
                platform: "android-tv".into(),
                app_version: "test".into(),
                metadata: Default::default(),
            },
            None,
        )
        .await
        .unwrap();
    let preview = migrated_run
        .lookup_activation(&activation.code)
        .await
        .unwrap();
    assert_eq!(preview.device_name, "Family Room TV");
    let approved = migrated_run
        .approve_activation(&activation.activation_id)
        .await
        .unwrap();
    assert_eq!(approved.status, ActivationStatus::Approved);
    assert_eq!(approved.swarm.unwrap().id, managed_link.swarms[0].id);
}

/// A managed service can keep the same database/ownership while its public
/// hostname or LAN address changes. The owner claim must be accepted at the
/// newly configured endpoint before either persisted base URL is migrated.
#[tokio::test]
async fn managed_swarm_adopts_a_new_endpoint_after_owner_claim_validation() {
    let _test_guard = INTEGRATION_TEST_LOCK.lock().await;
    let stun_base = spawn_stun_server().await;
    let alternate_base = stun_base.replacen("127.0.0.1", "localhost", 1);
    assert_ne!(stun_base, alternate_base);

    let base = std::env::temp_dir().join(format!(
        "swarm-managed-endpoint-migration-{}",
        stun_server::security::new_id()
    ));
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let mut config = ServerConfig {
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
        managed_rendezvous_url: Some(stun_base.clone()),
    };

    let first = ServerCore::start(config.clone()).await.unwrap();
    let original = first.stun_link().await.unwrap();
    assert_eq!(original.base_url, stun_base);

    config.managed_rendezvous_url = Some(alternate_base.clone());
    let migrated = ServerCore::start(config).await.unwrap();
    let migrated_link = migrated.stun_link().await.unwrap();
    assert_eq!(migrated_link.base_url, alternate_base);
    assert_eq!(migrated_link.swarms, original.swarms);
}

/// A server that belongs to two swarms leaves one and keeps the other:
/// `allowed` shrinks to drop only the peer reachable solely through the
/// left swarm, and the persisted link's `swarms` list shrinks to match.
#[tokio::test]
async fn leave_swarm_shrinks_allowed_peers_and_link() {
    let _test_guard = INTEGRATION_TEST_LOCK.lock().await;
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "leave-swarm@example.com").await;
    let home_id = browser.create_swarm("Home").await;
    let cabin_id = browser.create_swarm("Cabin").await;

    let server = spawn_media_server("leaver").await;
    let peer_home = spawn_media_server("home-peer").await;
    let peer_cabin = spawn_media_server("cabin-peer").await;

    let code_home = browser.create_code(&home_id).await;
    server
        .register_with_stun(&stun_base, &code_home, "Leaver")
        .await
        .unwrap();
    let code_cabin = browser.create_code(&cabin_id).await;
    server.join_additional_swarm(&code_cabin).await.unwrap();

    let peer_home_code = browser.create_code(&home_id).await;
    peer_home
        .register_with_stun(&stun_base, &peer_home_code, "Home Peer")
        .await
        .unwrap();
    let peer_cabin_code = browser.create_code(&cabin_id).await;
    peer_cabin
        .register_with_stun(&stun_base, &peer_cabin_code, "Cabin Peer")
        .await
        .unwrap();

    server.resync().await.unwrap();
    assert!(server.allowed.contains(&peer_home.identity.fingerprint));
    assert!(server.allowed.contains(&peer_cabin.identity.fingerprint));
    assert_eq!(server.stun_link().await.unwrap().swarms.len(), 2);

    server.leave_swarm(&cabin_id).await.unwrap();
    assert!(
        server.allowed.contains(&peer_home.identity.fingerprint),
        "Home membership must be untouched"
    );
    assert!(
        !server.allowed.contains(&peer_cabin.identity.fingerprint),
        "Cabin peer must drop out of allowed"
    );
    let link = server.stun_link().await.unwrap();
    assert_eq!(link.swarms.len(), 1);
    assert_eq!(link.swarms[0].id, home_id);
}

/// A server self-reports where it can be dialed (`peer_addr` metadata) —
/// otherwise a client's swarm roster tells it *that* a server exists but
/// never *where*, which is the whole point of the roster for a client
/// trying to connect. Covers both the immediate value submitted at
/// registration and the ongoing refresh on resync.
#[tokio::test]
async fn server_self_reports_a_dialable_peer_addr() {
    let _test_guard = INTEGRATION_TEST_LOCK.lock().await;
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "peer-addr@example.com").await;
    let swarm_id = browser.create_swarm("Home").await;

    let server = spawn_media_server("addr-report").await;
    let code = browser.create_code(&swarm_id).await;
    server
        .register_with_stun(&stun_base, &code, "Server")
        .await
        .unwrap();

    let roster: swarm_core::rest::SwarmDevicesResponse = browser
        .request(
            reqwest::Method::GET,
            &format!("/api/v1/swarms/{swarm_id}/devices"),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let device = roster
        .devices
        .iter()
        .find(|d| d.cert_fingerprint == server.identity.fingerprint)
        .unwrap();
    let reported: std::net::SocketAddr = device
        .metadata
        .get("peer_addr")
        .expect("peer_addr metadata missing")
        .parse()
        .unwrap();
    assert_eq!(reported.port(), server.listen_addr.port());

    // Ongoing refresh: resync must re-submit (not just the one-time value
    // from registration) — prove the mechanism actually runs on that path.
    server.resync().await.unwrap();
    let roster_after: swarm_core::rest::SwarmDevicesResponse = browser
        .request(
            reqwest::Method::GET,
            &format!("/api/v1/swarms/{swarm_id}/devices"),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let device_after = roster_after
        .devices
        .iter()
        .find(|d| d.cert_fingerprint == server.identity.fingerprint)
        .unwrap();
    assert_eq!(
        device_after.metadata.get("peer_addr"),
        Some(&reported.to_string())
    );
}
