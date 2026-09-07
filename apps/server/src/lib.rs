//! Shared server core: identity → library → scan → pinned QUIC listener,
//! plus SWARM membership (register with a join code, keep the QUIC listener's
//! allowed-peer set synced with the swarm roster). The Tauri desktop app owns
//! this core for its entire process lifetime, including while hidden to tray.

mod bandwidth;
mod http_media;
pub mod lan;
pub mod punch_connect;
mod state_db;
mod subtitle_download;
pub mod transcode_activity;
pub mod transcription;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use swarm_core::peer::MediaKind;
use swarm_core::rest::{
    ActivationPreview, ActivationStatusResponse, DeviceRegistration, DeviceType,
    ProvisionManagedSwarmRequest, SwarmDevicesResponse, SwarmSummary,
};
use swarm_core::signal::{SignalMessage, SignalPayload};
use swarm_media::bandwidth::BandwidthSample;
use swarm_media::roots::{MediaRoot, RootResolver, SharedRootResolver};
use swarm_media::scan::{
    scan_roots_cancellable_with_options, scan_roots_scoped_with_options,
    scan_roots_with_options, ScanOptions, ScanProgressEvent, ScanReport,
};
use swarm_media::scrape::{
    run_bulk_scrape, scrape_one_track, scrape_one_video, BulkScrapeReport, ScrapeConfig,
    ScrapeOneError, ScrapeProgressEvent, TmdbOverride,
};
use swarm_media::serve::{accept_loop, serve_connection, MediaService};
use swarm_media::store::Library;
use swarm_media::transcode::TranscodeConfig;
use swarm_p2p::identity::DeviceIdentity;
use swarm_p2p::pin::AllowedPeers;
use swarm_stun_client::{SignalingClient, StunClient, TokenStore};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use crate::punch_connect::{respond_to_punch_offer, ReceivedOffer};
use crate::transcode_activity::{TranscodeActivityMeter, TranscodeActivitySample};
use crate::transcription::{TranscriptionManager, TranscriptionStatus};

pub use state_db::{HttpMediaDeviceRecord, LocalPeerRecord, ManagedSwarmIdentity, StunLinkRecord};

