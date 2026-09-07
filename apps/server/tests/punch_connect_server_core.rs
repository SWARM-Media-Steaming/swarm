//! Proves `ServerCore` actually uses `punch_connect` on its own, not just
//! that the pieces compile together: a real `ServerCore`, registered
//! against a real STUN server exactly like `swarm-serverd` would be,
//! automatically answers an incoming `Offer` from a swarm-mate — no test
//! code calls `respond_to_punch_offer` directly here, that's
//! `punch_connect.rs`'s job. This is the thing only `establish_signaling` +
//! `spawn_punch_dispatch_loop` together can show: that registering with a
//! STUN server really does leave a running server ready to accept a
//! hole-punched connection with no further action.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use stun_server::config::Config as StunConfig;
use stun_server::email::Mailer;
use stun_server::hub::Hub;
use stun_server::routes::build_router;
use stun_server::security::BruteForceBlocker;
use stun_server::state::AppState;
use swarm_core::peer::{CatalogThumbprint, PeerRequest};
use swarm_core::rest::{DeviceRegistration, DeviceType};
use swarm_media::roots::MediaRoot;
use swarm_p2p::endpoint::{read_body, send_request};
use swarm_p2p::identity::ensure_identity;
use swarm_server::punch_connect::initiate_punch_connection;
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};
use swarm_stun_client::{SignalingClient, StunClient};

async fn spawn_stun_server_with_reflector() -> (String, SocketAddr) {
    let probe = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let reflector_port = probe.local_addr().unwrap().port();
    drop(probe);
    tokio::spawn(stun_server::reflector::run(reflector_port));
    tokio::time::sleep(Duration::from_millis(50)).await;

    let db_path = std::env::temp_dir().join(format!(
        "swarm-punch-core-stun-{}.sqlite",
        stun_server::security::new_id()
    ));
    let db = stun_server::db::connect(db_path.to_str().unwrap())
        .await
        .unwrap();
    let config = StunConfig {
        database_path: db_path.display().to_string(),
        http_bind: "127.0.0.1:0".parse().unwrap(),
        reflector_ports: vec![reflector_port],
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
    (
        format!("http://{addr}"),
        format!("127.0.0.1:{reflector_port}").parse().unwrap(),
    )
}

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
}

async fn spawn_media_server(tag: &str) -> Arc<ServerCore> {
    let base = std::env::temp_dir().join(format!("swarm-punch-core-{tag}-{}", std::process::id()));
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
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    };
    let core = ServerCore::start(config).await.unwrap();
    core
}

#[tokio::test]
async fn registering_with_stun_leaves_the_server_ready_to_accept_a_punched_connection() {
    let (stun_base, reflector_addr) = spawn_stun_server_with_reflector().await;
    let browser = Browser::login_fresh_account(&stun_base, "punch-core@example.com").await;
    let swarm_id = browser.create_swarm("Home").await;

    let server = spawn_media_server("server").await;
    let server_code = browser.create_code(&swarm_id).await;
    server
        .register_with_stun(&stun_base, &server_code, "Server")
        .await
        .unwrap();
    let server_device_id = server.stun_link().await.unwrap().device_id;

    // A plain client identity — not a ServerCore, just what the Fire TV
    // client's future Kotlin port will eventually be: an identity, a REST
    // registration, and a signaling session.
    let base = std::env::temp_dir().join(format!("swarm-punch-core-client-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let client_identity = ensure_identity(&base.join("client-id")).unwrap();
    let client_code = browser.create_code(&swarm_id).await;
    let client_registration = DeviceRegistration {
        name: "Client".into(),
        device_type: DeviceType::Client,
        machine_id: "machine-client".into(),
        cert_fingerprint: client_identity.fingerprint.clone(),
        platform: "test".into(),
        app_version: "0.1.0".into(),
        metadata: Default::default(),
    };
    let client_reg = StunClient::new(&stun_base)
        .register_device(&client_code, client_registration)
        .await
        .unwrap();

    // The server's roster sync is on a 30s timer; force it so the freshly
    // joined client is in AllowedPeers before the punch attempt below.
    server.resync().await.unwrap();

    let (client_signaling, mut client_rx) = SignalingClient::connect(
        &stun_base,
        &client_reg.access_token,
        &client_reg.device_id,
        None,
    )
    .await
    .unwrap();

    // No call to respond_to_punch_offer anywhere in this test — the server
    // is expected to answer entirely on its own, exactly as it would for a
    // real off-LAN Fire TV client.
    let connection = initiate_punch_connection(
        &client_signaling,
        &mut client_rx,
        reflector_addr,
        &server_device_id,
        &client_identity,
        &server.identity.fingerprint,
    )
    .await
    .unwrap();

    let request = PeerRequest {
        path: "/catalog/thumbprint".into(),
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    };
    let (header, mut recv) = send_request(&connection, &request).await.unwrap();
    assert_eq!(header.status, 200);
    let body = read_body(&header, &mut recv).await.unwrap();
    let thumbprint: CatalogThumbprint = serde_json::from_slice(&body).unwrap();
    assert_eq!(thumbprint.entry_count, 0); // empty media root — the point is that MediaService answered at all

    connection.close(0u32.into(), b"done");
}
