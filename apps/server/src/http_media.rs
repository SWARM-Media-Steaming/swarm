//! Plain-HTTP(S) pairing + media-playback surface for clients that can't
//! speak the QUIC peer transport — Roku-class devices, see the pinned
//! implementation-plan comment on GitHub issue #54. Runs unconditionally
//! from `ServerCore::start`, the same as the QUIC listener and LAN pairing
//! (`lan.rs`), on its own dedicated port (`ServerConfig::http_media_bind`,
//! not `bind`'s — a second TCP listener can't safely share `bind`'s port
//! number the way `lan.rs`'s raw-TCP listener already does, since `lan.rs`
//! claims it first inside `ServerCore::start`).
//!
//! Two credential models coexist deliberately, not by accident:
//! - `/pair/*` is unauthenticated by definition — a not-yet-paired device
//!   has no credential yet — so it is gated instead by `is_lan_ip` (only a
//!   private/link-local/loopback caller may even attempt pairing) and rate
//!   limited (`AllocationLimiter`), because real HTTP — unlike `lan.rs`'s
//!   raw NDJSON-over-TCP protocol, which no browser can speak — is directly
//!   reachable by any co-resident browser`'s `fetch()`, not just a real
//!   device client. `reject_cross_site` closes that gap: a real device
//!   client never sends the `Sec-Fetch-*` headers browsers attach to every
//!   fetch/XHR, so their mere presence is treated as "this is a webpage,
//!   not a device" and rejected outright.
//! - Everything else requires `Authorization: Bearer <token>`, checked
//!   against `state_db`'s `http_media_device` table (hash only — see
//!   `state_db.rs`), modeled on `apps/stun-server/src/authn.rs::require_device`'s
//!   per-device hash-lookup shape, deliberately not `mcp.rs::has_valid_bearer`'s
//!   single static-shared-secret comparison — a different, weaker model.
//!
//! `start()` optionally runs a **second** listener on its own port serving
//! the exact same `Router` over TLS (`HttpMediaTlsConfig`), terminated with
//! a leaf certificate freshly issued from this server's own long-lived HTTP
//! CA (`swarm_p2p::http_tls` — deliberately not the QUIC peer identity cert;
//! see that module's doc comment for why). This is what lets a paired
//! HTTP-only client reach the server through the relay (issue #54) without
//! the relay ever seeing plaintext: the relay only ever forwards already-
//! TLS-wrapped bytes, ciphertext in, ciphertext out. The plain listener is
//! unaffected either way and keeps serving ordinary same-LAN clients.
//! `/pair/poll`'s `Approved` response carries the CA's PEM
//! (`http_ca_pem`) so a device that just paired learns its trust anchor at
//! the same moment it learns its bearer token — it has no other way to
//! obtain it, since the CA is generated fresh per server install.

use crate::link::SwarmLinkStatus;
use crate::state_db::StateDb;
use axum::extract::{ConnectInfo, Extension, Json, OriginalUri, Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower_http::trace::TraceLayer;
use swarm_core::peer::{
    ByteRange, ClientErrorReport, LikeToggle, PeerRequest, PlaybackPreferences,
};
use swarm_media::serve::{is_lan_ip, stream_body, MediaService};
use tokio::net::TcpListener;

// ---------- local auth primitives ----------
//
// Deliberately duplicated rather than shared into `crates/swarm-core` or
// reused from `apps/stun-server`: swarm-core's own doc comment scopes it to
// wire-protocol types + identity primitives and asks callers to keep it
// dependency-light, and there is no cross-service wire-compatibility need
// here (unlike `PeerRequest`, which must byte-match across Kotlin/Rust,
// this crate's tokens are only ever validated against this crate's own
// table). This mirrors `is_lan_ip` (`swarm_media::serve`) vs. `lan.rs`'s
// own `is_lan_address` — already two independently-maintained copies of the
// same shape of helper in this codebase.

/// 256-bit opaque token as lowercase hex. Handed to the device once; only
/// [`token_hash`] is ever persisted.
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Fixed-window per-IP allocation limiter, mirroring
/// `apps/stun-server/src/security.rs::AllocationLimiter` (used there for
/// the identical purpose: bounding TV-activation creation). Not shared for
/// the same reason as the token helpers above.
struct AllocationLimiter {
    state: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    max: usize,
    window: Duration,
}

impl AllocationLimiter {
    fn new(max: usize, window: Duration) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            max,
            window,
        }
    }

    fn allow(&self, ip: IpAddr) -> bool {
        if ip.is_loopback() {
            return true;
        }
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        let entries = state.entry(ip).or_default();
        entries.retain(|at| now.duration_since(*at) < self.window);
        if entries.len() >= self.max {
            return false;
        }
        entries.push(now);
        true
    }
}

