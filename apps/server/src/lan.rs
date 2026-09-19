//! SWARM-free LAN discovery and TV-first activation.
//!
//! The media server advertises its QUIC endpoint and certificate fingerprint
//! over mDNS. Discovery is intentionally not authorization. A new Android TV
//! asks this server for a short-lived activation code, displays that code, and
//! privately polls with an unrelated random token. The user enters the visible
//! code in the media-server UI; approval persists the TV certificate in
//! `server-state.sqlite` and adds it to the same `AllowedPeers` set used by
//! SWARM roster members. All catalog and playback traffic still uses mTLS.

use crate::state_db::StateDb;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(debug_assertions)]
use std::time::{SystemTime, UNIX_EPOCH};
use swarm_p2p::pin::AllowedPeers;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

pub const SERVICE_TYPE: &str = "_swarm-peer._udp.local.";
const ACTIVATION_TTL: Duration = Duration::from_secs(5 * 60);
const TESTING_MODE_TTL: Duration = Duration::from_secs(10 * 60);
const TESTING_PAIRING_CODE: &str = "00000000";
const MAX_PENDING_ACTIVATIONS: usize = 32;
const MAX_PAIR_REQUEST: usize = 4096;

#[derive(Debug, Clone, Serialize)]
pub struct LanPairingApproval {
    pub name: String,
    pub fingerprint: String,
    pub testing: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LanPairingError {
    #[error("No pending LAN TV uses that code, or the code has expired.")]
    InvalidCode,
    #[error("More than one testing TV is waiting for code 00000000; use automated pairing or leave only one testing TV pending.")]
    AmbiguousTestingCode,
    #[error("Could not save the LAN TV approval: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone)]
struct PendingActivation {
    activation_id: String,
    poll_token: String,
    code: String,
    name: String,
    fingerprint: String,
    requester_ip: IpAddr,
    expires_at: Instant,
    approved: bool,
    testing: bool,
}

#[derive(Default)]
struct PairingState {
    activations: Vec<PendingActivation>,
}

#[derive(Debug)]
struct ActivationStarted {
    activation_id: String,
    poll_token: String,
    code: String,
    expires_in_seconds: u64,
    fingerprint: String,
    approved: bool,
    testing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivationKind {
    Normal,
    Testing { auto_approve: bool },
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
        fingerprint: String,
        requester_ip: IpAddr,
        kind: ActivationKind,
    ) -> Result<ActivationStarted, &'static str> {
        self.purge_expired();
        let testing = matches!(kind, ActivationKind::Testing { .. });
        if let Some(existing) = self.activations.iter().find(|activation| {
            activation.fingerprint == fingerprint
                && activation.requester_ip == requester_ip
                && activation.testing == testing
        }) {
            return Ok(started_from(existing));
        }
        if self.activations.len() >= MAX_PENDING_ACTIVATIONS {
            return Err("too_many_pending_activations");
        }

        let code = if testing {
            TESTING_PAIRING_CODE.to_string()
        } else {
            loop {
                let candidate = format!("{:08}", rand::random::<u32>() % 100_000_000);
                if candidate != TESTING_PAIRING_CODE
                    && self
                        .activations
                        .iter()
                        .all(|activation| activation.code != candidate)
                {
                    break candidate;
                }
            }
        };
        let ttl = if testing {
            TESTING_MODE_TTL
        } else {
            ACTIVATION_TTL
        };
        let activation = PendingActivation {
            activation_id: hex::encode(rand::random::<[u8; 16]>()),
            poll_token: hex::encode(rand::random::<[u8; 24]>()),
            code,
            name,
            fingerprint,
            requester_ip,
            expires_at: Instant::now() + ttl,
            approved: matches!(kind, ActivationKind::Testing { auto_approve: true }),
            testing,
        };
        let started = started_from(&activation);
        self.activations.push(activation);
        Ok(started)
    }

    fn poll(
        &mut self,
        activation_id: &str,
        poll_token: &str,
        requester_ip: IpAddr,
    ) -> &'static str {
        self.purge_expired();
        self.activations
            .iter()
            .find(|activation| {
                activation.activation_id == activation_id
                    && activation.poll_token == poll_token
                    && activation.requester_ip == requester_ip
            })
            .map(|activation| {
                if activation.approved {
                    "approved"
                } else {
                    "pending"
                }
            })
            .unwrap_or("expired")
    }