/// How often a linked server re-fetches its swarms' rosters. Not push-based
/// yet (that lands with WSS presence in Phase 4) — polling is the Phase 2/3
/// stand-in, and it keeps AllowedPeers fresh even while the desktop window is
/// hidden and no GUI action can trigger a manual resync.
const ROSTER_SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Where the STUN access token is stored at rest. See
/// `swarm_stun_client::TokenStore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TokenStoreMode {
    /// Try the OS keychain/credential manager first, falling back to a
    /// permission-restricted file only if no backend is available.
    #[default]
    PreferKeyring,
    /// Skip the keyring entirely — used by the desktop app so unsigned local
    /// rebuilds retain access, and by tests because keyring behavior varies
    /// too much across environments to assert on reliably.
    FileOnly,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// One or more library roots. A single root behaves exactly as before
    /// (its `label` is never written onto `relative_path`); 2+ roots are
    /// distinguished on-disk by a `{label}/` prefix — see
    /// `swarm_media::roots`.
    pub media_roots: Vec<MediaRoot>,
    pub scan_options: ScanOptions,
    pub data_dir: PathBuf,
    pub bind: SocketAddr,
    /// The plain-HTTP(S) pairing + media-playback surface (`http_media.rs`)
    /// for clients that can't speak the QUIC peer protocol — Roku-class
    /// devices. Runs unconditionally once the core starts, the same as
    /// `bind`'s QUIC listener, not gated behind a settings toggle.
    pub http_media_bind: SocketAddr,
    /// A second listener serving the exact same routes as `http_media_bind`
    /// over TLS, using a leaf certificate issued from this server's own
    /// long-lived HTTP CA (`swarm_p2p::http_tls`, deliberately not the QUIC
    /// peer identity — see that module's doc comment). `None` disables it
    /// (every test fixture that doesn't specifically exercise TLS). This is
    /// what lets an HTTP-only client reach the server off-LAN through the
    /// relay, which requires TLS end-to-end so the relay never sees
    /// plaintext.
    pub http_media_tls_bind: Option<SocketAddr>,
    /// Fingerprints allowed to connect regardless of STUN membership — for
    /// running without a STUN server at all (local testing, air-gapped use).
    /// A registered STUN link's roster is added on top of this set, never
    /// replacing it.
    pub allowed_fingerprints: Vec<String>,
    pub token_store_mode: TokenStoreMode,
    /// Public SWARM service used to create or renew the server-owned swarm.
    /// `None` preserves a legacy/manual link unless a managed identity was
    /// already created locally, in which case that identity is still renewed.
    pub managed_rendezvous_url: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerStatus {
    pub fingerprint: String,
    /// `"label: /absolute/path"` per configured root (single-root installs
    /// still get one entry here, just with no prefix applied on-disk).
    pub media_roots: Vec<String>,
    pub listen_addr: String,
    pub entry_count: u64,
    pub thumbprint: String,
    pub streaming_upload_budget_bps: u64,
    pub streaming_upload_budget_enabled: bool,
    pub active_playback_sessions: usize,
    /// True while a scan (initial, rescan, or a root change) is in
    /// progress — the library reflects whatever's been found so far either
    /// way, this is purely informational for a "still scanning…" indicator.
    pub scanning: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeleteAssetReport {
    pub removed_files: u64,
    /// Companion cleanup is best-effort after the primary media file has
    /// been removed. Returning warnings keeps the catalog consistent while
    /// still telling the UI about a permission or transient storage issue.
    pub cleanup_warnings: Vec<String>,
}

struct StunContext {
    client: StunClient,
    token_store: TokenStore,
    access_token: String,
    link: StunLinkRecord,
}

pub struct ServerCore {
    pub identity: DeviceIdentity,
    pub library: Arc<Library>,
    pub media_roots: SharedRootResolver,
    pub allowed: AllowedPeers,
    pub listen_addr: SocketAddr,
    pub http_media_addr: SocketAddr,
    pub http_media_tls_addr: Option<SocketAddr>,
    http_ca: Option<Arc<swarm_p2p::http_tls::HttpCa>>,
    service: Arc<MediaService>,
    transcription: Arc<TranscriptionManager>,
    transcode_activity: Arc<TranscodeActivityMeter>,
    data_dir: PathBuf,
    state_db: Arc<state_db::StateDb>,
    lan_service: lan::LanService,
    http_media: http_media::HttpMediaService,
    /// Fingerprints from `ServerConfig::allowed_fingerprints` — kept
    /// separate so a roster sync can rebuild `allowed` as
    /// `static_fingerprints ∪ swarm_roster` without losing the static set.
    static_fingerprints: Vec<String>,
    token_store_mode: TokenStoreMode,
    stun: Mutex<Option<StunContext>>,
    scraping: AtomicBool,
    /// Serializes every full scan (the initial background one, `rescan`, and
    /// `update_media_roots`) — `scan_roots` snapshots known entries then
    /// walks and reconciles based on that snapshot, so two overlapping scans
    /// of the same root set could race each other's reconciliation and
    /// resurrect/delete entries incorrectly. A later caller simply waits its
    /// turn rather than being rejected.
    scan_lock: tokio::sync::Mutex<()>,
    scan_active: Arc<AtomicBool>,
    scan_status: tokio::sync::watch::Sender<ScanState>,
    comprehensive_check: AtomicBool,
    scan_music_tracks: AtomicBool,
}

struct ScanActivityGuard {
    active: Arc<AtomicBool>,
}

impl ScanActivityGuard {
    fn start(active: &Arc<AtomicBool>) -> Self {
        active.store(true, Ordering::Release);
        Self {
            active: Arc::clone(active),
        }
    }
}

impl Drop for ScanActivityGuard {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

/// The current/last outcome of `ServerCore`'s background or on-demand
/// scanning — see [`ServerCore::start`]'s doc comment for why the initial
/// scan doesn't block startup, and [`ServerCore::wait_for_scan`] for how a
/// caller that specifically needs completion (tests, mainly) can get it.
#[derive(Debug, Clone, Default)]
pub enum ScanState {
    #[default]
    NotStarted,
    Scanning,
    Done(ScanReport),
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("identity error: {0}")]
    Identity(#[from] swarm_p2p::identity::IdentityError),
    #[error("library error: {0}")]
    Library(#[from] sqlx::Error),
    #[error("scan error: {0}")]
    Scan(#[from] swarm_media::scan::ScanError),
    #[error("p2p error: {0}")]
    P2p(#[from] swarm_p2p::endpoint::P2pError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SWARM server error: {0}")]
    Stun(#[from] swarm_stun_client::StunClientError),
    #[error("token storage error: {0}")]
    TokenStore(#[from] swarm_stun_client::TokenStoreError),
    #[error("a scrape is already running")]
    ScrapeInProgress,
    #[error("scrape error: {0}")]
    Scrape(#[from] ScrapeOneError),
    #[error("no library entry with that key")]
    EntryNotFound,
    #[error("at least one media root is required")]
    NoMediaRoots,
    #[error("no media root labeled \"{0}\" exists")]
    MediaRootNotFound(String),
    #[error("refusing to delete unsafe asset path \"{0}\"")]
    UnsafeAssetPath(String),
}

impl ServerCore {
    /// Establish identity, open the library, start serving peers, and
    /// restore any previously-established STUN link — all synchronously.
    /// The initial library scan is spawned in the background rather than
    /// awaited here: a real user's library can be tens of thousands of
    /// files on a network share, taking many minutes to walk, and every
    /// Tauri command touches this same `Arc<ServerCore>` — awaiting the scan
    /// inline meant the very first command after launch (and, since the GUI
    /// builds this core lazily behind a `OnceCell`, therefore *every*
    /// command from *every* tab) blocked on the entire scan before the app
    /// could respond to anything at all. Callers that specifically need the
    /// initial scan's result (mainly tests) can await [`Self::wait_for_scan`].
    pub async fn start(config: ServerConfig) -> Result<Arc<Self>, ServerError> {
        let configured_managed_url = config
            .managed_rendezvous_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_end_matches('/').to_string());
        std::fs::create_dir_all(&config.data_dir)?;
        let identity = swarm_p2p::identity::ensure_identity(&config.data_dir)?;
        // Deliberately non-fatal, unlike the QUIC identity above: this CA
        // only backs the secondary HTTP-only-client TLS surface (see
        // swarm_p2p::http_tls's doc comment), and a failure here must never
        // take down QUIC/LAN connectivity for every other client. The
        // plain-HTTP listener still starts either way; only the TLS one is
        // skipped.
        let http_ca = match swarm_p2p::http_tls::ensure_http_ca(&config.data_dir.join("http_ca"))
        {
            Ok(ca) => Some(Arc::new(ca)),
            Err(err) => {
                tracing::warn!(
                    %err,
                    "could not establish the HTTP media TLS CA; only the plain-HTTP media surface will be available"
                );
                None
            }
        };
        let library = Arc::new(
            Library::open(
                config
                    .data_dir
                    .join("library.sqlite")
                    .to_str()
                    .unwrap_or_default(),
            )
            .await?,
        );
        let state_db = Arc::new(state_db::StateDb::open(&config.data_dir).await?);
        let media_roots = SharedRootResolver::new(RootResolver::new(config.media_roots));

        let static_fingerprints: Vec<String> = config
            .allowed_fingerprints
            .iter()
            .map(|f| f.trim().to_lowercase())
            .collect();
        let allowed = AllowedPeers::new();
        let local_peers = state_db.local_peers().await?;
        let local_fingerprints = local_peers.iter().map(|peer| peer.fingerprint.clone());
        allowed.replace(
            static_fingerprints
                .iter()
                .cloned()
                .chain(local_fingerprints),
        );
        let endpoint = swarm_p2p::endpoint::listen(config.bind, &identity, allowed.clone())?;
        let listen_addr = endpoint.local_addr()?;
        let lan_service = lan::LanService::start(
            identity.fingerprint.clone(),
            listen_addr,
            allowed.clone(),
            Arc::clone(&state_db),
            // The *configured* port, not http_media's actual bound one —
            // http_media::start() hasn't run yet at this point in start(),
            // but for every real deployment (never ":0") the two are the
            // same value anyway, and advertising it doesn't need to block
            // on the listener actually being up.
            config.http_media_bind.port(),
            // Same reasoning, and `None` when TLS is disabled (or its CA
            // failed above) so nothing advertises a port nothing is
            // actually listening on.
            http_ca
                .as_ref()
                .and_then(|_| config.http_media_tls_bind.map(|addr| addr.port())),
        )
        .await?;

        // Seed the streaming budget from the last real measurement (if
        // any) right at construction, so a restart doesn't fall back to
        // the static default for a full probe interval before its first
        // tick completes — see bandwidth.rs.
        let mut transcode_config = transcode_config_from_env(&config.data_dir);
        if let Some(measured_bps) = state_db.latest_bandwidth_measurement().await? {
            transcode_config.max_upload_bps = measured_bps;
        }
        let ffmpeg_path = transcode_config.ffmpeg_path.clone();
        let service = Arc::new(MediaService::with_roots_and_artwork_cache(
            Arc::clone(&library),
            media_roots.clone(),
            transcode_config,
            config.data_dir.join("artwork-cache"),
        ));
        service.replace_client_names(
            local_peers
                .into_iter()
                .map(|peer| (peer.fingerprint, peer.name)),
        );
        tokio::spawn(accept_loop(endpoint, Arc::clone(&service)));
        tokio::spawn(bandwidth::run_periodic_probe(
            Arc::clone(&state_db),
            Arc::clone(service.transcode_manager()),
            bandwidth::interval_from_env(),
        ));
        // Always on, like the QUIC listener above — not gated behind a
        // settings toggle the way MCP is. See http_media.rs's module doc
        // comment for why it gets its own port rather than sharing bind's.
        let http_media_tls = match (&http_ca, config.http_media_tls_bind) {
            (Some(ca), Some(tls_bind)) => Some(http_media::HttpMediaTlsConfig {
                bind: tls_bind,
                ca: Arc::clone(ca),
                // The SANs a relay-reached or LAN-reached client might
                // connect through. `detect_local_ipv4` is the same
                // zero-packet route-table probe `lan.rs`'s mDNS
                // advertisement already trusts for this server's LAN
                // address; loopback/localhost cover same-machine testing.
                sans: vec![
                    "127.0.0.1".to_string(),
                    "localhost".to_string(),
                    swarm_p2p::local_addr::detect_local_ipv4().to_string(),
                ],
            }),
            _ => None,
        };
        let http_media = http_media::start(
            Arc::clone(&service),
            Arc::clone(&state_db),
            config.http_media_bind,
            http_media_tls,
        )
        .await?;
        // Startup always schedules an initial scan below. Start in the active
        // state so the transcription task cannot win the spawn race and load
        // Whisper before that scan has acquired its serialization lock.
        let scan_active = Arc::new(AtomicBool::new(true));
        let transcription = TranscriptionManager::start(
            Arc::clone(&library),
            media_roots.clone(),
            Arc::clone(service.transcode_manager()),
            Arc::clone(&scan_active),
            &config.data_dir,
            ffmpeg_path,
        )
        .await?;

        let transcode_activity = TranscodeActivityMeter::start(
            Arc::downgrade(service.transcode_manager()),
            Arc::downgrade(&transcription),
        );

        let core = Arc::new(Self {
            identity,
            library,
            media_roots,
            allowed,
            listen_addr,
            http_media_addr: http_media.local_addr,
            http_media_tls_addr: http_media.tls_local_addr,
            http_ca,
            service,
            transcription,
            transcode_activity,
            data_dir: config.data_dir,
            state_db,
            lan_service,
            http_media,
            static_fingerprints,
            token_store_mode: config.token_store_mode,
            stun: Mutex::new(None),
            scraping: AtomicBool::new(false),
            scan_lock: tokio::sync::Mutex::new(()),
            scan_active,
            scan_status: tokio::sync::watch::Sender::new(ScanState::NotStarted),
            comprehensive_check: AtomicBool::new(config.scan_options.comprehensive_check),
            scan_music_tracks: AtomicBool::new(config.scan_options.scan_music_tracks),
        });
        // A configured or previously-created managed swarm takes precedence
        // over an old manual link. Previously this restored the old link first
        // and skipped provisioning whenever *any* link existed. The resulting
        // token could browse a normal swarm but did not own a managed one, so
        // TV activation lookup/approval failed with 403.
        let stored_managed_url = core
            .state_db
            .load_managed_swarm_identity()
            .await?
            .map(|identity| identity.base_url);
        let managed_url = configured_managed_url.or(stored_managed_url);
        let mut managed_ready = false;
        if let Some(base_url) = managed_url {
            let name =
                std::env::var("SWARM_DEVICE_NAME").unwrap_or_else(|_| "SWARM Media Server".into());
            match Arc::clone(&core)
                .provision_managed_swarm(&base_url, &name)
                .await
            {
                Ok(_) => managed_ready = true,
                Err(err) => {
                    tracing::warn!(%err, "automatic SWARM provisioning failed; trying the saved link");
                }
            }
        }
        if !managed_ready {
            Arc::clone(&core).restore_stun_link().await;
        }

        // Mark Scanning synchronously, before returning, so a caller that
        // calls wait_for_scan() immediately after start() can never observe
        // the pre-scan NotStarted default and return early.
        core.scan_status.send_modify(|s| *s = ScanState::Scanning);
        let scan_core = Arc::clone(&core);
        tokio::spawn(async move {
            let roots = scan_core.media_roots.roots();
            match scan_core.run_scan(&roots, None).await {
                Ok(report) => tracing::info!(
                    added = report.added,
                    updated = report.updated,
                    removed = report.removed,
                    unchanged = report.unchanged,
                    "initial library scan complete"
                ),
                Err(err) => tracing::error!(%err, "initial library scan failed"),
            }
        });

        Ok(core)
    }

    /// Runs one full scan, serialized against every other scan on this core
    /// (initial, rescan, or a root change) via `scan_lock` — see the lock's
    /// doc comment on `Self` for why concurrent scans of the same root set
    /// would be unsafe, not just wasteful. Updates `scan_status` throughout.
    async fn run_scan(
        &self,
        roots: &[MediaRoot],
        progress_tx: Option<mpsc::Sender<ScanProgressEvent>>,
    ) -> Result<ScanReport, ServerError> {
        self.run_scan_inner(roots, progress_tx, None).await
    }

    async fn run_scan_inner(
        &self,
        roots: &[MediaRoot],
        progress_tx: Option<mpsc::Sender<ScanProgressEvent>>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<ScanReport, ServerError> {
        let _guard = self.scan_lock.lock().await;
        let _scan_activity = ScanActivityGuard::start(&self.scan_active);
        self.scan_status.send_modify(|s| *s = ScanState::Scanning);
        let options = self.scan_options();
        let result = match cancel {
            Some(cancel) => scan_roots_cancellable_with_options(
                &self.library,
                roots,
                progress_tx,
                cancel,
                options,
            )
            .await,
            None => scan_roots_with_options(&self.library, roots, progress_tx, options).await,
        };
        match result {
            Ok(report) => {
                self.scan_status
                    .send_modify(|s| *s = ScanState::Done(report.clone()));
                Ok(report)
            }
            Err(err) => {
                self.scan_status
                    .send_modify(|s| *s = ScanState::Failed(err.to_string()));
                Err(err.into())
            }
        }
    }

    /// Reconcile only named roots while preserving the current full root
    /// namespace. Used by network recovery so a returning SMB share does not
    /// force unrelated local roots to make another complete filesystem walk.
    ///
    /// `pause_transcription` controls whether this scoped scan sets
    /// `scan_active` (which pauses local transcription and surfaces "Paused
    /// while the media library is being scanned."). An automatic, unattended
    /// background rescan — the periodic library watch, or a share recovering
    /// from an outage — should never show the user a message implying a scan
    /// they didn't start is stuck; that pause is reserved for the initial
    /// scan and a scan the user explicitly asked for (repairing an SMB root).
    /// A flaky share can otherwise put one of these scoped scans in flight
    /// often enough to starve transcription almost entirely (#230).
    pub async fn rescan_roots_by_label(
        &self,
        labels: &[String],
        pause_transcription: bool,
    ) -> Result<ScanReport, ServerError> {
        let all_roots = self.media_roots.roots();
        let requested = labels.iter().map(String::as_str).collect::<HashSet<_>>();
        let selected = all_roots
            .iter()
            .filter(|root| requested.contains(root.label.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(missing) = labels
            .iter()
            .find(|label| !selected.iter().any(|root| root.label == label.as_str()))
        {
            return Err(ServerError::MediaRootNotFound(missing.clone()));
        }
        if selected.is_empty() {
            return Err(ServerError::NoMediaRoots);
        }

        let _guard = self.scan_lock.lock().await;
        let _scan_activity =
            pause_transcription.then(|| ScanActivityGuard::start(&self.scan_active));
        self.scan_status
            .send_modify(|state| *state = ScanState::Scanning);
        match scan_roots_scoped_with_options(
            &self.library,
            &selected,
            all_roots.len() > 1,
            None,
            self.scan_options(),
        )
        .await
        {
            Ok(report) => {
                self.scan_status
                    .send_modify(|state| *state = ScanState::Done(report.clone()));
                Ok(report)
            }
            Err(error) => {
                self.scan_status
                    .send_modify(|state| *state = ScanState::Failed(error.to_string()));
                Err(error.into())
            }
        }
    }

    /// The current/last scan outcome — see [`ScanState`].
    pub fn scan_status(&self) -> ScanState {
        self.scan_status.borrow().clone()
    }

    /// Blocks until the current or next scan to complete (or fail) finishes,
    /// returning its report. Mainly for tests that need deterministic
    /// post-scan assertions — regular callers should just read whatever the
    /// library currently has via `scan_status()`/`library.list()` and let it
    /// catch up live, the same way the scrape-progress UI already does.
    pub async fn wait_for_scan(&self) -> Result<ScanReport, String> {
        let mut rx = self.scan_status.subscribe();
        loop {
            match &*rx.borrow() {
                ScanState::Done(report) => return Ok(report.clone()),
                ScanState::Failed(err) => return Err(err.clone()),
                ScanState::NotStarted | ScanState::Scanning => {}
            }
            rx.changed()
                .await
                .expect("ServerCore dropped its own scan_status sender");
        }
    }

    pub async fn rescan(
        &self,
        progress_tx: Option<mpsc::Sender<ScanProgressEvent>>,
    ) -> Result<ScanReport, ServerError> {
        let roots = self.media_roots.roots();
        self.run_scan(&roots, progress_tx).await
    }

    fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            comprehensive_check: self.comprehensive_check.load(Ordering::Acquire),
            scan_music_tracks: self.scan_music_tracks.load(Ordering::Acquire),
        }
    }

    pub fn set_comprehensive_check(&self, enabled: bool) {
        self.comprehensive_check.store(enabled, Ordering::Release);
    }

    pub fn set_scan_music_tracks(&self, enabled: bool) {
        self.scan_music_tracks.store(enabled, Ordering::Release);
    }

    /// Manual full scan with cooperative cancellation. The ordinary
    /// background and root-management scans intentionally remain
    /// non-cancellable; this is reserved for a user-owned maintenance run.
    pub async fn rescan_cancellable(
        &self,
        progress_tx: Option<mpsc::Sender<ScanProgressEvent>>,
        cancel: Arc<AtomicBool>,
    ) -> Result<ScanReport, ServerError> {
        let roots = self.media_roots.roots();
        self.run_scan_inner(&roots, progress_tx, Some(cancel)).await
    }

    /// Permanently delete one asset and every server-managed file attached
    /// to it. The scan lock prevents an overlapping reconciliation from
    /// rediscovering the file between filesystem and catalog deletion.
    /// Shared artwork is retained until its final referencing entry is
    /// deleted; entry-specific thumbnail cache files are never shared.
    pub async fn delete_asset(&self, entry_key: &str) -> Result<DeleteAssetReport, ServerError> {
        let _guard = self.scan_lock.lock().await;
        let manifest = self
            .library
            .asset_deletion_manifest(entry_key)
            .await?
            .ok_or(ServerError::EntryNotFound)?;
        let media_path = self.safe_media_path(&manifest.entry.relative_path)?;
        // Validate every database-sourced media-root path before deleting
        // the primary file. A corrupt/stale artwork path must never leave a
        // still-catalogued entry whose media file is already gone.
        let artwork_paths = manifest
            .artwork_paths
            .iter()
            .map(|path| self.safe_media_path(path))
            .collect::<Result<Vec<_>, _>>()?;
        let unshared_artwork_paths = manifest
            .unshared_artwork_paths
            .iter()
            .map(|path| self.safe_media_path(path))
            .collect::<Result<Vec<_>, _>>()?;

        let mut removed_files = u64::from(remove_file_if_exists(&media_path).await?);
        let mut cleanup_warnings = Vec::new();

        for track in &manifest.subtitle_tracks {
            let subtitle_path = PathBuf::from(&track.file_path);
            if !self.subtitle_path_is_managed(entry_key, &media_path, &subtitle_path) {
                cleanup_warnings.push(format!(
                    "Skipped unrecognized subtitle path: {}",
                    subtitle_path.display()
                ));
                continue;
            }
            match remove_file_if_exists(&subtitle_path).await {
                Ok(removed) => removed_files += u64::from(removed),
                Err(error) => cleanup_warnings.push(format!(
                    "Could not delete subtitle {}: {error}",
                    subtitle_path.display()
                )),
            }
        }

        let mut thumbnail_dirs = HashSet::new();
        for artwork_path in &artwork_paths {
            if let Some(parent) = artwork_path.parent() {
                thumbnail_dirs.insert(parent.join(".swarm-thumbnails"));
            }
        }
        for directory in &thumbnail_dirs {
            match remove_entry_thumbnails(directory, entry_key).await {
                Ok(count) => removed_files += count,
                Err(error) => cleanup_warnings.push(format!(
                    "Could not clean artwork thumbnails in {}: {error}",
                    directory.display()
                )),
            }
        }

        let mut artwork_dirs = HashSet::new();
        for artwork_path in &unshared_artwork_paths {
            if let Some(parent) = artwork_path.parent() {
                artwork_dirs.insert(parent.to_path_buf());
            }
            match remove_file_if_exists(artwork_path).await {
                Ok(removed) => removed_files += u64::from(removed),
                Err(error) => cleanup_warnings.push(format!(
                    "Could not delete artwork {}: {error}",
                    artwork_path.display()
                )),
            }
        }

        // These cache/images folders are server-created. Remove them only
        // when empty; a non-empty directory necessarily still belongs to a
        // sibling asset and is left untouched.
        for directory in thumbnail_dirs.into_iter().chain(artwork_dirs) {
            match tokio::fs::remove_dir(&directory).await {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => cleanup_warnings.push(format!(
                    "Could not remove empty asset folder {}: {error}",
                    directory.display()
                )),
            }
        }

        self.library
            .remove_by_path(&manifest.entry.relative_path)
            .await?;
        Ok(DeleteAssetReport {
            removed_files,
            cleanup_warnings,
        })
    }

    fn safe_media_path(&self, relative_path: &str) -> Result<PathBuf, ServerError> {
        let (root, under_root) = self.media_roots.split(relative_path);
        let relative = Path::new(&under_root);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(ServerError::UnsafeAssetPath(relative_path.to_string()));
        }
        Ok(root.join(relative))
    }

    fn subtitle_path_is_managed(&self, entry_key: &str, media_path: &Path, path: &Path) -> bool {
        if path == transcription::whisper_subtitle_path(media_path) {
            return true;
        }
        let managed_dir = self.data_dir.join("subtitles");
        if path.parent() == Some(managed_dir.as_path())
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!("{entry_key}-")))
        {
            return true;
        }
        // A side-loaded (`source = "external"`) subtitle sidecar the scan
        // matched to this entry: a recognized subtitle file sitting in the
        // media file's own directory or a `Subs/` subfolder beneath it.
        // `media_path` is already root-validated, so anything under its
        // parent is inside a media root too.
        if let Some(media_dir) = media_path.parent() {
            let under_media_dir = path.starts_with(media_dir)
                && path.components().all(|component| {
                    matches!(
                        component,
                        Component::Normal(_) | Component::RootDir | Component::Prefix(_)
                    )
                });
            if under_media_dir
                && path
                    .to_str()
                    .and_then(swarm_media::subtitles::subtitle_extension)
                    .is_some()
            {
                return true;
            }
        }
        false
    }

    /// Live-swap the configured media roots and immediately reconcile the
    /// library against them — no restart required. Shared by every caller
    /// (`ServerCore`'s scan/scrape paths and `MediaService`'s P2P
    /// serving/artwork paths) all observe the new roots on their very next
    /// call, since they hold clones of the same [`SharedRootResolver`]
    /// handle rather than independent copies.
    ///
    /// Reconciliation reuses [`scan_roots`] unchanged: it snapshots every
    /// currently-known entry, walks only the roots passed in, and removes
    /// anything not seen during that walk. Pointing it at a different root
    /// set than the one that produced the current library state therefore
    /// already does the right thing — entries from a removed root are found
    /// nowhere during the walk and get removed exactly like a deleted file
    /// would, with no special-cased "root disappeared" handling needed.
    pub async fn update_media_roots(
        &self,
        roots: Vec<MediaRoot>,
    ) -> Result<ScanReport, ServerError> {
        if roots.is_empty() {
            return Err(ServerError::NoMediaRoots);
        }
        self.media_roots.replace(roots.clone());
        self.run_scan(&roots, None).await
    }

    pub async fn status(&self) -> Result<ServerStatus, ServerError> {
        Ok(ServerStatus {
            fingerprint: self.identity.fingerprint.clone(),
            media_roots: self
                .media_roots
                .roots()
                .iter()
                .map(|root| format!("{}: {}", root.label, root.path.display()))
                .collect(),
            listen_addr: self.listen_addr.to_string(),
            entry_count: self.library.entry_count().await?,
            thumbprint: self.library.thumbprint().await?,
            streaming_upload_budget_bps: self.service.transcode_manager().usable_upload_bps(),
            streaming_upload_budget_enabled: self
                .service
                .transcode_manager()
                .upload_budget_enabled(),
            active_playback_sessions: self.service.transcode_manager().active_sessions(),
            scanning: matches!(&*self.scan_status.borrow(), ScanState::Scanning),
        })
    }

    /// Up to the last 60 minutes of real streaming-bandwidth samples, one
    /// per 5-second bucket — see `swarm_media::bandwidth` — for the Details
    /// tab's live graph and "current" panel.
    pub fn bandwidth_history(&self) -> Vec<BandwidthSample> {
        self.service.bandwidth_meter().history()
    }

    /// Up to the last 60 minutes of transcoding/subtitle activity samples,
    /// one per 5-second bucket — see `crate::transcode_activity` — for the
    /// Details tab's live "Transcoding" graph.
    pub fn transcode_activity_history(&self) -> Vec<TranscodeActivitySample> {
        self.transcode_activity.history()
    }

    /// Fingerprint of this server's HTTP-media TLS CA (`swarm_p2p::http_tls`
    /// — deliberately not the QUIC peer identity fingerprint), for
    /// dashboard/diagnostic display. `None` when the TLS listener never
    /// started (CA generation failed, or `http_media_tls_bind` was `None`).
    pub fn http_ca_fingerprint(&self) -> Option<&str> {
        self.http_ca.as_deref().map(|ca| ca.fingerprint.as_str())
    }

    pub async fn artwork_cache_snapshot(&self) -> swarm_media::artwork_cache::ArtworkCacheSnapshot {
        self.service.artwork_cache_snapshot().await
    }

    /// Enables or pauses the durable local subtitle worker. Pausing is
    /// cooperative and preserves every completed ten-minute segment.
    pub fn set_local_transcription_enabled(&self, enabled: bool) {
        self.transcription.set_enabled(enabled);
    }

    pub fn set_transcription_pause_while_streaming(&self, enabled: bool) {
        self.transcription.set_pause_while_streaming(enabled);
    }

    /// Bulk-generation preference: skip a movie/episode that already has any
    /// subtitle track instead of (re)generating one for it.
    pub fn set_transcription_skip_if_subtitles_exist(&self, enabled: bool) {
        self.transcription.set_skip_if_subtitles_exist(enabled);
    }

    pub async fn transcription_status(&self) -> Result<TranscriptionStatus, ServerError> {
        Ok(self.transcription.status().await?)
    }

    /// Targeted, user-triggered subtitle generation for one movie/episode —
    /// jumps the queue and (re)generates regardless of the bulk
    /// skip-if-exists preference or a previously completed job.
    pub async fn generate_subtitles_for_entry(&self, entry_key: &str) -> Result<(), String> {
        self.transcription.enqueue_entry(entry_key).await
    }

    /// Download one subtitle from OpenSubtitles and register it in the same
    /// durable playback catalog used by generated subtitles.
    pub async fn download_subtitle(
        &self,
        api_key: &str,
        entry_key: &str,
        language: &str,
    ) -> Result<swarm_media::store::SubtitleRecord, String> {
        subtitle_download::download(&self.library, &self.data_dir, api_key, entry_key, language)
            .await
    }

    /// Live preference used by the desktop app. LAN connections always
    /// bypass the budget in `swarm_media::serve`, even when this is true.
    pub fn set_streaming_upload_budget_enabled(&self, enabled: bool) {
        self.service
            .transcode_manager()
            .set_upload_budget_enabled(enabled);
    }

    /// Live operator override for which H.264 encoder transcodes use.
    pub fn set_video_encoder_mode(&self, mode: swarm_media::transcode::VideoEncoderMode) {
        self.service.transcode_manager().set_video_encoder_mode(mode);
    }

    /// Live server-imposed cap on transcode output height (`0` = no cap).
    pub fn set_max_transcode_height(&self, height: u32) {
        self.service
            .transcode_manager()
            .set_max_transcode_height(height);
    }

    /// Live HLS segment length in seconds (clamped to >= 2). Affects sessions
    /// negotiated after the change.
    pub fn set_hls_segment_seconds(&self, seconds: u32) {
        self.service
            .transcode_manager()
            .set_hls_segment_seconds(seconds);
    }

    /// Live opt-in for the server-local artwork read-through cache.
    pub fn set_artwork_disk_cache_enabled(&self, enabled: bool) {
        self.service.set_artwork_disk_cache_enabled(enabled);
    }

    /// Scrape metadata/artwork for entries that don't have any yet. Rejects
    /// a concurrent call rather than racing two bulk jobs against the same
    /// library (the Drone's module-level-lock discipline). `progress_tx`,
    /// when given, receives one [`ScrapeProgressEvent`] per entry as it
    /// completes — entirely optional so this method still works exactly as
    /// before for any caller that doesn't need live updates.
    /// `force`: re-scrape and overwrite every entry, not just ones missing a
    /// scrape result — the UI's "redownload / override existing" checkbox.
    pub async fn run_scrape(
        &self,
        config: ScrapeConfig,
        progress_tx: Option<mpsc::UnboundedSender<ScrapeProgressEvent>>,
        force: bool,
    ) -> Result<BulkScrapeReport, ServerError> {
        self.run_scrape_inner(config, progress_tx, force, Arc::new(AtomicBool::new(false)))
            .await
    }

    /// Cancellable bulk scrape used by the desktop library-maintenance
    /// workflow. Work already written remains valid; cancellation stops
    /// before the next entry or lyric lookup.
    pub async fn run_scrape_cancellable(
        &self,
        config: ScrapeConfig,
        progress_tx: Option<mpsc::UnboundedSender<ScrapeProgressEvent>>,
        force: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<BulkScrapeReport, ServerError> {
        self.run_scrape_inner(config, progress_tx, force, cancel)
            .await
    }

    async fn run_scrape_inner(
        &self,
        config: ScrapeConfig,
        progress_tx: Option<mpsc::UnboundedSender<ScrapeProgressEvent>>,
        force: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<BulkScrapeReport, ServerError> {
        if self.scraping.swap(true, Ordering::AcqRel) {
            return Err(ServerError::ScrapeInProgress);
        }
        let result = run_bulk_scrape(
            &self.library,
            &self.media_roots,
            &config,
            &cancel,
            progress_tx,
            force,
        )
        .await;
        self.scraping.store(false, Ordering::Release);
        Ok(result?)
    }

    /// Pinpoint rescrape of one entry — unlike [`Self::run_scrape`], this
    /// succeeds even on an already-scraped entry (correcting a wrong match
    /// is the whole point) and is not gated by the bulk-scrape-in-progress
    /// guard, since it's a single targeted lookup rather than a library-wide
    /// job. `tmdb_override` is ignored for music entries (no TMDb concept
    /// there); a track rescrape always re-syncs its whole (artist, album)
    /// group, matching bulk behavior.
    pub async fn rescrape_entry(
        &self,
        entry_key: &str,
        config: ScrapeConfig,
        tmdb_override: Option<TmdbOverride>,
    ) -> Result<(), ServerError> {
        let entry = self
            .library
            .get(entry_key)
            .await?
            .ok_or(ServerError::EntryNotFound)?;
        match entry.kind {
            MediaKind::Track => {
                scrape_one_track(&self.library, &self.media_roots, &config, &entry).await?;
            }
            MediaKind::Movie | MediaKind::Episode => {
                scrape_one_video(
                    &self.library,
                    &self.media_roots,
                    &config,
                    &entry,
                    tmdb_override,
                )
                .await?;
            }
        }
        Ok(())
    }

    fn token_store(&self) -> Result<TokenStore, ServerError> {
        let fallback_path = self.data_dir.join("stun-token");
        match self.token_store_mode {
            TokenStoreMode::FileOnly => Ok(TokenStore::file_only(fallback_path)),
            TokenStoreMode::PreferKeyring => Ok(TokenStore::new(
                "swarm-server",
                &self.identity.fingerprint,
                fallback_path,
            )?),
        }
    }

    fn managed_claim_store(&self) -> Result<TokenStore, ServerError> {
        let fallback_path = self.data_dir.join("managed-swarm-claim");
        match self.token_store_mode {
            TokenStoreMode::FileOnly => Ok(TokenStore::file_only(fallback_path)),
            TokenStoreMode::PreferKeyring => Ok(TokenStore::new(
                "swarm-server-managed-owner",
                &self.identity.fingerprint,
                fallback_path,
            )?),
        }
    }

    fn server_registration(&self, device_name: &str, machine_id: String) -> DeviceRegistration {
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "peer_addr".to_string(),
            swarm_p2p::local_addr::detect_local_addr(self.listen_addr.port()).to_string(),
        );
        DeviceRegistration {
            name: device_name.to_string(),
            device_type: DeviceType::Server,
            machine_id,
            cert_fingerprint: self.identity.fingerprint.clone(),
            platform: std::env::consts::OS.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            metadata,
        }
    }

    /// Idempotently creates or renews the private swarm this media server
    /// owns. The claim secret lives in a separate OS credential entry (or a
    /// 0600 fallback file), never in SQLite.
    pub async fn provision_managed_swarm(
        self: Arc<Self>,
        base_url: &str,
        device_name: &str,
    ) -> Result<SwarmSummary, ServerError> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let claim_store = self.managed_claim_store()?;
        let existing = self.state_db.load_managed_swarm_identity().await?;
        let (mut identity, claim_token) = match existing {
            Some(identity) => {
                let claim = claim_store.load()?.ok_or_else(|| {
                    ServerError::Stun(swarm_stun_client::StunClientError::Decode(
                        "managed swarm identity exists but its owner credential is missing".into(),
                    ))
                })?;
                (identity, claim)
            }
            None => {
                let identity = ManagedSwarmIdentity {
                    base_url: base_url.clone(),
                    swarm_id: swarm_stun_client::random_token(),
                };
                let claim = swarm_stun_client::random_token();
                claim_store.save(&claim)?;
                self.state_db.save_managed_swarm_identity(&identity).await?;
                (identity, claim)
            }
        };
        let machine_id = swarm_stun_client::machine_id::ensure_machine_id(&self.data_dir)?;
        let registration = self.server_registration(device_name, machine_id);
        let client = StunClient::new(base_url.clone());
        let response = client
            .provision_managed_swarm(ProvisionManagedSwarmRequest {
                swarm_id: identity.swarm_id.clone(),
                claim_token,
                swarm_name: format!("{}'s SWARM", device_name.trim()),
                device: registration,
            })
            .await?;
        // The configured endpoint is authoritative, but persist it only
        // after the existing swarm id + owner claim have been accepted by
        // that endpoint. This lets a service survive a hostname/IP change
        // (notably DHCP after a desktop restart) without silently trusting
        // an address that did not prove it owns the same managed swarm.
        if identity.base_url.trim_end_matches('/') != base_url {
            identity.base_url = base_url.clone();
            self.state_db.save_managed_swarm_identity(&identity).await?;
        }
        let token_store = self.token_store()?;
        token_store.save(&response.access_token)?;
        let link = StunLinkRecord {
            base_url,
            device_id: response.device_id.clone(),
            swarms: vec![response.swarm.clone()],
        };
        self.state_db.save_stun_link(&link).await?;
        self.establish_signaling(&link.base_url, &response.access_token, &link.device_id)
            .await;
        *self.stun.lock().await = Some(StunContext {
            client,
            token_store,
            access_token: response.access_token,
            link,
        });
        Arc::clone(&self).spawn_roster_sync_loop();
        self.sync_roster().await?;
        Ok(response.swarm)
    }

    /// Redeem a join code against a STUN server, persist the link + token,
    /// and start keeping `allowed` synced with the swarm roster.
    pub async fn register_with_stun(
        self: &Arc<Self>,
        base_url: &str,
        code: &str,
        device_name: &str,
    ) -> Result<SwarmSummary, ServerError> {
        let machine_id = swarm_stun_client::machine_id::ensure_machine_id(&self.data_dir)?;
        // Submitted immediately so a client checking the roster right after
        // this server joins doesn't have to wait for the first periodic
        // sync tick to learn where to dial it — see sync_roster for the
        // ongoing refresh.
        let registration = self.server_registration(device_name, machine_id);
        let base_url = base_url.trim_end_matches('/').to_string();
        let client = StunClient::new(base_url.clone());
        let response = client.register_device(code, registration).await?;

        let token_store = self.token_store()?;
        token_store.save(&response.access_token)?;
        let link = StunLinkRecord {
            base_url,
            device_id: response.device_id.clone(),
            swarms: vec![response.swarm.clone()],
        };
        self.state_db.save_stun_link(&link).await?;

        self.establish_signaling(&link.base_url, &response.access_token, &link.device_id)
            .await;
        *self.stun.lock().await = Some(StunContext {
            client,
            token_store,
            access_token: response.access_token,
            link,
        });
        Arc::clone(self).spawn_roster_sync_loop();
        self.sync_roster().await?;
        Ok(response.swarm)
    }

    /// Add an already-linked device to another swarm with a fresh code.
    pub async fn join_additional_swarm(&self, code: &str) -> Result<SwarmSummary, ServerError> {
        let swarm = {
            let mut guard = self.stun.lock().await;
            let ctx = guard.as_mut().ok_or(ServerError::Stun(
                swarm_stun_client::StunClientError::Network(
                    "not linked to a SWARM server yet".into(),
                ),
            ))?;
            let swarm = ctx.client.join_swarm(&ctx.access_token, code).await?;
            ctx.link.swarms.push(swarm.clone());
            self.state_db.save_stun_link(&ctx.link).await?;
            swarm
        };
        self.sync_roster().await?;
        Ok(swarm)
    }

    /// Leave one swarm this server belongs to, keeping the STUN link (and
    /// its other swarm memberships) intact — symmetric with
    /// `join_additional_swarm`. Shrinks `allowed` via the roster resync
    /// that follows.
    pub async fn leave_swarm(&self, swarm_id: &str) -> Result<(), ServerError> {
        {
            let mut guard = self.stun.lock().await;
            let ctx = guard.as_mut().ok_or(ServerError::Stun(
                swarm_stun_client::StunClientError::Network(
                    "not linked to a SWARM server yet".into(),
                ),
            ))?;
            ctx.client
                .leave_swarm(&ctx.access_token, swarm_id, &ctx.link.device_id)
                .await?;
            ctx.link.swarms.retain(|s| s.id != swarm_id);
            self.state_db.save_stun_link(&ctx.link).await?;
        }
        self.sync_roster().await?;
        Ok(())
    }

    /// The currently-linked STUN server and swarms, if any.
    pub async fn stun_link(&self) -> Option<StunLinkRecord> {
        self.stun.lock().await.as_ref().map(|ctx| ctx.link.clone())
    }

    /// One joined swarm's device roster — a straight passthrough to the STUN
    /// server's own view, for display in a GUI. Unlike `sync_roster`, this
    /// never touches `allowed`; it's read-only from this device's
    /// perspective.
    pub async fn swarm_devices(&self, swarm_id: &str) -> Result<SwarmDevicesResponse, ServerError> {
        let guard = self.stun.lock().await;
        let ctx = guard.as_ref().ok_or(ServerError::Stun(
            swarm_stun_client::StunClientError::Network("not linked to a SWARM server yet".into()),
        ))?;
        Ok(ctx
            .client
            .swarm_devices(&ctx.access_token, swarm_id)
            .await?)
    }

    pub async fn lookup_activation(&self, code: &str) -> Result<ActivationPreview, ServerError> {
        let guard = self.stun.lock().await;
        let ctx = guard.as_ref().ok_or(ServerError::Stun(
            swarm_stun_client::StunClientError::Network("not linked to a SWARM service yet".into()),
        ))?;
        Ok(ctx
            .client
            .lookup_activation(&ctx.access_token, code)
            .await?)
    }

    pub async fn approve_activation(
        &self,
        activation_id: &str,
    ) -> Result<ActivationStatusResponse, ServerError> {
        let guard = self.stun.lock().await;
        let ctx = guard.as_ref().ok_or(ServerError::Stun(
            swarm_stun_client::StunClientError::Network("not linked to a SWARM service yet".into()),
        ))?;
        let result = ctx
            .client
            .approve_activation(&ctx.access_token, activation_id)
            .await?;
        drop(guard);
        self.sync_roster().await?;
        Ok(result)
    }

    /// Manually trigger a roster re-sync (a GUI "Resync" button, or a test
    /// that doesn't want to wait for `ROSTER_SYNC_INTERVAL`). Also runs on
    /// that fixed schedule automatically while linked. Returns the number of
    /// distinct peer fingerprints now allowed.
    pub async fn resync(&self) -> Result<usize, ServerError> {
        self.sync_roster().await
    }

    /// Approves the short-lived code displayed by a TV on this LAN. The
    /// pending request is bound to that TV's certificate fingerprint; once
    /// approved, future mTLS reconnects do not require another code.
    pub async fn approve_lan_pairing(
        &self,
        code: &str,
    ) -> Result<lan::LanPairingApproval, lan::LanPairingError> {
        let approval = self.lan_service.approve_pairing_code(code).await?;
        self.service
            .set_client_name(approval.fingerprint.clone(), approval.name.clone());
        Ok(approval)
    }

    pub async fn local_peers(&self) -> Result<Vec<LocalPeerRecord>, ServerError> {
        Ok(self.state_db.local_peers().await?)
    }

    pub async fn revoke_local_peer(&self, fingerprint: &str) -> Result<(), ServerError> {
        self.state_db.remove_local_peer(fingerprint).await?;
        self.sync_roster().await?;
        Ok(())
    }

    /// The same `MediaService`/`TranscodeManager` instance the QUIC accept
    /// loop drives — used by `http_media.rs` so an HTTP-served session
    /// shares one accounting system with QUIC-served ones, not a duplicate.
    pub fn media_service(&self) -> &Arc<MediaService> {
        &self.service
    }

    /// Approves the short-lived code displayed by an HTTP-only (Roku-class)
    /// device — see `http_media.rs`'s module doc comment for how this
    /// differs from `approve_lan_pairing`'s cert-based flow. Owner-only,
    /// never called from the network.
    pub async fn approve_http_media_pairing(
        &self,
        code: &str,
    ) -> Result<(String, String), &'static str> {
        self.http_media.approve(code).await
    }

    pub async fn http_media_devices(&self) -> Result<Vec<HttpMediaDeviceRecord>, ServerError> {
        Ok(self.state_db.http_media_devices().await?)
    }

    pub async fn revoke_http_media_device(&self, token_hash: &str) -> Result<(), ServerError> {
        self.state_db.remove_http_media_device(token_hash).await?;
        Ok(())
    }

    async fn restore_stun_link(self: Arc<Self>) {
        let Some(link) = self.state_db.load_stun_link().await.unwrap_or_else(|err| {
            tracing::warn!(%err, "could not read saved STUN link; starting unlinked");
            None
        }) else {
            return;
        };
        let token_store = match self.token_store() {
            Ok(store) => store,
            Err(err) => {
                tracing::warn!(%err, "could not open token store; STUN link not restored");
                return;
            }
        };
        let access_token = match token_store.load() {
            Ok(Some(token)) => token,
            Ok(None) => {
                tracing::warn!(
                    "stun-link.json present but no access token stored; re-registration required"
                );
                return;
            }
            Err(err) => {
                tracing::warn!(%err, "could not read stored access token; re-registration required");
                return;
            }
        };
        let client = StunClient::new(link.base_url.clone());
        self.establish_signaling(&link.base_url, &access_token, &link.device_id)
            .await;
        *self.stun.lock().await = Some(StunContext {
            client,
            token_store,
            access_token,
            link,
        });
        tracing::info!("restored STUN link, starting roster sync");
        Arc::clone(&self).spawn_roster_sync_loop();
        if let Err(err) = self.sync_roster().await {
            tracing::debug!(%err, "initial roster sync after restore failed; will retry on schedule");
        }
    }

    /// Opens a signaling session and, if that succeeds, resolves the
    /// reflector's address and starts the punch-dispatch loop. Best-effort
    /// and never fatal to the caller: a server with no working signaling
    /// session still serves LAN direct-play peers via `peer_addr` just
    /// fine, it just can't accept a connection from anyone off-LAN —
    /// logged, not propagated as an error.
    async fn establish_signaling(
        self: &Arc<Self>,
        base_url: &str,
        access_token: &str,
        device_id: &str,
    ) {
        let (signaling, signal_rx) = match SignalingClient::connect(
            base_url,
            access_token,
            device_id,
            None,
        )
        .await
        {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(%err, "could not open a signaling session; hole-punch connections unavailable on this link");
                return;
            }
        };
        let Some(reflector_addr) =
            resolve_reflector_addr(base_url, &signaling.reflector_ports).await
        else {
            tracing::warn!("could not resolve the reflector's address; hole-punch connections unavailable on this link");
            return;
        };
        Arc::clone(self).spawn_punch_dispatch_loop(signaling, signal_rx, reflector_addr);
    }

    /// Owns the signaling receiver for as long as this link lives: reacts to
    /// an incoming `Offer` from a swarm-mate by answering it, punching, and
    /// — once mutually confirmed — serving the resulting QUIC connection
    /// exactly like one accepted on the main listener. Everything else
    /// (presence, stray signals) is ignored; nothing else on this server
    /// reads from this receiver, so there's no contention to design around.
    ///
    /// Known limitation, not solved here: only one punch negotiation runs
    /// at a time, since answering one offer borrows this receiver until
    /// that attempt finishes or times out (see `punch_connect`'s module
    /// doc). A second peer's offer arriving mid-negotiation sits in the
    /// channel until the first attempt is done, rather than being handled
    /// concurrently.
    fn spawn_punch_dispatch_loop(
        self: Arc<Self>,
        signaling: SignalingClient,
        mut signal_rx: mpsc::UnboundedReceiver<SignalMessage>,
        reflector_addr: SocketAddr,
    ) {
        tokio::spawn(async move {
            loop {
                let message = match signal_rx.recv().await {
                    Some(message) => message,
                    None => {
                        tracing::debug!("signaling session closed; no longer accepting hole-punched connections");
                        return;
                    }
                };
                let SignalMessage::Signal {
                    from: Some(from),
                    payload:
                        SignalPayload::Offer {
                            punch_id,
                            candidates,
                            cert_fingerprint,
                        },
                    ..
                } = message
                else {
                    continue;
                };
                let offer = ReceivedOffer {
                    from: from.clone(),
                    punch_id,
                    candidates,
                    cert_fingerprint,
                };
                match respond_to_punch_offer(
                    &signaling,
                    &mut signal_rx,
                    reflector_addr,
                    offer,
                    &self.identity,
                    self.allowed.clone(),
                )
                .await
                {
                    Ok(connection) => {
                        tracing::info!(peer = %from, "hole-punched connection established");
                        tokio::spawn(serve_connection(connection, Arc::clone(&self.service)));
                    }
                    Err(err) => {
                        tracing::debug!(peer = %from, %err, "hole-punch negotiation failed")
                    }
                }
            }
        });
    }

    fn spawn_roster_sync_loop(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(ROSTER_SYNC_INTERVAL);
            interval.tick().await; // fires immediately; start()/register already did one sync
            loop {
                interval.tick().await;
                if let Err(err) = self.sync_roster().await {
                    tracing::debug!(%err, "swarm roster sync failed; will retry next tick");
                }
            }
        });
    }

    /// Fetch every joined swarm's roster and rebuild `allowed` as
    /// `static_fingerprints ∪ swarm_members` (excluding this device). On any
    /// fetch error the previous `allowed` set is left untouched — a
    /// transient STUN outage must never silently strand connected peers.
    async fn sync_roster(&self) -> Result<usize, ServerError> {
        let guard = self.stun.lock().await;
        let mut fingerprints: HashSet<String> = self.static_fingerprints.iter().cloned().collect();
        let local_peers = self.state_db.local_peers().await?;
        let mut client_names: HashMap<String, String> = local_peers
            .iter()
            .map(|peer| (peer.fingerprint.clone(), peer.name.clone()))
            .collect();
        fingerprints.extend(local_peers.into_iter().map(|peer| peer.fingerprint));
        let Some(ctx) = guard.as_ref() else {
            let count = fingerprints.len();
            self.allowed.replace(fingerprints);
            self.service.replace_client_names(client_names);
            return Ok(count);
        };

        // Best-effort: keep the connectable address peers see on the STUN
        // roster fresh (DHCP renewal, wifi reconnect, ...). Never blocks or
        // fails the allowed-peer sync below — a stale address just means a
        // client's next connect attempt uses last-known-good info, same
        // spirit as the peer route memory the Kotlin/Rust P2P clients keep.
        let mut self_metadata = BTreeMap::new();
        self_metadata.insert(
            "peer_addr".to_string(),
            swarm_p2p::local_addr::detect_local_addr(self.listen_addr.port()).to_string(),
        );
        if let Err(err) = ctx
            .client
            .patch_metadata(&ctx.access_token, &ctx.link.device_id, self_metadata)
            .await
        {
            tracing::debug!(%err, "failed to self-report peer address this cycle");
        }

        for swarm in &ctx.link.swarms {
            match ctx.client.swarm_devices(&ctx.access_token, &swarm.id).await {
                Ok(roster) => {
                    for device in roster.devices {
                        if device.cert_fingerprint != self.identity.fingerprint {
                            client_names.insert(device.cert_fingerprint.clone(), device.name);
                            fingerprints.insert(device.cert_fingerprint);
                        }
                    }
                }
                Err(err) if err.is_unauthorized() => {
                    tracing::warn!(swarm = %swarm.name, "STUN access token was rejected (revoked?); clearing it");
                    let _ = ctx.token_store.delete();
                    return Err(err.into());
                }
                Err(err) => {
                    tracing::debug!(swarm = %swarm.name, %err, "roster fetch failed, keeping previous allowed-peer set");
                    return Err(err.into());
                }
            }
        }
        let count = fingerprints.len();
        self.allowed.replace(fingerprints);
        self.service.replace_client_names(client_names);
        tracing::debug!(count, "allowed-peer set synced from swarm roster(s)");
        Ok(count)
    }
}

async fn remove_file_if_exists(path: &Path) -> std::io::Result<bool> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

async fn remove_entry_thumbnails(directory: &Path, entry_key: &str) -> std::io::Result<u64> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let prefix = format!("{entry_key}-");
    let mut removed = 0;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_string_lossy().starts_with(&prefix)
            && remove_file_if_exists(&entry.path()).await?
        {
            removed += 1;
        }
    }
    Ok(removed)
}

