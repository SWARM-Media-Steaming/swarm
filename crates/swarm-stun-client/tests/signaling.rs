//! Proves `SignalingClient` end to end against a real (in-process) STUN
//! server: hello/hello_ack, presence fan-out on a swarm-mate connecting and
//! disconnecting, signal relay between swarm-mates with `from` stamped
//! server-side, signal rejection across swarms, and hello rejection on a bad
//! token. Mirrors `apps/stun-server/tests/e2e.rs`'s coverage of the same
//! server-side behavior, but through this crate's actual client rather than
//! a raw `tokio-tungstenite` connection — the thing real callers will use.
//!
//! `signal_relays_between_swarm_mates_with_from_stamped` was observed to
//! fail intermittently under `cargo test --workspace` (never in isolated
//! repeats of just this file), panicking with a stray `Presence` in place
//! of the expected `Signal`. Root cause: a connecting device's own
//! presence broadcast to its mates happens *after* its `hello_ack` is sent
//! (see `apps/stun-server/src/routes/ws.rs`), so under scheduler load that
//! broadcast for `a`'s connect can be delayed past `b`'s connect — landing
//! a "presence of a" frame in `b_rx` interleaved with unrelated signal
//! traffic. That reordering is legitimate protocol behavior (presence
//! delivery isn't ordered against later signals to a different peer), so
//! the test tolerates it by skipping stray `Presence` frames while waiting
//! for the `Signal` it actually cares about.

use std::collections::BTreeMap;
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
use swarm_core::signal::{Candidate, CandidateKind, SignalMessage, SignalPayload};
use swarm_stun_client::{SignalingClient, SignalingError, StunClient};
use tokio::sync::mpsc;

async fn spawn_stun_server() -> String {
    let db_path = std::env::temp_dir().join(format!(
        "swarm-signaling-stun-{}.sqlite",
        stun_server::security::new_id()
    ));
    let db = stun_server::db::connect(db_path.to_str().unwrap())
        .await
        .unwrap();
    let config = StunConfig {
        database_path: db_path.display().to_string(),
        http_bind: "127.0.0.1:0".parse().unwrap(),
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
/// register an account, create a swarm, and mint join codes.
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

fn registration(name: &str, fingerprint_byte: &str) -> DeviceRegistration {
    DeviceRegistration {
        name: name.into(),
        device_type: DeviceType::Client,
        machine_id: format!("machine-{name}"),
        cert_fingerprint: fingerprint_byte.repeat(32),
        platform: "test".into(),
        app_version: "0.1.0".into(),
        metadata: BTreeMap::new(),
    }
}

/// Registers a fresh device into `swarm_id` via a freshly minted join code
/// and returns `(device_id, access_token)`.
async fn register_device(
    browser: &Browser,
    stun_base: &str,
    swarm_id: &str,
    name: &str,
    fp_byte: &str,
) -> (String, String) {
    let code = browser.create_code(swarm_id).await;
    let response = StunClient::new(stun_base)
        .register_device(&code, registration(name, fp_byte))
        .await
        .unwrap();
    (response.device_id, response.access_token)
}

async fn expect_signal(rx: &mut mpsc::UnboundedReceiver<SignalMessage>) -> SignalMessage {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .expect("channel closed unexpectedly")
}

/// Like `expect_signal`, but discards any interleaved `Presence` frames
/// first — a connecting peer's own presence broadcast can be delayed by
/// scheduler load and arrive after other, later traffic (see module docs).
async fn expect_signal_ignoring_presence(
    rx: &mut mpsc::UnboundedReceiver<SignalMessage>,
) -> SignalMessage {
    loop {
        match expect_signal(rx).await {
            SignalMessage::Presence { .. } => continue,
            other => return other,
        }
    }
}

#[tokio::test]
async fn hello_ack_reports_a_real_session() {
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "signaling-hello@example.com").await;
    let swarm_id = browser.create_swarm("Home").await;
    let (device_id, token) = register_device(&browser, &stun_base, &swarm_id, "dev", "aa").await;

    let (client, _rx) = SignalingClient::connect(&stun_base, &token, &device_id, None)
        .await
        .unwrap();
    assert!(!client.session_id.is_empty());
    assert!(client.observed_addr.contains("127.0.0.1"));
    assert_eq!(client.reflector_ports, vec![443, 3478]);
}

#[tokio::test]
async fn presence_fires_on_swarm_mate_connect_and_disconnect() {
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "signaling-presence@example.com").await;
    let swarm_id = browser.create_swarm("Home").await;
    let (a_id, a_token) = register_device(&browser, &stun_base, &swarm_id, "a", "aa").await;
    let (b_id, b_token) = register_device(&browser, &stun_base, &swarm_id, "b", "bb").await;

    let (_a, mut a_rx) = SignalingClient::connect(&stun_base, &a_token, &a_id, None)
        .await
        .unwrap();
    let (b, mut b_rx) = SignalingClient::connect(&stun_base, &b_token, &b_id, None)
        .await
        .unwrap();

    // A sees B come online (B connected after A).
    match expect_signal(&mut a_rx).await {
        SignalMessage::Presence {
            device_id, online, ..
        } => {
            assert_eq!(device_id, b_id);
            assert!(online);
        }
        other => panic!("expected Presence, got {other:?}"),
    }

    b.bye().unwrap();
    drop(b_rx.recv().await); // let the Bye frame land before dropping the connection
    drop(b);

    match expect_signal(&mut a_rx).await {
        SignalMessage::Presence {
            device_id, online, ..
        } => {
            assert_eq!(device_id, b_id);
            assert!(!online);
        }
        other => panic!("expected offline Presence, got {other:?}"),
    }
}