    fn end_testing(
        &mut self,
        activation_id: &str,
        poll_token: &str,
        requester_ip: IpAddr,
    ) -> Option<(String, String, bool)> {
        let index = self.activations.iter().position(|activation| {
            activation.testing
                && activation.activation_id == activation_id
                && activation.poll_token == poll_token
                && activation.requester_ip == requester_ip
        })?;
        let activation = self.activations.remove(index);
        Some((
            activation.fingerprint,
            activation.activation_id,
            activation.approved,
        ))
    }
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
        fingerprint: activation.fingerprint.clone(),
        approved: activation.approved,
        testing: activation.testing,
    }
}

/// Everything needed to rebuild the mDNS record for a different address.
#[derive(Clone)]
struct AdvertiseParams {
    server_fingerprint: String,
    peer_port: u16,
    pairing_port: u16,
    http_media_port: u16,
    http_media_tls_port: Option<u16>,
}

struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
    params: AdvertiseParams,
}

pub struct LanService {
    pairing: Arc<Mutex<PairingState>>,
    state_db: Arc<StateDb>,
    allowed: AllowedPeers,
    /// Behind a lock because the record's address must be replaced when the
    /// machine changes networks — see [`LanService::refresh_advertisement`].
    advertisement: std::sync::Mutex<Option<Advertisement>>,
}

impl LanService {
    pub async fn start(
        server_fingerprint: String,
        peer_addr: SocketAddr,
        allowed: AllowedPeers,
        state_db: Arc<StateDb>,
        http_media_port: u16,
        http_media_tls_port: Option<u16>,
    ) -> std::io::Result<Self> {
        // TCP and QUIC/UDP can share the same numeric port. Keeping activation
        // on the peer port avoids an unpredictable firewall exception.
        let listener = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, peer_addr.port())).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?
            }
            Err(error) => return Err(error),
        };
        let pairing_port = listener.local_addr()?.port();
        let pairing = Arc::new(Mutex::new(PairingState::default()));
        let listener_pairing = Arc::clone(&pairing);
        let listener_allowed = allowed.clone();
        tokio::spawn(async move {
            loop {
                let (socket, remote) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(err) => {
                        tracing::warn!(%err, "LAN activation listener stopped");
                        return;
                    }
                };
                let state = Arc::clone(&listener_pairing);
                let request_allowed = listener_allowed.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        handle_pair_request(socket, remote, state, request_allowed),
                    )
                    .await
                    {
                        Ok(Err(err)) => {
                            tracing::debug!(%remote, %err, "LAN activation request failed")
                        }
                        Err(_) => tracing::debug!(%remote, "LAN activation request timed out"),
                        Ok(Ok(())) => {}
                    }
                });
            }
        });

        let advertisement = advertise(AdvertiseParams {
            server_fingerprint,
            peer_port: peer_addr.port(),
            pairing_port,
            http_media_port,
            http_media_tls_port,
        });
        Ok(Self {
            pairing,
            state_db,
            allowed,
            advertisement: std::sync::Mutex::new(advertisement),
        })
    }

    /// Re-announces this server on the machine's *current* LAN address.
    ///
    /// The record used to be built once at startup with whatever address the
    /// machine had then. A laptop that moved to another network (or got a new
    /// DHCP lease) kept advertising the old address, so LAN clients kept
    /// discovering a server they could not connect to and showed it offline.
    /// Registering the same service name again replaces the record and
    /// re-announces it. Returns the address now advertised, or `None` when
    /// nothing is being advertised at all.
    pub fn refresh_advertisement(&self) -> Option<IpAddr> {
        let guard = self.advertisement.lock().unwrap_or_else(|p| p.into_inner());
        let advertisement = guard.as_ref()?;
        let address = swarm_p2p::local_addr::detect_local_ipv4();
        let info = build_service_info(&advertisement.params, address)
            .map_err(|err| tracing::warn!(%err, "could not rebuild mDNS advertisement"))
            .ok()?;
        advertisement
            .daemon
            .register(info)
            .map_err(|err| tracing::warn!(%err, "could not re-register mDNS advertisement"))
            .ok()?;
        tracing::info!(%address, "re-announced media server on the LAN after a network change");
        Some(address)
    }

    pub async fn approve_pairing_code(
        &self,
        code: &str,
    ) -> Result<LanPairingApproval, LanPairingError> {
        approve_pending_pairing(&self.pairing, &self.state_db, &self.allowed, code).await
    }
}