// ---------- pairing state machine ----------
//
// A near-twin of `lan.rs`'s `PairingState`, kept as a *separate*
// implementation rather than merged into it: `lan.rs`'s Android/cert-based
// flow already works and is already tested, and this task deliberately
// avoids touching it (see the port-collision reasoning in the
// implementation-plan comment on issue #54 for why this surface got its
// own dedicated port instead of trying to share `lan.rs`'s). The real
// difference from `lan.rs`: an HTTP-only device has no cert yet, so `begin`
// takes just a name, and approval mints a bearer token instead of
// persisting a client-presented fingerprint.

const ACTIVATION_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_PENDING_ACTIVATIONS: usize = 32;

#[derive(Debug, Clone)]
struct PendingActivation {
    activation_id: String,
    poll_token: String,
    code: String,
    name: String,
    requester_ip: IpAddr,
    expires_at: Instant,
    approved_token: Option<String>,
}

#[derive(Default)]
struct PairingState {
    activations: Vec<PendingActivation>,
}

struct ActivationStarted {
    activation_id: String,
    poll_token: String,
    code: String,
    expires_in_seconds: u64,
}

enum PollStatus {
    Pending,
    Approved(String),
    Expired,
}

fn started_from(activation: &PendingActivation) -> ActivationStarted {
    ActivationStarted {
        activation_id: activation.activation_id.clone(),
        poll_token: activation.poll_token.clone(),
        code: activation.code.clone(),
        expires_in_seconds: activation
            .expires_at
            .saturating_duration_since(Instant::now())
            .as_secs()
            .max(1),
    }
}

impl PairingState {
    fn purge_expired(&mut self) {
        let now = Instant::now();
        self.activations
            .retain(|activation| activation.expires_at > now);
    }

    fn begin(
        &mut self,
        name: String,
        requester_ip: IpAddr,
    ) -> Result<ActivationStarted, &'static str> {
        self.purge_expired();
        if let Some(existing) = self.activations.iter().find(|activation| {
            activation.name == name && activation.requester_ip == requester_ip
        }) {
            return Ok(started_from(existing));
        }
        if self.activations.len() >= MAX_PENDING_ACTIVATIONS {
            return Err("too_many_pending_activations");
        }
        let code = loop {
            let candidate = format!("{:08}", rand::random::<u32>() % 100_000_000);
            if self
                .activations
                .iter()
                .all(|activation| activation.code != candidate)
            {
                break candidate;
            }
        };
        let activation = PendingActivation {
            activation_id: hex::encode(rand::random::<[u8; 16]>()),
            poll_token: hex::encode(rand::random::<[u8; 24]>()),
            code,
            name,
            requester_ip,
            expires_at: Instant::now() + ACTIVATION_TTL,
            approved_token: None,
        };
        let started = started_from(&activation);
        self.activations.push(activation);
        Ok(started)
    }

    fn poll(&mut self, activation_id: &str, poll_token: &str, requester_ip: IpAddr) -> PollStatus {
        self.purge_expired();
        match self.activations.iter().find(|activation| {
            activation.activation_id == activation_id
                && activation.poll_token == poll_token
                && activation.requester_ip == requester_ip
        }) {
            Some(activation) => match &activation.approved_token {
                Some(token) => PollStatus::Approved(token.clone()),
                None => PollStatus::Pending,
            },
            None => PollStatus::Expired,
        }
    }

    /// Owner-only: mints a fresh bearer token for the pending activation
    /// matching `code`. Never called from the network — see
    /// [`HttpMediaService::approve`].
    fn approve(&mut self, code: &str) -> Result<(String, String), &'static str> {
        self.purge_expired();
        let activation = self
            .activations
            .iter_mut()
            .find(|activation| activation.code == code)
            .ok_or("invalid_code")?;
        let token = generate_token();
        activation.approved_token = Some(token.clone());
        Ok((activation.name.clone(), token))
    }
}