/// The reflector runs inside the STUN server process (`docs/PROTOCOL.md`),
/// so its address is the STUN base URL's host plus whichever port
/// `hello_ack` advertised as live — resolved via DNS since the host in a
/// base URL is as likely to be a domain name as a literal IP.
async fn resolve_reflector_addr(base_url: &str, reflector_ports: &[u16]) -> Option<SocketAddr> {
    let port = *reflector_ports.first()?;
    let without_scheme = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))?;
    let host_and_port = without_scheme.split('/').next().unwrap_or(without_scheme);
    let host = host_and_port.split(':').next().unwrap_or(host_and_port);
    tokio::net::lookup_host((host, port)).await.ok()?.next()
}

/// Desktop-server transcode settings. The usable streaming budget is
/// `max_upload * (1 - reserve_percent)`; every negotiated playback session
/// reserves from that one aggregate pool.
pub fn transcode_config_from_env(data_dir: &std::path::Path) -> TranscodeConfig {
    let max_upload_mbps = std::env::var("SWARM_MAX_UPLOAD_MBPS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(10.0);
    let reserve_percent = std::env::var("SWARM_UPLOAD_RESERVE_PERCENT")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(90)
        .min(90);
    let max_sessions = std::env::var("SWARM_MAX_STREAMS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(2);
    let disabled = std::env::var("SWARM_TRANSCODING_DISABLED")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false);
    let video_encoder_mode = std::env::var("SWARM_VIDEO_ENCODER")
        .ok()
        .map(|value| swarm_media::transcode::VideoEncoderMode::from_str_lenient(&value))
        .unwrap_or_default();
    let max_transcode_height = std::env::var("SWARM_MAX_TRANSCODE_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let segment_duration_secs = std::env::var("SWARM_HLS_SEGMENT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .map(|value| value.max(2))
        .unwrap_or(4);
    TranscodeConfig {
        enabled: !disabled,
        ffmpeg_path: resolve_ffmpeg_path(),
        session_dir: data_dir.join("transcodes"),
        max_upload_bps: (max_upload_mbps * 1_000_000.0) as u64,
        reserve_percent,
        max_sessions,
        idle_timeout: std::time::Duration::from_secs(300),
        segment_duration_secs,
        video_encoder_mode,
        max_transcode_height,
    }
}

/// Resolve FFmpeg while the process still has its startup environment.
///
/// macOS GUI applications normally receive a much smaller `PATH` than an
/// interactive shell, so a Homebrew or MacPorts FFmpeg can be installed and
/// still be invisible when the server is opened from Finder or at login. Keep
/// the documented override authoritative, then search `PATH`, then the common
/// package-manager locations used by both Apple Silicon and Intel Macs.
fn resolve_ffmpeg_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    let platform_candidates = [
        PathBuf::from("/opt/homebrew/bin/ffmpeg"),
        PathBuf::from("/usr/local/bin/ffmpeg"),
        PathBuf::from("/opt/local/bin/ffmpeg"),
    ];
    #[cfg(not(target_os = "macos"))]
    let platform_candidates: [PathBuf; 0] = [];

    resolve_ffmpeg_path_from(
        std::env::var_os("SWARM_FFMPEG_PATH"),
        std::env::var_os("PATH"),
        &platform_candidates,
    )
}

fn resolve_ffmpeg_path_from(
    configured: Option<std::ffi::OsString>,
    search_path: Option<std::ffi::OsString>,
    platform_candidates: &[PathBuf],
) -> PathBuf {
    if let Some(configured) = configured {
        return PathBuf::from(configured);
    }

    let executable_name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    search_path
        .as_deref()
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join(executable_name))
        .chain(platform_candidates.iter().cloned())
        .find(|candidate| is_executable_file(candidate))
        .unwrap_or_else(|| PathBuf::from(executable_name))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod ffmpeg_path_tests {
    use super::*;

    #[test]
    fn configured_ffmpeg_path_is_authoritative() {
        let configured = PathBuf::from("/custom/tools/ffmpeg");
        let resolved = resolve_ffmpeg_path_from(
            Some(configured.clone().into_os_string()),
            None,
            &[PathBuf::from("/another/ffmpeg")],
        );

        assert_eq!(resolved, configured);
    }

    #[test]
    fn finds_ffmpeg_in_search_path() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let ffmpeg = second.path().join(if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        });
        std::fs::write(&ffmpeg, b"test executable").unwrap();
        make_executable(&ffmpeg);
        let search_path = std::env::join_paths([first.path(), second.path()]).unwrap();

        let resolved = resolve_ffmpeg_path_from(None, Some(search_path), &[]);

        assert_eq!(resolved, ffmpeg);
    }

    #[test]
    fn finds_ffmpeg_in_platform_locations_when_path_does_not_contain_it() {
        let directory = tempfile::tempdir().unwrap();
        let ffmpeg = directory.path().join("ffmpeg");
        std::fs::write(&ffmpeg, b"test executable").unwrap();
        make_executable(&ffmpeg);

        let resolved = resolve_ffmpeg_path_from(None, None, std::slice::from_ref(&ffmpeg));

        assert_eq!(resolved, ffmpeg);
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = path.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &std::path::Path) {}
}