impl Drop for LanService {
    fn drop(&mut self) {
        let guard = self.advertisement.get_mut().unwrap_or_else(|p| p.into_inner());
        if let Some(advertisement) = guard.as_ref() {
            let _ = advertisement.daemon.unregister(&advertisement.fullname);
            let _ = advertisement.daemon.shutdown();
        }
    }
}

/// Builds the mDNS record advertising this server at `address`.
fn build_service_info(params: &AdvertiseParams, address: IpAddr) -> Result<ServiceInfo, String> {
    let short = &params.server_fingerprint[..params.server_fingerprint.len().min(12)];
    let instance = format!("SWARM Media Server {short}");
    let hostname = format!("swarm-{short}.local.");
    let peer_port = params.peer_port.to_string();
    let pair_port = params.pairing_port.to_string();
    // Not consumed by the Fire TV client — it only ever reads
    // fingerprint/peer_port/pair_port here and pairs over QUIC, never these
    // two. Advertised for an HTTP-only client (Roku) that can't speak QUIC
    // at all and has no other way to discover these ports; adding them
    // costs nothing and means that client's own resolver work doesn't also
    // need a server-side change.
    let http_media_port_str = params.http_media_port.to_string();
    let http_media_tls_port_str = params.http_media_tls_port.map(|port| port.to_string());
    let mut properties = vec![
        ("protocol", "2"),
        ("name", "SWARM Media Server"),
        ("fingerprint", params.server_fingerprint.as_str()),
        ("peer_port", peer_port.as_str()),
        ("pair_port", pair_port.as_str()),
        ("http_media_port", http_media_port_str.as_str()),
    ];
    // Only present once the TLS listener actually started (see
    // ServerCore::start) — a client must never learn a port nothing is
    // listening on.
    if let Some(tls_port) = &http_media_tls_port_str {
        properties.push(("http_media_tls_port", tls_port.as_str()));
    }
    ServiceInfo::new(
        SERVICE_TYPE,
        &instance,
        &hostname,
        address.to_string(),
        params.peer_port,
        &properties[..],
    )
    .map_err(|err| err.to_string())
}

fn advertise(params: AdvertiseParams) -> Option<Advertisement> {
    let daemon = ServiceDaemon::new()
        .map_err(|err| tracing::warn!(%err, "could not start mDNS advertiser"))
        .ok()?;
    let info = build_service_info(&params, swarm_p2p::local_addr::detect_local_ipv4())
        .map_err(|err| tracing::warn!(%err, "could not build mDNS advertisement"))
        .ok()?;
    let fullname = info.get_fullname().to_string();
    daemon
        .register(info)
        .map_err(|err| tracing::warn!(%err, "could not register mDNS advertisement"))
        .ok()?;
    tracing::info!(
        service = %fullname,
        pairing_port = params.pairing_port,
        http_media_port = params.http_media_port,
        "advertising media server on the LAN"
    );
    Some(Advertisement {
        daemon,
        fullname,
        params,
    })
}

#[derive(Deserialize)]
struct PairRequest {
    action: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    fingerprint: Option<String>,
    #[serde(default)]
    activation_id: Option<String>,
    #[serde(default)]
    poll_token: Option<String>,
    #[serde(default)]
    testing_token: Option<String>,
}

#[cfg(debug_assertions)]
#[derive(Deserialize)]
struct TestingControl {
    token: String,
    expires_at_unix_seconds: u64,
}

#[derive(Default, Serialize)]
struct PairResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    poll_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
}