// ---------- public handle ----------

/// Owner-facing handle `ServerCore` holds, mirroring `lan::LanService`'s
/// role for the cert-based flow.
pub struct HttpMediaService {
    pairing: Arc<Mutex<PairingState>>,
    state_db: Arc<StateDb>,
    /// The real bound address — `ServerConfig::http_media_bind` may be a
    /// `:0` ephemeral port (every test fixture uses one), so this is the
    /// only way to learn what was actually chosen. Mirrors
    /// `ServerCore::listen_addr`'s identical role for the QUIC listener.
    pub local_addr: SocketAddr,
    /// The real bound address of the TLS listener, `None` when
    /// `HttpMediaTlsConfig` wasn't supplied to `start()`. Same `:0`-in-tests
    /// reasoning as `local_addr`.
    pub tls_local_addr: Option<SocketAddr>,
}

/// Configuration for the optional second, TLS-terminated listener. See the
/// module doc comment for why this exists and what it's for.
pub struct HttpMediaTlsConfig {
    pub bind: SocketAddr,
    pub ca: Arc<swarm_p2p::http_tls::HttpCa>,
    /// SANs the freshly issued leaf must cover — every address/hostname a
    /// client might actually connect through (LAN IP, loopback, and,
    /// eventually, a granted relay hostname).
    pub sans: Vec<String>,
}

impl HttpMediaService {
    /// Approves the short-lived code displayed by an HTTP-only device.
    /// Invoked only from a trusted local Tauri command
    /// (`gui.rs::approve_http_media_pairing`), never over the network — the
    /// same trust boundary as `lan::LanService::approve_pairing_code`.
    /// Persists the newly-minted token's hash and returns the raw token
    /// once (the caller is expected to discard it after this call; the
    /// device itself learns it via its next `/pair/poll`).
    pub async fn approve(&self, code: &str) -> Result<(String, String), &'static str> {
        let (name, token) = {
            let mut pairing = self.pairing.lock().unwrap();
            pairing.approve(code)?
        };
        self.state_db
            .save_http_media_device(&token_hash(&token), &name)
            .await
            .map_err(|_| "storage_error")?;
        Ok((name, token))
    }
}

// ---------- axum wiring ----------

#[derive(Clone)]
struct AppState {
    service: Arc<MediaService>,
    state_db: Arc<StateDb>,
    pairing: Arc<Mutex<PairingState>>,
    pair_limiter: Arc<AllocationLimiter>,
    /// The HTTP CA's PEM, handed back once in `/pair/poll`'s `Approved`
    /// response so a freshly paired device learns its trust anchor at the
    /// same moment it learns its bearer token. `None` when the TLS listener
    /// isn't running — such a device is LAN-only and has no relay path to
    /// need a trust anchor for.
    http_ca_pem: Option<String>,
    /// Live SWARM-link status for `/health`.
    link_status: tokio::sync::watch::Receiver<SwarmLinkStatus>,
}

/// `.0` is the paired device's display name (dashboard/log label); `.1` is
/// its `token_hash` — stable for the device's lifetime, unlike its name
/// which an owner can rename in the dashboard, and unlike its address which
/// can roam across an HTTP-only device's reconnects. Used as `playback_owner`
/// so claimed-session reaping (`TranscodeManager::plan` ->
/// `cancel_stale_claimed_for_owner`) runs on this transport exactly as it
/// does for a QUIC peer's certificate fingerprint — see
/// `resolve_for_peer`/`resolve_for_transport` in `swarm-media`'s `serve.rs`.
#[derive(Clone)]
struct AuthenticatedDevice(String, String);