#[tokio::test]
async fn signal_relays_between_swarm_mates_with_from_stamped() {
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "signaling-relay@example.com").await;
    let swarm_id = browser.create_swarm("Home").await;
    let (a_id, a_token) = register_device(&browser, &stun_base, &swarm_id, "a", "aa").await;
    let (b_id, b_token) = register_device(&browser, &stun_base, &swarm_id, "b", "bb").await;

    let (a, mut a_rx) = SignalingClient::connect(&stun_base, &a_token, &a_id, None)
        .await
        .unwrap();
    let (_b, mut b_rx) = SignalingClient::connect(&stun_base, &b_token, &b_id, None)
        .await
        .unwrap();
    let _ = expect_signal(&mut a_rx).await; // A's presence-of-B notification, not under test here

    let offer = SignalPayload::Offer {
        punch_id: "p1".into(),
        candidates: vec![Candidate {
            kind: CandidateKind::Lan,
            ip: "192.168.1.10".into(),
            port: 40000,
        }],
        cert_fingerprint: "aa".repeat(32),
    };
    a.send_signal(&b_id, offer.clone()).unwrap();

    match expect_signal_ignoring_presence(&mut b_rx).await {
        SignalMessage::Signal { from, to, payload } => {
            assert_eq!(from.as_deref(), Some(a_id.as_str()));
            assert_eq!(to, b_id);
            assert_eq!(payload, offer);
        }
        other => panic!("expected Signal, got {other:?}"),
    }
}

#[tokio::test]
async fn signal_across_swarms_is_rejected() {
    let stun_base = spawn_stun_server().await;
    let browser =
        Browser::login_fresh_account(&stun_base, "signaling-cross-swarm@example.com").await;
    let swarm_a = browser.create_swarm("Home").await;
    let swarm_b = browser.create_swarm("Cabin").await;
    let (a_id, a_token) = register_device(&browser, &stun_base, &swarm_a, "a", "aa").await;
    let (b_id, b_token) = register_device(&browser, &stun_base, &swarm_b, "b", "bb").await;

    let (a, mut a_rx) = SignalingClient::connect(&stun_base, &a_token, &a_id, None)
        .await
        .unwrap();
    let (_b, _b_rx) = SignalingClient::connect(&stun_base, &b_token, &b_id, None)
        .await
        .unwrap();

    a.send_signal(
        &b_id,
        SignalPayload::Punched {
            punch_id: "p1".into(),
            ok: true,
        },
    )
    .unwrap();

    match expect_signal(&mut a_rx).await {
        SignalMessage::Error { code, .. } => assert_eq!(code, "not_swarm_mates"),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_bad_token_is_rejected_at_hello() {
    let stun_base = spawn_stun_server().await;
    let browser = Browser::login_fresh_account(&stun_base, "signaling-bad-token@example.com").await;
    let swarm_id = browser.create_swarm("Home").await;
    let (device_id, _token) = register_device(&browser, &stun_base, &swarm_id, "dev", "aa").await;

    let err = SignalingClient::connect(&stun_base, "not-the-real-token", &device_id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, SignalingError::Rejected { code, .. } if code == "unauthorized"));
}