async fn handle_pair_request(
    mut socket: TcpStream,
    remote: SocketAddr,
    pairing: Arc<Mutex<PairingState>>,
    allowed: AllowedPeers,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !is_lan_address(remote.ip()) {
        write_response(
            &mut socket,
            PairResponse {
                error: Some("not_lan"),
                ..PairResponse::default()
            },
        )
        .await?;
        return Ok(());
    }
    let request_bytes = read_request(&mut socket).await?;
    let request: PairRequest = match serde_json::from_slice(&request_bytes) {
        Ok(request) => request,
        Err(_) => {
            write_response(
                &mut socket,
                PairResponse {
                    error: Some("invalid_request"),
                    ..PairResponse::default()
                },
            )
            .await?;
            return Ok(());
        }
    };

    match request.action.as_str() {
        "begin" | "begin_testing" => {
            let testing = request.action == "begin_testing";
            if testing && !cfg!(debug_assertions) {
                return reject(&mut socket, "testing_unavailable").await;
            }
            let fingerprint = request
                .fingerprint
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return reject(&mut socket, "invalid_fingerprint").await;
            }
            let name = request.name.unwrap_or_default().trim().to_string();
            if name.is_empty() || name.len() > 80 {
                return reject(&mut socket, "invalid_name").await;
            }
            let kind = if testing {
                ActivationKind::Testing {
                    auto_approve: testing_token_is_authorized(request.testing_token.as_deref()),
                }
            } else {
                ActivationKind::Normal
            };
            let started =
                pairing
                    .lock()
                    .await
                    .begin(name.clone(), fingerprint.clone(), remote.ip(), kind);
            match started {
                Ok(started) => {
                    if started.testing && started.approved {
                        authorize_ephemeral_testing_peer(
                            &allowed,
                            &started.fingerprint,
                            &started.activation_id,
                            Duration::from_secs(started.expires_in_seconds),
                        );
                        tracing::warn!(
                            client = %name,
                            fingerprint = %fingerprint,
                            %remote,
                            expires_in_seconds = started.expires_in_seconds,
                            "automatically approved ephemeral LAN TV testing activation"
                        );
                    } else {
                        tracing::info!(client = %name, fingerprint = %fingerprint, %remote, testing, "created pending LAN TV activation");
                    }
                    write_response(
                        &mut socket,
                        PairResponse {
                            ok: true,
                            code: Some(started.code),
                            activation_id: Some(started.activation_id),
                            poll_token: Some(started.poll_token),
                            expires_in_seconds: Some(started.expires_in_seconds),
                            status: Some(if started.approved {
                                "approved"
                            } else {
                                "pending"
                            }),
                            ..PairResponse::default()
                        },
                    )
                    .await?;
                }
                Err(error) => reject(&mut socket, error).await?,
            }
        }
        "poll" => {
            let activation_id = request.activation_id.unwrap_or_default();
            let poll_token = request.poll_token.unwrap_or_default();
            if activation_id.is_empty() || poll_token.is_empty() {
                return reject(&mut socket, "invalid_request").await;
            }
            let status = pairing
                .lock()
                .await
                .poll(&activation_id, &poll_token, remote.ip());
            write_response(
                &mut socket,
                PairResponse {
                    ok: true,
                    status: Some(status),
                    ..PairResponse::default()
                },
            )
            .await?;
        }
        "end_testing" => {
            let activation_id = request.activation_id.unwrap_or_default();
            let poll_token = request.poll_token.unwrap_or_default();
            if activation_id.is_empty() || poll_token.is_empty() {
                return reject(&mut socket, "invalid_request").await;
            }
            let ended = pairing
                .lock()
                .await
                .end_testing(&activation_id, &poll_token, remote.ip());
            let Some((fingerprint, grant_id, approved)) = ended else {
                return reject(&mut socket, "invalid_testing_activation").await;
            };
            if approved {
                allowed.remove_ephemeral(&fingerprint, &grant_id);
            }
            tracing::info!(%fingerprint, "revoked ephemeral LAN TV testing activation");
            write_response(
                &mut socket,
                PairResponse {
                    ok: true,
                    status: Some("ended"),
                    ..PairResponse::default()
                },
            )
            .await?;
        }
        _ => reject(&mut socket, "invalid_request").await?,
    }
    Ok(())
}

fn testing_token_is_authorized(token: Option<&str>) -> bool {
    #[cfg(not(debug_assertions))]
    {
        let _ = token;
        false
    }
    #[cfg(debug_assertions)]
    {
        let Some(token) = token.filter(|value| value.len() >= 32) else {
            return false;
        };
        let Some(path) = std::env::var_os("SWARM_TV_E2E_CONTROL_FILE") else {
            return false;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let Ok(control) = serde_json::from_slice::<TestingControl>(&bytes) else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        control.expires_at_unix_seconds > now && control.token == token
    }
}

fn authorize_ephemeral_testing_peer(
    allowed: &AllowedPeers,
    fingerprint: &str,
    grant_id: &str,
    ttl: Duration,
) {
    allowed.insert_ephemeral(fingerprint, grant_id);
    let allowed = allowed.clone();
    let fingerprint = fingerprint.to_string();
    let grant_id = grant_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(ttl).await;
        allowed.remove_ephemeral(&fingerprint, &grant_id);
        tracing::info!(%fingerprint, "expired ephemeral LAN TV testing activation");
    });
}