/// Starts the listener as a detached background task (matching every other
/// listener in this app — QUIC's `accept_loop`, `lan.rs`'s TCP accept loop —
/// none of which have a graceful-shutdown mechanism either; see the
/// implementation-plan comment on issue #54 for why that's a deliberate,
/// not-yet-needed omission here too) and returns the owner-facing handle.
pub async fn start(
    service: Arc<MediaService>,
    state_db: Arc<StateDb>,
    bind: SocketAddr,
    tls: Option<HttpMediaTlsConfig>,
    link_status: tokio::sync::watch::Receiver<SwarmLinkStatus>,
) -> std::io::Result<HttpMediaService> {
    let pairing = Arc::new(Mutex::new(PairingState::default()));
    let pair_limiter = Arc::new(AllocationLimiter::new(20, Duration::from_secs(3600)));
    let http_ca_pem = tls.as_ref().map(|config| config.ca.cert_pem.clone());
    let state = AppState {
        service,
        state_db: Arc::clone(&state_db),
        pairing: Arc::clone(&pairing),
        pair_limiter,
        http_ca_pem,
        link_status,
    };

    let pairing_routes = Router::new()
        .route("/pair/begin", post(pair_begin))
        .route("/pair/poll", post(pair_poll))
        .layer(middleware::from_fn(reject_cross_site))
        .layer(middleware::from_fn(require_lan));

    // Answers "is this server reachable, and is it visible to SWARM-paired
    // clients?" for anything monitoring it from the LAN. Unauthenticated like
    // pairing, so it is held to the same LAN-only and cross-site guards and
    // reports only the sanitized link status.
    let health_routes = Router::new()
        .route("/health", get(health))
        .layer(middleware::from_fn(reject_cross_site))
        .layer(middleware::from_fn(require_lan));

    let media_routes = Router::new()
        .route("/play/{entry_key}", post(play))
        .route("/stream/{session_id}/media", get(media_get))
        .route("/media/{entry_key}", get(media_get))
        // `{*rest}` (axum's catch-all), not a fixed `{rendition}/{file}` two
        // segments: swarm-media's safe_hls_path validates each `/`-separated
        // segment individually with no depth limit, and the QUIC dispatch
        // only ever splits `session_id` off the front (`rest.split_once('/')`)
        // — a fixed-depth route here would silently 404 anything nested
        // deeper than one level, a real mismatch this route pattern must not
        // reintroduce.
        .route("/hls/{session_id}/{*rest}", get(media_get))
        .route("/catalog/thumbprint", get(media_get))
        .route("/catalog/manifest", get(media_get))
        .route("/catalog/manifest.gz", get(media_get))
        .route("/catalog/changes", get(media_get))
        .route("/catalog/changes.gz", get(media_get))
        .route("/art/{entry_key}/{kind}", get(media_get))
        // Same resolve_for_network dispatch as everything else above, so no
        // new handler — /stop is a mutation (releases a session early), POST
        // to match /play's convention, but media_get itself has no
        // verb-specific logic.
        .route("/stop/{session_id}", post(media_get))
        .route("/subtitles/{entry_key}/{filename}", get(media_get))
        .route("/errors/report", post(report_client_error))
        .route("/likes/toggle", post(toggle_like))
        .route("/notifications/{device_id}", get(media_get))
        .route(
            "/notifications/{device_id}/{error_id}/dismiss",
            post(media_get),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));

    // Debug-build-only, loopback-only automation hook mirroring the desktop
    // GUI's "Resolve" button for a client-reported problem — see
    // `resolve_client_error_debug`'s doc comment. Not merged into a release
    // binary at all (`cfg(debug_assertions)`), and never behind
    // `require_bearer`: it isn't reachable by a TV client, only by the
    // closed-loop TV UAT suite running on this same machine.
    let mut app = pairing_routes.merge(health_routes).merge(media_routes);
    #[cfg(debug_assertions)]
    {
        app = app.merge(Router::new().route("/errors/{id}/resolve", post(resolve_client_error_debug)));
    }

    // Logs method/path/status/latency for every request. Every authenticated
    // route additionally logs the calling device's name via
    // `resolve_and_respond`'s own `tracing::info!`, since `AuthenticatedDevice`
    // isn't resolved yet at the point this layer runs.
    let app = app.layer(TraceLayer::new_for_http()).with_state(state);

    let listener = TcpListener::bind(bind).await?;
    let local_addr = listener.local_addr()?;
    {
        let app = app.clone();
        tokio::spawn(async move {
            let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
            if let Err(err) = axum::serve(listener, make_service).await {
                tracing::error!(%err, "http media server stopped");
            }
        });
    }

    let tls_local_addr = match tls {
        Some(config) => Some(start_tls_listener(app, config).await?),
        None => None,
    };

    Ok(HttpMediaService {
        pairing,
        state_db,
        local_addr,
        tls_local_addr,
    })
}

/// Issues a fresh leaf from `config.ca` and starts the second, TLS-wrapped
/// listener serving the identical `app` — see the module doc comment and
/// `HttpMediaTlsConfig`. Returns the real bound address (mirrors `start`'s
/// own plain-listener handling of a `:0` ephemeral test port).
async fn start_tls_listener(app: Router, config: HttpMediaTlsConfig) -> std::io::Result<SocketAddr> {
    let leaf = swarm_p2p::http_tls::issue_http_leaf(&config.ca, config.sans)
        .map_err(std::io::Error::other)?;
    let rustls_config = axum_server::tls_rustls::RustlsConfig::from_der(
        vec![leaf.cert_der.to_vec()],
        leaf.key_der.secret_der().to_vec(),
    )
    .await?;

    // A std listener, not tokio's: `axum_server::from_tcp_rustls` takes
    // ownership of one and converts it internally, and binding it
    // synchronously (rather than `TcpListener::bind(..).await`) means the
    // real address is known before this function returns — the same
    // ":0`-in-tests reasoning as the plain listener above, since
    // `axum_server::Server::serve` runs the accept loop itself and offers
    // no synchronous "what did I actually bind" hook otherwise.
    let std_listener = std::net::TcpListener::bind(config.bind)?;
    std_listener.set_nonblocking(true)?;
    let tls_local_addr = std_listener.local_addr()?;
    let server = axum_server::from_tcp_rustls(std_listener, rustls_config)?;

    tokio::spawn(async move {
        let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
        if let Err(err) = server.serve(make_service).await {
            tracing::error!(%err, "http media TLS server stopped");
        }
    });

    Ok(tls_local_addr)
}

/// `GET /health`. `ok` means this HTTP surface is serving; `swarm_link` says
/// whether the server is visible through the SWARM service, which can be
/// down while everything on the LAN works.
async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let link = state.link_status.borrow().public();
    Json(serde_json::json!({ "ok": true, "swarm_link": link }))
}