async fn approve_pending_pairing(
    pairing: &Arc<Mutex<PairingState>>,
    state_db: &Arc<StateDb>,
    allowed: &AllowedPeers,
    code: &str,
) -> Result<LanPairingApproval, LanPairingError> {
    let normalized_code: String = code.chars().filter(char::is_ascii_digit).collect();
    let mut state = pairing.lock().await;
    state.purge_expired();
    let matching: Vec<usize> = state
        .activations
        .iter()
        .enumerate()
        .filter_map(|(index, activation)| {
            (activation.code == normalized_code && !activation.approved).then_some(index)
        })
        .collect();
    if normalized_code == TESTING_PAIRING_CODE && matching.len() > 1 {
        return Err(LanPairingError::AmbiguousTestingCode);
    }
    let index = matching
        .first()
        .copied()
        .ok_or(LanPairingError::InvalidCode)?;
    let activation = &mut state.activations[index];
    if activation.testing {
        authorize_ephemeral_testing_peer(
            allowed,
            &activation.fingerprint,
            &activation.activation_id,
            activation
                .expires_at
                .saturating_duration_since(Instant::now()),
        );
    } else {
        state_db
            .save_local_peer(&activation.fingerprint, &activation.name)
            .await?;
        allowed.insert(&activation.fingerprint);
    }
    activation.approved = true;
    let approval = LanPairingApproval {
        name: activation.name.clone(),
        fingerprint: activation.fingerprint.clone(),
        testing: activation.testing,
    };
    tracing::info!(client = %approval.name, fingerprint = %approval.fingerprint, testing = approval.testing, "approved local TV activation");
    Ok(approval)
}

async fn read_request(socket: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut request_bytes = Vec::new();
    loop {
        if request_bytes.len() >= MAX_PAIR_REQUEST {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
        let mut byte = [0u8; 1];
        if socket.read(&mut byte).await? == 0 || byte[0] == b'\n' {
            break;
        }
        request_bytes.push(byte[0]);
    }
    Ok(request_bytes)
}

async fn reject(
    socket: &mut TcpStream,
    error: &'static str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_response(
        socket,
        PairResponse {
            error: Some(error),
            ..PairResponse::default()
        },
    )
    .await?;
    Ok(())
}

async fn write_response(socket: &mut TcpStream, response: PairResponse) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(&response).unwrap_or_default();
    bytes.push(b'\n');
    socket.write_all(&bytes).await
}