async fn require_lan(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if !is_lan_ip(addr.ip()) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

/// See the module doc comment's second bullet for why mere presence — not
/// trying to parse same-origin vs. cross-site — is the right, fail-closed
/// check: nothing legitimate that can reach `/pair/*` is ever a browser
/// page's `fetch()`.
async fn reject_cross_site(
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if headers.contains_key("sec-fetch-site") || headers.contains_key("sec-fetch-mode") {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

async fn require_bearer(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let hash = token_hash(token);
    match state.state_db.http_media_device_name(&hash).await {
        Ok(Some(name)) => {
            request
                .extensions_mut()
                .insert(AuthenticatedDevice(name, hash));
            Ok(next.run(request).await)
        }
        Ok(None) => Err(StatusCode::UNAUTHORIZED),
        Err(err) => {
            tracing::error!(%err, "http media auth lookup failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(serde::Deserialize)]
struct PairBeginRequest {
    name: String,
}

#[derive(serde::Serialize)]
struct PairBeginResponse {
    activation_id: String,
    poll_token: String,
    code: String,
    expires_in_seconds: u64,
}

async fn pair_begin(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<PairBeginRequest>,
) -> Result<Json<PairBeginResponse>, StatusCode> {
    if !state.pair_limiter.allow(addr.ip()) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let name = body.name.trim().to_string();
    if name.is_empty() || name.len() > 80 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let started = {
        let mut pairing = state.pairing.lock().unwrap();
        pairing.begin(name, addr.ip())
    }
    .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    Ok(Json(PairBeginResponse {
        activation_id: started.activation_id,
        poll_token: started.poll_token,
        code: started.code,
        expires_in_seconds: started.expires_in_seconds,
    }))
}

#[derive(serde::Deserialize)]
struct PairPollRequest {
    activation_id: String,
    poll_token: String,
}

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum PairPollResponse {
    Pending,
    Approved {
        token: String,
        /// PEM of this server's HTTP CA — the device's sole trust anchor
        /// for the TLS listener (see `swarm_p2p::http_tls`). `None` when
        /// the TLS listener isn't running, in which case the device is
        /// LAN-only. Additive field: existing callers that only read
        /// `token` are unaffected.
        #[serde(skip_serializing_if = "Option::is_none")]
        http_ca_pem: Option<String>,
    },
    Expired,
}

async fn pair_poll(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<PairPollRequest>,
) -> Json<PairPollResponse> {
    let status = {
        let mut pairing = state.pairing.lock().unwrap();
        pairing.poll(&body.activation_id, &body.poll_token, addr.ip())
    };
    Json(match status {
        PollStatus::Pending => PairPollResponse::Pending,
        PollStatus::Approved(token) => PairPollResponse::Approved {
            token,
            http_ca_pem: state.http_ca_pem.clone(),
        },
        PollStatus::Expired => PairPollResponse::Expired,
    })
}

async fn play(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Extension(device): Extension<AuthenticatedDevice>,
    AxumPath(entry_key): AxumPath<String>,
    Json(preferences): Json<PlaybackPreferences>,
) -> Response {
    let request = PeerRequest {
        path: format!("/play/{entry_key}"),
        range: None,
        if_none_match: None,
        playback: Some(preferences),
        error_report: None,
        like: None,
    };
    resolve_and_respond(&state, &request, addr.ip(), &device).await
}

async fn report_client_error(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Extension(device): Extension<AuthenticatedDevice>,
    Json(report): Json<ClientErrorReport>,
) -> Response {
    let request = PeerRequest {
        path: "/errors/report".into(),
        range: None,
        if_none_match: None,
        playback: None,
        error_report: Some(report),
        like: None,
    };
    resolve_and_respond(&state, &request, addr.ip(), &device).await
}

async fn toggle_like(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Extension(device): Extension<AuthenticatedDevice>,
    Json(like): Json<LikeToggle>,
) -> Response {
    let request = PeerRequest {
        path: "/likes/toggle".into(),
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: Some(like),
    };
    resolve_and_respond(&state, &request, addr.ip(), &device).await
}

#[cfg(debug_assertions)]
#[derive(serde::Deserialize, Default)]
struct ResolveClientErrorRequest {
    #[serde(default)]
    comments: Option<String>,
}

/// Debug-build-only automation hook mirroring the desktop GUI's "Resolve"
/// button (the `resolve_client_error` Tauri command in `gui.rs`) so the
/// closed-loop TV UAT suite can drive a report -> resolve -> TV-notification
/// round trip unattended (see scenario 11 of that suite). Never compiled
/// into a release binary, and — unlike every other `/errors/*` route —
/// deliberately not behind `require_bearer`: this is a host-local automation
/// surface for a script running on the same machine as the server, not
/// something a TV client should ever reach, so it's restricted to loopback
/// callers at runtime instead in case a debug build is ever reachable from
/// `http_media_bind`'s `0.0.0.0` default.
#[cfg(debug_assertions)]
async fn resolve_client_error_debug(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumPath(id): AxumPath<i64>,
    body: Option<Json<ResolveClientErrorRequest>>,
) -> StatusCode {
    if !addr.ip().is_loopback() {
        return StatusCode::FORBIDDEN;
    }
    let comments = body.and_then(|Json(req)| req.comments);
    match state
        .service
        .library()
        .resolve_client_error(id, comments.as_deref())
        .await
    {
        Ok(true) => StatusCode::OK,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(err) => {
            tracing::error!(%err, "debug resolve_client_error failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Shared by every non-`/play`, non-`/pair` route (`/stream/{id}/media`,
/// `/media/{entry_key}`, `/hls/{id}/{rendition}/{file}`, `/catalog/*`,
/// `/art/{entry_key}/{kind}`) — every one of these is just an opaque path
/// string to `MediaService::resolve_for_network` already (see
/// `crates/swarm-media/src/serve.rs`'s dispatch), so this reads the real
/// request path via `OriginalUri` instead of extracting per-route path
/// params it doesn't otherwise need. Uses `path_and_query`, not `path()`
/// alone: `/art/*` encodes a thumbnail-width request as a query string
/// (`?w=320`) that `swarm-media`'s `artwork_thumbnail_width` parses back out
/// of the same `PeerRequest.path` field QUIC sends it in — dropping the
/// query here would silently serve full-size artwork instead.
async fn media_get(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Extension(device): Extension<AuthenticatedDevice>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let range = headers.get(header::RANGE).and_then(parse_range_header);
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let path = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| uri.path().to_string());
    let request = PeerRequest {
        path,
        range,
        if_none_match,
        playback: None,
        error_report: None,
        like: None,
    };
    resolve_and_respond(&state, &request, addr.ip(), &device).await
}

fn parse_range_header(value: &axum::http::HeaderValue) -> Option<ByteRange> {
    let text = value.to_str().ok()?;
    let spec = text.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        Some(ByteRange::Suffix {
            last: end.parse().ok()?,
        })
    } else {
        Some(ByteRange::FromTo {
            start: start.parse().ok()?,
            end: if end.is_empty() {
                None
            } else {
                Some(end.parse().ok()?)
            },
        })
    }
}

async fn resolve_and_respond(
    state: &AppState,
    request: &PeerRequest,
    remote_ip: IpAddr,
    device: &AuthenticatedDevice,
) -> Response {
    let AuthenticatedDevice(client, playback_owner) = device;
    let is_lan = is_lan_ip(remote_ip);
    tracing::info!(device = %client, path = %request.path, %remote_ip, is_lan, "http media request");
    let resolved = state
        .service
        .resolve_for_peer(request, is_lan, client, playback_owner)
        .await;

    let status =
        StatusCode::from_u16(resolved.header.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response_headers = HeaderMap::new();
    if let Some(content_type) = resolved.header.content_type.clone() {
        if let Ok(value) = content_type.parse() {
            response_headers.insert(header::CONTENT_TYPE, value);
        }
    }
    if let Some(content_range) = &resolved.header.content_range {
        response_headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
        if let Ok(value) = format!(
            "bytes {}-{}/{}",
            content_range.start, content_range.end, content_range.total
        )
        .parse()
        {
            response_headers.insert(header::CONTENT_RANGE, value);
        }
    }
    // Artwork's 304 Not Modified path depends on this: media_get sends
    // If-None-Match through as PeerRequest.if_none_match, and art() (see
    // crates/swarm-media/src/serve.rs) only short-circuits to 304 when it
    // matches the ETag it would have served — a client only gets that
    // benefit if it can learn today's ETag from a 200 response first.
    if let Some(etag) = &resolved.header.etag {
        if let Ok(value) = etag.parse() {
            response_headers.insert(header::ETAG, value);
        }
    }

    let body = axum::body::Body::from_stream(stream_body(resolved, &state.service));
    (status, response_headers, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip() -> IpAddr {
        "192.168.1.20".parse().unwrap()
    }

    #[test]
    fn begin_then_approve_then_poll_returns_the_token() {
        let mut state = PairingState::default();
        let started = state.begin("Living Room Roku".into(), ip()).unwrap();
        assert_eq!(started.code.len(), 8);

        match state.poll(&started.activation_id, &started.poll_token, ip()) {
            PollStatus::Pending => {}
            _ => panic!("must still be pending before approval"),
        }

        let (name, token) = state.approve(&started.code).unwrap();
        assert_eq!(name, "Living Room Roku");
        assert_eq!(token.len(), 64, "generate_token is 32 bytes hex-encoded");

        match state.poll(&started.activation_id, &started.poll_token, ip()) {
            PollStatus::Approved(polled_token) => assert_eq!(polled_token, token),
            _ => panic!("must be approved after approve()"),
        }
    }

    #[test]
    fn repeated_begin_for_the_same_name_and_ip_reuses_one_pending_code() {
        let mut state = PairingState::default();
        let first = state.begin("Roku".into(), ip()).unwrap();
        let second = state.begin("Roku".into(), ip()).unwrap();
        assert_eq!(first.code, second.code);
        assert_eq!(state.activations.len(), 1);
    }

    #[test]
    fn poll_with_a_wrong_token_or_after_expiry_reports_expired_not_approved() {
        let mut state = PairingState::default();
        let started = state.begin("Roku".into(), ip()).unwrap();
        state.approve(&started.code).unwrap();

        // Wrong poll_token for a real activation_id: must not leak "pending"/
        // "approved" status to a caller that doesn't hold the real token.
        match state.poll(&started.activation_id, "wrong-token", ip()) {
            PollStatus::Expired => {}
            _ => panic!("a mismatched poll_token must read as expired, not approved"),
        }

        state.activations[0].expires_at = Instant::now() - Duration::from_secs(1);
        match state.poll(&started.activation_id, &started.poll_token, ip()) {
            PollStatus::Expired => {}
            _ => panic!("a genuinely expired activation must read as expired"),
        }
    }

    #[test]
    fn approve_with_an_unknown_code_is_a_clean_error() {
        let mut state = PairingState::default();
        assert!(state.approve("00000000").is_err());
    }

    #[test]
    fn allocation_limiter_blocks_after_max_and_exempts_loopback() {
        let limiter = AllocationLimiter::new(2, Duration::from_secs(60));
        assert!(limiter.allow(ip()));
        assert!(limiter.allow(ip()));
        assert!(!limiter.allow(ip()), "third attempt within the window must be blocked");

        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..10 {
            assert!(limiter.allow(loopback), "loopback must never be rate-limited");
        }
    }

    #[test]
    fn token_hash_is_deterministic_and_the_raw_token_never_appears_in_it() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        let hash_once = token_hash(&token);
        let hash_again = token_hash(&token);
        assert_eq!(hash_once, hash_again);
        assert_ne!(hash_once, token);
    }

    #[test]
    fn parses_a_bounded_and_a_suffix_range_header() {
        let bounded: axum::http::HeaderValue = "bytes=500-599".parse().unwrap();
        assert_eq!(
            parse_range_header(&bounded),
            Some(ByteRange::FromTo {
                start: 500,
                end: Some(599)
            })
        );

        let open_ended: axum::http::HeaderValue = "bytes=500-".parse().unwrap();
        assert_eq!(
            parse_range_header(&open_ended),
            Some(ByteRange::FromTo {
                start: 500,
                end: None
            })
        );

        let suffix: axum::http::HeaderValue = "bytes=-100".parse().unwrap();
        assert_eq!(parse_range_header(&suffix), Some(ByteRange::Suffix { last: 100 }));

        let garbage: axum::http::HeaderValue = "not-a-range".parse().unwrap();
        assert_eq!(parse_range_header(&garbage), None);
    }
}