fn is_lan_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local() || ip.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertise_params() -> AdvertiseParams {
        AdvertiseParams {
            server_fingerprint: "ea138cad6b4d4625fb8e3ba8e77f0472d5fad17be61108e99bdcf5b391af97c0"
                .into(),
            peer_port: 8543,
            pairing_port: 8543,
            http_media_port: 8546,
            http_media_tls_port: Some(8547),
        }
    }

    /// The whole point of refreshing: the same service must be re-announced
    /// with a different address, under the same name (so it replaces the old
    /// record instead of appearing as a second server).
    #[test]
    fn the_advertised_record_follows_the_address_it_is_built_for() {
        let params = advertise_params();
        let home = build_service_info(&params, "192.168.0.133".parse().unwrap()).unwrap();
        let away = build_service_info(&params, "10.4.2.9".parse().unwrap()).unwrap();

        assert_eq!(home.get_fullname(), away.get_fullname());
        assert!(home.get_addresses().iter().any(|a| a.to_string() == "192.168.0.133"));
        assert!(!home.get_addresses().iter().any(|a| a.to_string() == "10.4.2.9"));
        assert!(away.get_addresses().iter().any(|a| a.to_string() == "10.4.2.9"));
        assert!(!away.get_addresses().iter().any(|a| a.to_string() == "192.168.0.133"));
    }

    #[test]
    fn the_record_carries_the_ports_and_fingerprint_clients_read() {
        let info = build_service_info(&advertise_params(), "192.168.0.133".parse().unwrap()).unwrap();
        let prop = |key: &str| info.get_property_val_str(key).map(str::to_string);
        assert_eq!(prop("peer_port").as_deref(), Some("8543"));
        assert_eq!(prop("http_media_port").as_deref(), Some("8546"));
        assert_eq!(prop("http_media_tls_port").as_deref(), Some("8547"));
        assert!(prop("fingerprint").unwrap().starts_with("ea138cad6b4d"));
        assert_eq!(info.get_port(), 8543);
    }

    #[test]
    fn a_tls_port_is_only_advertised_when_its_listener_exists() {
        let mut params = advertise_params();
        params.http_media_tls_port = None;
        let info = build_service_info(&params, "192.168.0.133".parse().unwrap()).unwrap();
        assert_eq!(info.get_property_val_str("http_media_tls_port"), None);
    }

    #[test]
    fn only_private_link_local_or_loopback_addresses_can_pair() {
        assert!(is_lan_address("192.168.1.10".parse().unwrap()));
        assert!(is_lan_address("10.0.0.2".parse().unwrap()));
        assert!(is_lan_address("127.0.0.1".parse().unwrap()));
        assert!(!is_lan_address("8.8.8.8".parse().unwrap()));
    }

    async fn exchange(
        pairing: Arc<Mutex<PairingState>>,
        allowed: AllowedPeers,
        request: serde_json::Value,
    ) -> serde_json::Value {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, remote) = listener.accept().await.unwrap();
            handle_pair_request(socket, remote, pairing, allowed)
                .await
                .unwrap();
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        server.await.unwrap();
        serde_json::from_slice(&response).unwrap()
    }

    #[tokio::test]
    async fn tv_first_activation_persists_authorizes_and_polls_approved() {
        let dir = std::env::temp_dir().join(format!(
            "swarm-lan-activation-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(StateDb::open(&dir).await.unwrap());
        let allowed = AllowedPeers::new();
        let pairing = Arc::new(Mutex::new(PairingState::default()));
        let fingerprint = "ab".repeat(32);

        let started = exchange(
            Arc::clone(&pairing),
            allowed.clone(),
            serde_json::json!({
                "action": "begin",
                "name": "Living Room TV",
                "fingerprint": fingerprint,
            }),
        )
        .await;
        assert_eq!(started["ok"], true);
        assert_eq!(started["code"].as_str().unwrap().len(), 8);
        assert_eq!(started["status"], "pending");

        let approval =
            approve_pending_pairing(&pairing, &db, &allowed, started["code"].as_str().unwrap())
                .await
                .unwrap();
        assert_eq!(approval.name, "Living Room TV");
        assert!(allowed.contains(&fingerprint));
        assert_eq!(db.local_peers().await.unwrap()[0].fingerprint, fingerprint);

        let polled = exchange(
            Arc::clone(&pairing),
            allowed.clone(),
            serde_json::json!({
                "action": "poll",
                "activation_id": started["activation_id"],
                "poll_token": started["poll_token"],
            }),
        )
        .await;
        assert_eq!(polled["ok"], true);
        assert_eq!(polled["status"], "approved");

        drop(db);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn repeated_begin_for_same_tv_reuses_one_pending_code() {
        let mut state = PairingState::default();
        let ip = "192.168.1.20".parse().unwrap();
        let first = state
            .begin("TV".into(), "ab".repeat(32), ip, ActivationKind::Normal)
            .unwrap();
        let second = state
            .begin("TV".into(), "ab".repeat(32), ip, ActivationKind::Normal)
            .unwrap();
        assert_eq!(first.code, second.code);
        assert_eq!(state.activations.len(), 1);
    }

    #[tokio::test]
    async fn testing_activation_uses_fixed_code_without_persisting_trust() {
        let dir = std::env::temp_dir().join(format!(
            "swarm-lan-testing-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(StateDb::open(&dir).await.unwrap());
        let allowed = AllowedPeers::new();
        let pairing = Arc::new(Mutex::new(PairingState::default()));
        let fingerprint = "cd".repeat(32);
        let ip = "192.168.1.21".parse().unwrap();
        let started = pairing
            .lock()
            .await
            .begin(
                "Test TV".into(),
                fingerprint.clone(),
                ip,
                ActivationKind::Testing {
                    auto_approve: false,
                },
            )
            .unwrap();

        assert_eq!(started.code, TESTING_PAIRING_CODE);
        let approval = approve_pending_pairing(&pairing, &db, &allowed, TESTING_PAIRING_CODE)
            .await
            .unwrap();
        assert!(approval.testing);
        assert!(allowed.contains(&fingerprint));
        assert!(db.local_peers().await.unwrap().is_empty());

        let ended = pairing
            .lock()
            .await
            .end_testing(&started.activation_id, &started.poll_token, ip)
            .unwrap();
        allowed.remove_ephemeral(&ended.0, &ended.1);
        assert!(!allowed.contains(&fingerprint));

        drop(db);
        std::fs::remove_dir_all(&dir).ok();
    }
}
