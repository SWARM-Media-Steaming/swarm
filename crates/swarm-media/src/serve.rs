//! The peer-facing media service: maps `PeerRequest`s onto the library and
//! the filesystem, and the QUIC accept loop that runs it.
//!
//! Path safety follows the Drone discipline: entry keys are validated as
//! lowercase hex *before* any lookup, and the file path served always comes
//! from the library row (derived from the scanned relative path under the
//! media root) — never from request input.

use crate::artwork_cache::{ArtworkCacheEventKind, ArtworkCacheMonitor, ArtworkCacheSnapshot};
use crate::bandwidth::BandwidthMeter;
use crate::range::{content_type, resolve, ResolvedRange};
use crate::recommend::{
    recommend, DiscoveryMode, EraPreference, KindPreference, LibraryItem, Mood, ScoringWeights,
};
use crate::roots::{RootResolver, SharedRootResolver};
use crate::store::{ArtworkKind, Library};
use crate::transcode::{
    hls_content_type, SessionRateLimiter, TranscodeConfig, TranscodeError, TranscodeManager,
};
use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::stream::{self, Stream};
use std::collections::{HashMap, VecDeque};
use std::io::BufWriter;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use swarm_core::entry_key::is_valid_entry_key;
use swarm_core::peer::{
    BuzzChoice, BuzzRequest, BuzzResponse, CatalogEntry, CatalogManifest, CatalogThumbprint,
    PeerRequest, PeerResponseHeader, PlaybackPlan, SubtitleTrack,
};
use swarm_p2p::endpoint::{read_request, write_response_header, P2pError};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub struct MediaService {
    library: Arc<Library>,
    roots: SharedRootResolver,
    transcodes: Arc<TranscodeManager>,
    thumbnail_generation: tokio::sync::Mutex<()>,
    artwork_cache_dir: Option<PathBuf>,
    artwork_cache_enabled: AtomicBool,
    artwork_cache_fills: [tokio::sync::Mutex<()>; 32],
    artwork_cache_monitor: ArtworkCacheMonitor,
    client_names: std::sync::RwLock<HashMap<String, String>>,
    catalog_snapshots: std::sync::Mutex<VecDeque<CatalogSnapshot>>,
    bandwidth: Arc<BandwidthMeter>,
}

#[derive(Clone)]
struct CatalogSnapshot {
    thumbprint: String,
    entries: Vec<CatalogEntry>,
}

const CATALOG_SNAPSHOT_HISTORY: usize = 16;
const CATALOG_CHANGE_WAIT: Duration = Duration::from_secs(20);
/// Re-check interval while a `/catalog/changes` request is parked. Each
/// check rebuilds the whole catalog snapshot (`Library::catalog_snapshot`
/// hashes every entry), so this is deliberately not sub-second: one browsing
/// TV would otherwise pin a snapshot rebuild loop on the server for the full
/// wait window, multiplied by every connected client. One second keeps the
/// "updates show up on their own within a second or two" feel the feed is
/// for without that cost.
const CATALOG_CHANGE_POLL: Duration = Duration::from_secs(1);

const ARTWORK_CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug)]
enum BuzzError {
    BadRequest,
    NotFound,
    Database,
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn seed_from(value: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn next_question(answers: &crate::recommend::SessionAnswers) -> Option<&'static str> {
    ["mood", "kind", "era"]
        .into_iter()
        .find(|id| !answers.answered_questions.iter().any(|seen| seen == id))
}

fn apply_answer(
    answers: &mut crate::recommend::SessionAnswers,
    question: &str,
    value: &str,
) -> Result<(), BuzzError> {
    match question {
        "mood" => match value {
            "funny" => answers.moods = vec![Mood::Funny],
            "weird" => answers.moods = vec![Mood::Weird],
            "action" => answers.moods = vec![Mood::Action],
            "scary" => answers.moods = vec![Mood::Scary],
            "surprise" => answers.mode = DiscoveryMode::SurpriseMe,
            _ => return Err(BuzzError::BadRequest),
        },
        "kind" => {
            answers.kind = match value {
                "movie" => KindPreference::Movie,
                "show" => KindPreference::Show,
                "dont_care" => KindPreference::DontCare,
                _ => return Err(BuzzError::BadRequest),
            }
        }
        "era" => {
            answers.era = match value {
                "older" => EraPreference::Older,
                "newer" => EraPreference::Newer,
                "dont_care" => EraPreference::DontCare,
                _ => return Err(BuzzError::BadRequest),
            }
        }
        _ => return Err(BuzzError::BadRequest),
    }
    answers.answered_questions.push(question.to_owned());
    Ok(())
}

fn voice_asset(text: &str) -> Option<&'static str> {
    match text {
        "Let's find you something." => Some("buzz/lets_find_you_something_01.opus"),
        "Movie or show?" => Some("buzz/movie_or_show_01.opus"),
        "I think I've got one." => Some("buzz/i_think_ive_got_one_01.opus"),
        _ => None,
    }
}

fn question_response(
    session_id: &str,
    answers: &crate::recommend::SessionAnswers,
    history: &crate::recommend::DeviceHistory,
) -> BuzzResponse {
    let question = next_question(answers).unwrap_or("mood");
    let (text, choices) = match question {
        "mood" => (
            "What kind of night is this?",
            vec![
                ("funny", "Make me laugh"),
                ("weird", "Something weird"),
                ("action", "Action"),
                ("scary", "Scare me"),
                ("surprise", "You decide"),
            ],
        ),
        "kind" => {
            let movies = history
                .kind_affinity
                .get(&swarm_core::peer::MediaKind::Movie)
                .copied()
                .unwrap_or(0.0);
            let shows = history
                .kind_affinity
                .get(&swarm_core::peer::MediaKind::Episode)
                .copied()
                .unwrap_or(0.0);
            (
                if movies > shows && movies > 1.0 {
                    "Movie again?"
                } else {
                    "Movie or show?"
                },
                vec![
                    ("movie", "Movie"),
                    ("show", "Show"),
                    ("dont_care", "Don't care"),
                ],
            )
        }
        _ => (
            "Older or newer?",
            vec![
                ("older", "Older"),
                ("newer", "Newer"),
                ("dont_care", "Don't care"),
            ],
        ),
    };
    BuzzResponse {
        session_id: session_id.into(),
        screen: "question".into(),
        buzz_text: text.into(),
        voice_asset: voice_asset(text).map(str::to_owned),
        choices: choices
            .into_iter()
            .map(|(id, label)| BuzzChoice {
                id: id.into(),
                label: label.into(),
            })
            .collect(),
        media_id: None,
        title: None,
        reasons: vec![],
        actions: vec![],
    }
}

/// A resolved response: header plus a body source the transport streams out.
pub enum Body {
    Bytes(Vec<u8>),
    File {
        path: PathBuf,
        offset: u64,
        len: u64,
        rate_limiters: Vec<Arc<SessionRateLimiter>>,
    },
}

pub struct Resolved {
    pub header: PeerResponseHeader,
    pub body: Body,
    /// Playback session held in-use until `handle_stream` finishes writing.
    session_id: Option<String>,
}

fn status(status: u16) -> Resolved {
    Resolved {
        header: PeerResponseHeader {
            status,
            len: 0,
            content_type: None,
            content_range: None,
            etag: None,
        },
        body: Body::Bytes(Vec::new()),
        session_id: None,
    }
}

fn json_response(status: u16, value: &impl serde::Serialize) -> Resolved {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    Resolved {
        header: PeerResponseHeader {
            status,
            len: bytes.len() as u64,
            content_type: Some("application/json".into()),
            content_range: None,
            etag: None,
        },
        body: Body::Bytes(bytes),
        session_id: None,
    }
}

fn gzip_json_response(status: u16, value: &impl serde::Serialize) -> Resolved {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    let bytes = serde_json::to_writer(&mut encoder, value)
        .and_then(|_| encoder.finish().map_err(serde_json::Error::io))
        .unwrap_or_default();
    Resolved {
        header: PeerResponseHeader {
            status,
            len: bytes.len() as u64,
            content_type: Some("application/gzip".into()),
            content_range: None,
            etag: None,
        },
        body: Body::Bytes(bytes),
        session_id: None,
    }
}

fn catalog_delta(
    previous: CatalogSnapshot,
    thumbprint: String,
    entries: Vec<CatalogEntry>,
) -> CatalogManifest {
    let old_by_key: HashMap<&str, &CatalogEntry> = previous
        .entries
        .iter()
        .map(|entry| (entry.entry_key.as_str(), entry))
        .collect();
    let new_keys: std::collections::HashSet<String> = entries
        .iter()
        .map(|entry| entry.entry_key.clone())
        .collect();
    let changed = entries
        .into_iter()
        .filter(|entry| old_by_key.get(entry.entry_key.as_str()).copied() != Some(entry))
        .collect();
    let removed = previous
        .entries
        .into_iter()
        .filter(|entry| !new_keys.contains(entry.entry_key.as_str()))
        .map(|entry| entry.entry_key)
        .collect();
    CatalogManifest {
        thumbprint,
        entries: changed,
        removed,
        reset: false,
    }
}

impl MediaService {
    /// Exposes the underlying library for host-local automation hooks (see
    /// `apps/server/src/http_media.rs`'s debug-build-only resolve route,
    /// used by the closed-loop TV UAT suite) — not used by any peer-facing
    /// request path, which always goes through `resolve_for_network`/
    /// `resolve_for_client` instead.
    pub fn library(&self) -> &Arc<Library> {
        &self.library
    }

    pub fn new(library: Arc<Library>, media_root: PathBuf) -> Self {
        let config = TranscodeConfig::disabled(std::env::temp_dir().join("swarm-hls-disabled"));
        Self::with_transcoding(library, media_root, config)
    }

    pub fn with_transcoding(
        library: Arc<Library>,
        media_root: PathBuf,
        config: TranscodeConfig,
    ) -> Self {
        Self::with_roots(
            library,
            SharedRootResolver::new(RootResolver::single(media_root)),
            config,
        )
    }

    /// Multi-root variant of [`Self::with_transcoding`] — see `crate::roots`.
    /// Takes a [`SharedRootResolver`] (not a bare [`RootResolver`]) so a
    /// caller that later live-updates its roots (see
    /// `ServerCore::update_media_roots`) can share the exact same handle
    /// with this service — a bare `RootResolver` clone would silently drift
    /// out of sync on the next update.
    pub fn with_roots(
        library: Arc<Library>,
        roots: SharedRootResolver,
        config: TranscodeConfig,
    ) -> Self {
        Self::with_optional_artwork_cache(library, roots, config, None)
    }

    /// Construct a service whose optional artwork cache lives on the media
    /// server's local disk. Caching remains off until
    /// [`Self::set_artwork_disk_cache_enabled`] is called, matching the
    /// persisted desktop preference's opt-in behavior.
    pub fn with_roots_and_artwork_cache(
        library: Arc<Library>,
        roots: SharedRootResolver,
        config: TranscodeConfig,
        artwork_cache_dir: PathBuf,
    ) -> Self {
        Self::with_optional_artwork_cache(library, roots, config, Some(artwork_cache_dir))
    }

    fn with_optional_artwork_cache(
        library: Arc<Library>,
        roots: SharedRootResolver,
        config: TranscodeConfig,
        artwork_cache_dir: Option<PathBuf>,
    ) -> Self {
        let artwork_cache_monitor = ArtworkCacheMonitor::new(artwork_cache_dir.clone());
        Self {
            library,
            roots,
            transcodes: TranscodeManager::new(config),
            thumbnail_generation: tokio::sync::Mutex::new(()),
            artwork_cache_dir,
            artwork_cache_enabled: AtomicBool::new(false),
            artwork_cache_fills: std::array::from_fn(|_| tokio::sync::Mutex::new(())),
            artwork_cache_monitor,
            client_names: std::sync::RwLock::new(HashMap::new()),
            catalog_snapshots: std::sync::Mutex::new(VecDeque::new()),
            bandwidth: BandwidthMeter::new(),
        }
    }

    pub fn set_artwork_disk_cache_enabled(&self, enabled: bool) {
        self.artwork_cache_enabled.store(enabled, Ordering::Relaxed);
    }

    pub async fn artwork_cache_snapshot(&self) -> ArtworkCacheSnapshot {
        self.artwork_cache_monitor
            .snapshot(self.artwork_cache_enabled.load(Ordering::Relaxed))
            .await
    }

    pub fn replace_client_names(&self, clients: impl IntoIterator<Item = (String, String)>) {
        *self.client_names.write().unwrap() = clients.into_iter().collect();
    }

    pub fn set_client_name(&self, fingerprint: String, name: String) {
        self.client_names.write().unwrap().insert(fingerprint, name);
    }

    fn client_name(&self, fingerprint: &str) -> Option<String> {
        self.client_names.read().unwrap().get(fingerprint).cloned()
    }

    pub fn transcode_manager(&self) -> &Arc<TranscodeManager> {
        &self.transcodes
    }

    pub fn bandwidth_meter(&self) -> &Arc<BandwidthMeter> {
        &self.bandwidth
    }

    pub async fn resolve(&self, request: &PeerRequest) -> Resolved {
        self.resolve_for_transport(request, false, "Local request", None)
            .await
    }

    /// Resolve a request with transport context. `is_lan` bypasses upload
    /// admission limits and pacing because a local transfer does not spend
    /// the internet uplink the budget is meant to protect.
    pub async fn resolve_for_network(&self, request: &PeerRequest, is_lan: bool) -> Resolved {
        self.resolve_for_transport(request, is_lan, "Unknown client", None)
            .await
    }

    /// Resolve with a dashboard-facing client label. Transports should use a
    /// paired device name when available and a network address otherwise.
    pub async fn resolve_for_client(
        &self,
        request: &PeerRequest,
        is_lan: bool,
        client: &str,
    ) -> Resolved {
        self.resolve_for_transport(request, is_lan, client, None)
            .await
    }

    /// Resolve a request for an authenticated transport peer. The stable
    /// owner identity lets a repeated playback negotiation supersede only
    /// that peer's unclaimed reservation.
    pub async fn resolve_for_peer(
        &self,
        request: &PeerRequest,
        is_lan: bool,
        client: &str,
        playback_owner: &str,
    ) -> Resolved {
        self.resolve_for_transport(request, is_lan, client, Some(playback_owner))
            .await
    }

    /// Resolve with separate display and reservation identities. QUIC peers
    /// use their certificate fingerprint as `playback_owner`, so reconnecting
    /// with a different address or sharing a friendly device name cannot
    /// strand or cross-cancel another TV's unclaimed playback reservation.
    async fn resolve_for_transport(
        &self,
        request: &PeerRequest,
        is_lan: bool,
        client: &str,
        playback_owner: Option<&str>,
    ) -> Resolved {
        let (request_path, query) = request
            .path
            .split_once('?')
            .map_or((request.path.as_str(), ""), |(path, query)| (path, query));
        match request_path {
            "/catalog/thumbprint" => self.thumbprint().await,
            "/catalog/manifest" => self.manifest(false).await,
            "/catalog/manifest.gz" => self.manifest(true).await,
            "/catalog/changes" => self.catalog_changes(query, false).await,
            "/catalog/changes.gz" => self.catalog_changes(query, true).await,
            "/errors/report" => self.report_error(request).await,
            "/likes/toggle" => self.set_like(request, playback_owner).await,
            "/buzz" => self.buzz(query, playback_owner).await,
            path => {
                if let Some(rest) = path.strip_prefix("/notifications/") {
                    self.client_notifications(rest).await
                } else if let Some(entry_key) = path.strip_prefix("/media/") {
                    self.media(entry_key, request, is_lan).await
                } else if let Some(entry_key) = path.strip_prefix("/play/") {
                    self.play(entry_key, request, is_lan, playback_owner).await
                } else if let Some(rest) = path.strip_prefix("/stream/") {
                    self.session_media(rest, request, is_lan).await
                } else if let Some(rest) = path.strip_prefix("/hls/") {
                    self.hls(rest, request, is_lan).await
                } else if let Some(session_id) = path.strip_prefix("/stop/") {
                    self.stop(session_id).await
                } else if let Some(rest) = path.strip_prefix("/subtitles/") {
                    self.subtitle(rest).await
                } else if let Some(rest) = path.strip_prefix("/art/") {
                    let mut segments = rest.splitn(2, '/');
                    let entry_key = segments.next().unwrap_or("");
                    let kind = segments.next().unwrap_or("");
                    self.art(entry_key, kind, request, client).await
                } else {
                    status(404)
                }
            }
        }
    }

    async fn buzz(&self, query: &str, device_id: Option<&str>) -> Resolved {
        let Some(device_id) = device_id else {
            return status(401);
        };
        let Some(payload) = query
            .split('&')
            .find_map(|part| part.strip_prefix("payload="))
        else {
            return status(400);
        };
        let Ok(bytes) = hex::decode(payload) else {
            return status(400);
        };
        let Ok(request) = serde_json::from_slice::<BuzzRequest>(&bytes) else {
            return status(400);
        };
        match self.buzz_transition(device_id, &request).await {
            Ok(Some(response)) => json_response(200, &response),
            Ok(None) => status(204),
            Err(BuzzError::BadRequest) => status(400),
            Err(BuzzError::NotFound) => status(404),
            Err(BuzzError::Database) => status(500),
        }
    }

    async fn buzz_transition(
        &self,
        device_id: &str,
        request: &BuzzRequest,
    ) -> Result<Option<BuzzResponse>, BuzzError> {
        if request.action == "start" {
            let mode = match request.value.as_deref().unwrap_or("find_me_something") {
                "find_me_something" => DiscoveryMode::FindMeSomething,
                "surprise_me" => DiscoveryMode::SurpriseMe,
                "buzz_knows_best" => DiscoveryMode::BuzzKnowsBest,
                _ => return Err(BuzzError::BadRequest),
            };
            let id = hex::encode(rand::random::<[u8; 16]>());
            let answers = crate::recommend::SessionAnswers {
                mode,
                session_seed: seed_from(&id),
                ..Default::default()
            };
            self.library
                .create_buzz_session(
                    &id,
                    device_id,
                    request.profile_id.as_deref(),
                    mode,
                    &answers,
                )
                .await
                .map_err(|_| BuzzError::Database)?;
            if mode != DiscoveryMode::FindMeSomething {
                return self
                    .buzz_recommend(device_id, request.profile_id.as_deref(), &id, &answers)
                    .await
                    .map(Some);
            }
            let history = self
                .library
                .buzz_history(device_id, request.profile_id.as_deref())
                .await
                .map_err(|_| BuzzError::Database)?;
            let response = question_response(&id, &answers, &history);
            self.library
                .record_buzz_event(
                    &id,
                    "question_shown",
                    next_question(&answers),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .map_err(|_| BuzzError::Database)?;
            return Ok(Some(response));
        }

        let session_id = request.session_id.as_deref().ok_or(BuzzError::BadRequest)?;
        let mut session = self
            .library
            .buzz_session(session_id, device_id)
            .await
            .map_err(|_| BuzzError::Database)?
            .ok_or(BuzzError::NotFound)?;
        match request.action.as_str() {
            "answer" => {
                let value = request.value.as_deref().ok_or(BuzzError::BadRequest)?;
                let question = next_question(&session.answers).ok_or(BuzzError::BadRequest)?;
                apply_answer(&mut session.answers, question, value)?;
                self.library
                    .save_buzz_answers(session_id, &session.answers, question, value)
                    .await
                    .map_err(|_| BuzzError::Database)?;
                if next_question(&session.answers).is_some() {
                    let history = self
                        .library
                        .buzz_history(device_id, session.profile_id.as_deref())
                        .await
                        .map_err(|_| BuzzError::Database)?;
                    let response = question_response(session_id, &session.answers, &history);
                    self.library
                        .record_buzz_event(
                            session_id,
                            "question_shown",
                            next_question(&session.answers),
                            None,
                            None,
                            None,
                            None,
                            None,
                        )
                        .await
                        .map_err(|_| BuzzError::Database)?;
                    Ok(Some(response))
                } else {
                    self.buzz_recommend(
                        device_id,
                        session.profile_id.as_deref(),
                        session_id,
                        &session.answers,
                    )
                    .await
                    .map(Some)
                }
            }
            "try_again" | "not_interested" | "play" | "playback_outcome" => {
                let media_id = request.media_id.as_deref().ok_or(BuzzError::BadRequest)?;
                self.library
                    .record_buzz_event(
                        session_id,
                        &request.action,
                        None,
                        request.value.as_deref(),
                        None,
                        Some(media_id),
                        request
                            .value
                            .as_deref()
                            .filter(|_| request.action == "playback_outcome"),
                        request
                            .value
                            .as_deref()
                            .filter(|_| request.action == "not_interested"),
                    )
                    .await
                    .map_err(|_| BuzzError::Database)?;
                if request.action == "try_again" || request.action == "not_interested" {
                    self.buzz_recommend(
                        device_id,
                        session.profile_id.as_deref(),
                        session_id,
                        &session.answers,
                    )
                    .await
                    .map(Some)
                } else {
                    Ok(None)
                }
            }
            _ => Err(BuzzError::BadRequest),
        }
    }

    async fn buzz_recommend(
        &self,
        device_id: &str,
        profile_id: Option<&str>,
        session_id: &str,
        answers: &crate::recommend::SessionAnswers,
    ) -> Result<BuzzResponse, BuzzError> {
        let (_, entries) = self
            .library
            .catalog_snapshot()
            .await
            .map_err(|_| BuzzError::Database)?;
        let history = self
            .library
            .buzz_history(device_id, profile_id)
            .await
            .map_err(|_| BuzzError::Database)?;
        let items = entries
            .iter()
            .filter(|e| {
                e.kind != swarm_core::peer::MediaKind::Track && e.parent_entry_key.is_none()
            })
            .map(LibraryItem::from_catalog)
            .collect::<Vec<_>>();
        let picks = recommend(
            &items,
            answers,
            &history,
            &ScoringWeights::default(),
            unix_seconds(),
            10,
        );
        let Some(pick) = picks.first() else {
            return Ok(BuzzResponse {
                session_id: session_id.into(),
                screen: "empty".into(),
                buzz_text: "I couldn't find a match in this library.".into(),
                voice_asset: None,
                choices: vec![],
                media_id: None,
                title: None,
                reasons: vec![],
                actions: vec!["back".into()],
            });
        };
        let picks_json = serde_json::to_string(&picks).ok();
        self.library
            .record_buzz_event(
                session_id,
                "recommendation_shown",
                None,
                None,
                picks_json.as_deref(),
                Some(&pick.media_id),
                None,
                None,
            )
            .await
            .map_err(|_| BuzzError::Database)?;
        Ok(BuzzResponse {
            session_id: session_id.into(),
            screen: "recommendation".into(),
            buzz_text: "I think I've got one.".into(),
            voice_asset: voice_asset("I think I've got one.").map(str::to_owned),
            choices: vec![],
            media_id: Some(pick.media_id.clone()),
            title: Some(pick.title.clone()),
            reasons: pick.reasons.clone(),
            actions: vec!["play".into(), "try_again".into(), "not_interested".into()],
        })
    }

    async fn thumbprint(&self) -> Resolved {
        match self.library.catalog_snapshot().await {
            Ok((thumbprint, entries)) => json_response(
                200,
                &CatalogThumbprint {
                    thumbprint,
                    entry_count: entries.len() as u64,
                },
            ),
            _ => status(500),
        }
    }

    async fn manifest(&self, compressed: bool) -> Resolved {
        let Ok((thumbprint, entries)) = self.library.catalog_snapshot().await else {
            return status(500);
        };
        self.remember_catalog_snapshot(&thumbprint, &entries);
        let manifest = CatalogManifest {
            thumbprint,
            entries,
            removed: Vec::new(),
            reset: false,
        };
        if compressed {
            gzip_json_response(200, &manifest)
        } else {
            json_response(200, &manifest)
        }
    }

    /// Long-held catalog request used as the server-to-TV change feed. The
    /// response is empty after a quiet timeout, a compact delta when the
    /// caller's snapshot is still in bounded history, or a reset snapshot
    /// after a server restart/very stale client.
    async fn catalog_changes(&self, query: &str, compressed: bool) -> Resolved {
        let Some(since) = query
            .split('&')
            .find_map(|part| part.strip_prefix("since="))
            .filter(|value| !value.is_empty())
        else {
            return status(400);
        };
        let deadline = tokio::time::Instant::now() + CATALOG_CHANGE_WAIT;
        loop {
            let Ok((thumbprint, entries)) = self.library.catalog_snapshot().await else {
                return status(500);
            };
            if thumbprint != since {
                let previous = self.catalog_snapshot(since);
                self.remember_catalog_snapshot(&thumbprint, &entries);
                let manifest = match previous {
                    Some(previous) => catalog_delta(previous, thumbprint, entries),
                    None => CatalogManifest {
                        thumbprint,
                        entries,
                        removed: Vec::new(),
                        reset: true,
                    },
                };
                return if compressed {
                    gzip_json_response(200, &manifest)
                } else {
                    json_response(200, &manifest)
                };
            }
            self.remember_catalog_snapshot(&thumbprint, &entries);
            if tokio::time::Instant::now() >= deadline {
                return status(204);
            }
            tokio::time::sleep(CATALOG_CHANGE_POLL).await;
        }
    }

    fn catalog_snapshot(&self, thumbprint: &str) -> Option<CatalogSnapshot> {
        self.catalog_snapshots
            .lock()
            .unwrap()
            .iter()
            .find(|snapshot| snapshot.thumbprint == thumbprint)
            .cloned()
    }

    fn remember_catalog_snapshot(&self, thumbprint: &str, entries: &[CatalogEntry]) {
        let mut snapshots = self.catalog_snapshots.lock().unwrap();
        if snapshots
            .iter()
            .any(|snapshot| snapshot.thumbprint == thumbprint)
        {
            return;
        }
        snapshots.push_back(CatalogSnapshot {
            thumbprint: thumbprint.to_owned(),
            entries: entries.to_vec(),
        });
        while snapshots.len() > CATALOG_SNAPSHOT_HISTORY {
            snapshots.pop_front();
        }
    }

    /// `/errors/report` — a client persists a [`swarm_core::peer::ClientErrorReport`]
    /// here for later triage on this server's own swarm page, rather than it
    /// only ever existing in on-device logs nobody's looking at.
    async fn report_error(&self, request: &PeerRequest) -> Resolved {
        let Some(report) = &request.error_report else {
            return status(400);
        };
        if report.device_id.is_empty() || report.message.is_empty() {
            return status(400);
        }
        match self.library.record_client_error(report).await {
            Ok(()) => status(204),
            Err(_) => status(500),
        }
    }

    /// Resolved-problem inbox for one client. A list request is
    /// `/notifications/{device_id}`; dismissal is
    /// `/notifications/{device_id}/{error_id}/dismiss`. These ride the same
    /// authenticated peer plane as reporting the original problem.
    async fn client_notifications(&self, rest: &str) -> Resolved {
        let parts = rest.split('/').collect::<Vec<_>>();
        if parts.len() == 1 && !parts[0].is_empty() {
            return match self
                .library
                .list_client_resolution_notifications(parts[0])
                .await
            {
                Ok(notifications) => json_response(200, &notifications),
                Err(_) => status(500),
            };
        }
        if parts.len() == 3 && parts[2] == "dismiss" && !parts[0].is_empty() {
            let Ok(id) = parts[1].parse::<i64>() else {
                return status(400);
            };
            return match self
                .library
                .dismiss_client_resolution_notification(parts[0], id)
                .await
            {
                Ok(true) => status(204),
                Ok(false) => status(404),
                Err(_) => status(500),
            };
        }
        status(404)
    }

    /// `/likes/toggle` — see [`swarm_core::peer::LikeToggle`]'s doc comment
    /// for the idempotent-desired-end-state semantics.
    async fn set_like(
        &self,
        request: &PeerRequest,
        authenticated_device_id: Option<&str>,
    ) -> Resolved {
        let Some(like) = &request.like else {
            return status(400);
        };
        if like.device_id.is_empty() || like.entry_key.is_empty() {
            return status(400);
        }
        match self
            .library
            .set_like(
                &like.entry_key,
                authenticated_device_id.unwrap_or(&like.device_id),
                &like.device_name,
                like.liked,
            )
            .await
        {
            Ok(()) => status(204),
            Err(_) => status(500),
        }
    }

    /// A client hitting a catalog entry whose backing file is gone (renamed
    /// or deleted since the last scan) means the periodic library watch
    /// hasn't caught up yet — flip the row unavailable right now instead of
    /// leaving every subsequent request against the same stale entry to
    /// fail the same way until the next scheduled rescan (up to
    /// `AUTO_LIBRARY_WATCH_INTERVAL`, 15 minutes, or a manual rescan) runs.
    /// Uses the exact same first-miss-flips-`available`-immediately policy
    /// `scan::scan_roots_scoped_inner` already applies, so this only ever
    /// makes the existing reconciliation fire sooner, never differently.
    /// Best-effort: a DB error here just leaves the row as it was, same as
    /// before this existed.
    async fn mark_entry_missing(&self, relative_path: &str) {
        if let Err(error) = self
            .library
            .mark_missing_by_path(relative_path, crate::scan::MISSING_CONFIRMATION_GRACE_MS)
            .await
        {
            tracing::warn!(%error, "could not mark streaming-time-missing entry unavailable");
        }
    }

    async fn media(&self, entry_key: &str, request: &PeerRequest, is_lan: bool) -> Resolved {
        if !is_valid_entry_key(entry_key) {
            return status(404);
        }
        let Ok(Some(entry)) = self.library.get(entry_key).await else {
            return status(404);
        };
        self.media_entry(entry, request, None, self.rate_limiters(is_lan, None))
            .await
    }

    async fn media_entry(
        &self,
        entry: crate::store::EntryRecord,
        request: &PeerRequest,
        session_id: Option<String>,
        rate_limiters: Vec<Arc<SessionRateLimiter>>,
    ) -> Resolved {
        let path = self.roots.resolve(&entry.relative_path);
        let Ok(metadata) = std::fs::metadata(&path) else {
            self.mark_entry_missing(&entry.relative_path).await;
            return status(404); // deleted since last scan
        };
        let total = metadata.len();
        match resolve(request.range, total) {
            ResolvedRange::Full { len } => Resolved {
                header: PeerResponseHeader {
                    status: 200,
                    len,
                    content_type: Some(content_type(&entry.relative_path).into()),
                    content_range: None,
                    etag: Some(entry.fingerprint.clone()),
                },
                body: Body::File {
                    path,
                    offset: 0,
                    len,
                    rate_limiters,
                },
                session_id,
            },
            ResolvedRange::Partial(content_range) => {
                let len = content_range.end - content_range.start + 1;
                Resolved {
                    header: PeerResponseHeader {
                        status: 206,
                        len,
                        content_type: Some(content_type(&entry.relative_path).into()),
                        content_range: Some(content_range),
                        etag: Some(entry.fingerprint.clone()),
                    },
                    body: Body::File {
                        path,
                        offset: content_range.start,
                        len,
                        rate_limiters,
                    },
                    session_id,
                }
            }
            ResolvedRange::Unsatisfiable => status(416),
        }
    }

    async fn play(
        &self,
        entry_key: &str,
        request: &PeerRequest,
        is_lan: bool,
        playback_owner: Option<&str>,
    ) -> Resolved {
        if !is_valid_entry_key(entry_key) {
            return status(404);
        }
        let Some(preferences) = request.playback.as_ref() else {
            return transcode_error(TranscodeError::MissingPreferences);
        };
        let Ok(Some(entry)) = self.library.get(entry_key).await else {
            return status(404);
        };
        let media_path = self.roots.resolve(&entry.relative_path);
        if !media_path.is_file() {
            self.mark_entry_missing(&entry.relative_path).await;
            return status(404);
        }
        match self
            .transcodes
            .plan(&entry, &media_path, preferences, is_lan, playback_owner)
            .await
        {
            Ok(mut plan) => {
                // Keep ownership across the post-plan metadata awaits too.
                // If the peer disappears while lyrics/subtitles are being
                // loaded, dropping `play()` must release the already-created
                // transcode just like cancellation during FFmpeg startup.
                let mut reservation = PlaybackReservationGuard::new(
                    Arc::clone(&self.transcodes),
                    Some(plan.session_id.clone()),
                );
                if entry.kind == swarm_core::peer::MediaKind::Track {
                    match self.library.track_lyrics(entry_key).await {
                        Ok(lyrics) => plan.lyrics = lyrics,
                        Err(error) => {
                            tracing::warn!(entry_key, %error, "could not load cached lyrics for playback");
                        }
                    }
                } else if !preferences.preview {
                    match self.library.subtitle_tracks(entry_key).await {
                        Ok(tracks) => {
                            plan.subtitles = tracks
                                .into_iter()
                                .filter(|track| track.fingerprint == entry.fingerprint)
                                .filter(|track| PathBuf::from(&track.file_path).is_file())
                                .map(|track| SubtitleTrack {
                                    path: format!("/subtitles/{entry_key}/{}.vtt", track.id),
                                    id: track.id,
                                    language: track.language,
                                    label: track.label,
                                    source: track.source,
                                })
                                .collect();
                        }
                        Err(error) => {
                            tracing::warn!(entry_key, %error, "could not load generated subtitles for playback");
                        }
                    }
                }
                let response = json_response(200, &plan);
                reservation.disarm();
                response
            }
            Err(error) => {
                tracing::warn!(entry_key, %error, "playback negotiation failed");
                transcode_error(error)
            }
        }
    }

    /// Explicit early release of a playback session's bandwidth reservation
    /// (player screen torn down — back-press or moving to the next entry).
    /// Idempotent and always 200, including for an id that already expired
    /// or was never valid: the client fires this best-effort on its way out
    /// and has no useful recovery if the server disagrees about whether the
    /// session still existed.
    async fn stop(&self, session_id: &str) -> Resolved {
        self.transcodes.release(session_id);
        status(200)
    }

    /// Serve a completed track previously registered in SQLite. The request
    /// never becomes a filesystem path, so this cannot traverse out of the
    /// stored `file_path`. Whisper/OpenSubtitles tracks are already WebVTT
    /// on disk; a side-loaded (`source = "external"`) `.srt` sidecar is
    /// converted to WebVTT here so the client sees one uniform format.
    async fn subtitle(&self, rest: &str) -> Resolved {
        let Some((entry_key, filename)) = rest.split_once('/') else {
            return status(404);
        };
        if !is_valid_entry_key(entry_key) {
            return status(404);
        }
        let Some(track_id) = filename.strip_suffix(".vtt") else {
            return status(404);
        };
        let Ok(Some(track)) = self.library.subtitle_track(entry_key, track_id).await else {
            return status(404);
        };
        let Ok(Some(entry)) = self.library.get(entry_key).await else {
            return status(404);
        };
        if track.fingerprint != entry.fingerprint {
            return status(404);
        }
        let bytes = match track.format.as_str() {
            "vtt" => tokio::fs::read(&track.file_path).await.ok(),
            "srt" => tokio::fs::read_to_string(&track.file_path)
                .await
                .ok()
                .map(|text| crate::subtitles::srt_to_webvtt(&text).into_bytes()),
            _ => None,
        };
        match bytes {
            Some(bytes) => Resolved {
                header: PeerResponseHeader {
                    status: 200,
                    len: bytes.len() as u64,
                    content_type: Some("text/vtt; charset=utf-8".into()),
                    content_range: None,
                    etag: None,
                },
                body: Body::Bytes(bytes),
                session_id: None,
            },
            None => status(404),
        }
    }

    async fn session_media(&self, rest: &str, request: &PeerRequest, is_lan: bool) -> Resolved {
        let Some((session_id, tail)) = rest.split_once('/') else {
            return status(404);
        };
        if tail != "media" {
            return status(404);
        }
        let Some((entry_key, rate_limiter)) = self.transcodes.open_direct(session_id) else {
            return status(404);
        };
        let Ok(Some(entry)) = self.library.get(&entry_key).await else {
            self.transcodes.finish_use(session_id);
            return status(404);
        };
        self.media_entry(
            entry,
            request,
            Some(session_id.to_string()),
            self.rate_limiters(is_lan, Some(rate_limiter)),
        )
        .await
    }

    async fn hls(&self, rest: &str, request: &PeerRequest, is_lan: bool) -> Resolved {
        let Some((session_id, relative_path)) = rest.split_once('/') else {
            return status(404);
        };
        let Some(file) = self.transcodes.open_hls(session_id, relative_path) else {
            return status(404);
        };
        let Ok(metadata) = std::fs::metadata(&file.path) else {
            self.transcodes.finish_use(session_id);
            return status(404);
        };
        let total = metadata.len();
        let content_type = hls_content_type(&file.path);
        match resolve(request.range, total) {
            ResolvedRange::Full { len } => Resolved {
                header: PeerResponseHeader {
                    status: 200,
                    len,
                    content_type: Some(content_type.into()),
                    content_range: None,
                    etag: None,
                },
                body: Body::File {
                    path: file.path,
                    offset: 0,
                    len,
                    rate_limiters: self.rate_limiters(is_lan, Some(file.rate_limiter)),
                },
                session_id: Some(file.session_id),
            },
            ResolvedRange::Partial(content_range) => {
                let len = content_range.end - content_range.start + 1;
                Resolved {
                    header: PeerResponseHeader {
                        status: 206,
                        len,
                        content_type: Some(content_type.into()),
                        content_range: Some(content_range),
                        etag: None,
                    },
                    body: Body::File {
                        path: file.path,
                        offset: content_range.start,
                        len,
                        rate_limiters: self.rate_limiters(is_lan, Some(file.rate_limiter)),
                    },
                    session_id: Some(file.session_id),
                }
            }
            ResolvedRange::Unsatisfiable => {
                self.transcodes.finish_use(session_id);
                status(416)
            }
        }
    }

    fn rate_limiters(
        &self,
        is_lan: bool,
        session: Option<Arc<SessionRateLimiter>>,
    ) -> Vec<Arc<SessionRateLimiter>> {
        if !self.transcodes.should_throttle(is_lan) {
            return Vec::new();
        }
        let mut limiters = vec![self.transcodes.global_rate_limiter()];
        if let Some(session) = session {
            limiters.push(session);
        }
        limiters
    }

    /// `GET /art/{entry_key}/{poster|season|backdrop|cover|artist}` — the artwork a
    /// scrape wrote, served the same way as media bytes (Range + etag), with
    /// `if_none_match` short-circuiting to 304 when the client already has
    /// the current version. `artist` falls back to the artist's first album
    /// cover when no artist photo was ever scraped (#277), so clients can
    /// treat this route as always answerable without their own fallback.
    async fn art(
        &self,
        entry_key: &str,
        kind_segment: &str,
        request: &PeerRequest,
        client: &str,
    ) -> Resolved {
        if !is_valid_entry_key(entry_key) {
            return status(404);
        }
        let Some(kind) = ArtworkKind::parse(kind_segment) else {
            return status(404);
        };
        let lookup = if kind == ArtworkKind::ArtistPhoto {
            self.library.artist_photo_or_fallback(entry_key).await
        } else {
            self.library.artwork(entry_key, kind).await
        };
        let Ok(Some((relative_path, version))) = lookup else {
            return status(404);
        };
        let requested_width = artwork_thumbnail_width(&request.path);
        let etag = requested_width.map_or_else(
            || format!("v{version}"),
            |width| format!("v{version}-w{width}"),
        );
        if request.if_none_match.as_deref() == Some(etag.as_str()) {
            return Resolved {
                header: PeerResponseHeader {
                    status: 304,
                    len: 0,
                    content_type: None,
                    content_range: None,
                    etag: Some(etag),
                },
                body: Body::Bytes(Vec::new()),
                session_id: None,
            };
        }
        let source_path = self.roots.resolve(&relative_path);
        let source_path = self
            .cached_artwork_path(
                &source_path,
                entry_key,
                kind.route_segment(),
                version,
                client,
            )
            .await
            .unwrap_or(source_path);
        let path = match requested_width {
            Some(width) => self
                .thumbnail_path(
                    &source_path,
                    entry_key,
                    kind.route_segment(),
                    version,
                    width,
                )
                .await
                .unwrap_or(source_path),
            None => source_path,
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            return status(404); // artwork file missing from disk since the scrape
        };
        let total = metadata.len();
        match resolve(request.range, total) {
            ResolvedRange::Full { len } => Resolved {
                header: PeerResponseHeader {
                    status: 200,
                    len,
                    content_type: Some(image_content_type(path.to_string_lossy().as_ref()).into()),
                    content_range: None,
                    etag: Some(etag),
                },
                body: Body::File {
                    path,
                    offset: 0,
                    len,
                    rate_limiters: Vec::new(),
                },
                session_id: None,
            },
            ResolvedRange::Partial(content_range) => {
                let len = content_range.end - content_range.start + 1;
                Resolved {
                    header: PeerResponseHeader {
                        status: 206,
                        len,
                        content_type: Some(
                            image_content_type(path.to_string_lossy().as_ref()).into(),
                        ),
                        content_range: Some(content_range),
                        etag: Some(etag),
                    },
                    body: Body::File {
                        path,
                        offset: content_range.start,
                        len,
                        rate_limiters: Vec::new(),
                    },
                    session_id: None,
                }
            }
            ResolvedRange::Unsatisfiable => status(416),
        }
    }

    /// Resolve an artwork request through the server-local read-through
    /// cache. The library artwork version makes scrape/manual replacements
    /// immediately select a new cache key; the fixed TTL also refreshes a
    /// source file that was changed outside those managed workflows.
    async fn cached_artwork_path(
        &self,
        source: &std::path::Path,
        entry_key: &str,
        kind: &str,
        version: u32,
        client: &str,
    ) -> Option<PathBuf> {
        if !self.artwork_cache_enabled.load(Ordering::Relaxed) {
            return None;
        }
        let cache_root = self.artwork_cache_dir.as_ref()?;
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 10
                    && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            .unwrap_or("img")
            .to_ascii_lowercase();
        let shard = entry_key.get(..2).unwrap_or("00");
        let file_prefix = format!("{entry_key}-{kind}-");
        let target = cache_root
            .join(shard)
            .join(format!("{file_prefix}v{version}.{extension}"));
        if artwork_cache_file_is_fresh(&target) {
            self.artwork_cache_monitor
                .record(client, ArtworkCacheEventKind::ServedFromCache);
            return Some(target);
        }

        // Requests for the same key share a lock to avoid duplicate SMB reads;
        // unrelated misses can still fill concurrently during a large browse.
        let fill_index =
            usize::from_str_radix(shard, 16).unwrap_or(0) % self.artwork_cache_fills.len();
        let _fill = self.artwork_cache_fills[fill_index].lock().await;
        if artwork_cache_file_is_fresh(&target) {
            self.artwork_cache_monitor
                .record(client, ArtworkCacheEventKind::ServedFromCache);
            return Some(target);
        }

        let source = source.to_path_buf();
        let output = target.clone();
        let prefix = file_prefix.clone();
        let refreshed =
            tokio::task::spawn_blocking(move || fill_artwork_cache(&source, &output, &prefix))
                .await
                .ok()
                .and_then(Result::ok)
                .is_some();
        if refreshed {
            self.artwork_cache_monitor
                .record(client, ArtworkCacheEventKind::Cached);
            Some(target)
        } else if std::fs::metadata(&target).is_ok_and(|metadata| metadata.len() > 0) {
            self.artwork_cache_monitor
                .record(client, ArtworkCacheEventKind::ServedFromCache);
            Some(target)
        } else {
            None
        }
    }

    /// Build a persistent, version-keyed JPEG thumbnail beside the source
    /// artwork. Generation is serialized and performed on the blocking pool:
    /// image decode/resize/encode is CPU and filesystem work and must never
    /// occupy a Tokio request worker. Any failure falls back to the original
    /// file, so thumbnail support cannot make existing artwork unavailable.
    async fn thumbnail_path(
        &self,
        source: &std::path::Path,
        entry_key: &str,
        kind: &str,
        version: u32,
        width: u32,
    ) -> Option<PathBuf> {
        let parent = source.parent()?;
        let cache_dir = parent.join(".swarm-thumbnails");
        let file_prefix = format!("{entry_key}-{kind}-");
        let target = cache_dir.join(format!("{file_prefix}v{version}-w{width}.jpg"));
        if std::fs::metadata(&target).is_ok_and(|metadata| metadata.len() > 0) {
            return Some(target);
        }

        let _generation = self.thumbnail_generation.lock().await;
        if std::fs::metadata(&target).is_ok_and(|metadata| metadata.len() > 0) {
            return Some(target);
        }

        let source = source.to_path_buf();
        let output = target.clone();
        tokio::task::spawn_blocking(move || {
            generate_artwork_thumbnail(&source, &output, &file_prefix, width)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .map(|_| target)
    }
}

fn artwork_cache_file_is_fresh(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| {
        metadata.len() > 0
            && metadata.modified().ok().is_some_and(|modified| {
                modified
                    .elapsed()
                    .map_or(true, |age| age < ARTWORK_CACHE_TTL)
            })
    })
}

fn fill_artwork_cache(
    source: &std::path::Path,
    target: &std::path::Path,
    file_prefix: &str,
) -> std::io::Result<()> {
    let cache_dir = target.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "artwork cache target has no parent",
        )
    })?;
    std::fs::create_dir_all(cache_dir)?;
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artwork");
    let temporary = cache_dir.join(format!(".{filename}.{}.tmp", std::process::id()));
    if let Err(error) = std::fs::copy(source, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if std::fs::metadata(&temporary)?.len() == 0 {
        let _ = std::fs::remove_file(&temporary);
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "artwork source is empty",
        ));
    }
    if let Err(error) = std::fs::rename(&temporary, target) {
        // Windows does not replace an existing destination. Only remove the
        // stale file after the complete replacement has been copied locally.
        if target.exists() {
            std::fs::remove_file(target)?;
            std::fs::rename(&temporary, target)?;
        } else {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
    }

    // Artwork-version changes invalidate immediately; removing superseded
    // files prevents repeated scrapes from growing the cache indefinitely.
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(file_prefix) && path != target {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    Ok(())
}

fn artwork_thumbnail_width(path: &str) -> Option<u32> {
    let query = path.split_once('?')?.1;
    query.split('&').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        if name != "w" {
            return None;
        }
        match value.parse::<u32>().ok()? {
            320 => Some(320),
            640 => Some(640),
            _ => None,
        }
    })
}

fn generate_artwork_thumbnail(
    source: &std::path::Path,
    target: &std::path::Path,
    file_prefix: &str,
    width: u32,
) -> Result<(), image::ImageError> {
    let image = image::ImageReader::open(source)?
        .with_guessed_format()?
        .decode()?;
    let target_height = ((image.height() as u64 * width as u64) / image.width().max(1) as u64)
        .clamp(1, 1280) as u32;
    let thumbnail = image.thumbnail(width, target_height);
    let cache_dir = target.parent().ok_or_else(|| {
        image::ImageError::IoError(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "thumbnail target has no parent",
        ))
    })?;
    std::fs::create_dir_all(cache_dir)?;

    let temporary = cache_dir.join(format!(".{file_prefix}{}.tmp", std::process::id()));
    {
        let file = std::fs::File::create(&temporary)?;
        let mut writer = BufWriter::new(file);
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 82);
        encoder.encode_image(&thumbnail)?;
    }
    std::fs::rename(&temporary, target)?;

    // One current variant per entry/kind/size is enough. Removing old
    // version files keeps long-lived libraries from accumulating thumbnails
    // every time artwork is replaced.
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(file_prefix)
                && name.ends_with(&format!("-w{width}.jpg"))
                && path != target
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    Ok(())
}

fn transcode_error(error: TranscodeError) -> Resolved {
    let status_code = match &error {
        TranscodeError::Capacity | TranscodeError::Bandwidth => 429,
        TranscodeError::MissingPreferences => 400,
        _ => 503,
    };
    json_response(
        status_code,
        &serde_json::json!({ "error": error.to_string() }),
    )
}

fn image_content_type(relative_path: &str) -> &'static str {
    match relative_path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "image/jpeg", // every scraper writes .jpg today
    }
}

/// Calls [`TranscodeManager::finish_use`] exactly once when dropped — on
/// natural stream completion (the final yielded [`BodyState`] is dropped)
/// and on early drop alike, since a caller abandoning a stream mid-read (an
/// HTTP client seeking or disconnecting mid-range-request, say) is routine,
/// not exceptional, and must not leak the session either way. Kept private:
/// [`Resolved::session_id`] is private for the same reason — nothing outside
/// [`stream_body`] should be able to forget to release it.
struct SessionGuard {
    manager: Arc<TranscodeManager>,
    session_id: Option<String>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(session_id) = self.session_id.take() {
            self.manager.finish_use(&session_id);
        }
    }
}

enum BodyState {
    Bytes {
        bytes: Bytes,
        guard: SessionGuard,
    },
    FilePending {
        path: PathBuf,
        offset: u64,
        remaining: u64,
        rate_limiters: Vec<Arc<SessionRateLimiter>>,
        bandwidth: Arc<BandwidthMeter>,
        guard: SessionGuard,
    },
    FileOpen {
        file: tokio::fs::File,
        remaining: u64,
        rate_limiters: Vec<Arc<SessionRateLimiter>>,
        bandwidth: Arc<BandwidthMeter>,
        guard: SessionGuard,
    },
    Finished {
        #[allow(dead_code)]
        guard: SessionGuard,
    },
}

/// Reads at most one 64 KiB chunk from `file`, applying every rate limiter
/// and recording bandwidth exactly as the QUIC transport always has.
/// `Ok(None)` means `remaining` was already zero — the body is exhausted.
async fn read_file_chunk(
    file: &mut tokio::fs::File,
    remaining: u64,
    rate_limiters: &[Arc<SessionRateLimiter>],
    bandwidth: &Arc<BandwidthMeter>,
) -> std::io::Result<Option<Bytes>> {
    if remaining == 0 {
        return Ok(None);
    }
    let want = (64 * 1024).min(remaining as usize);
    let mut buffer = vec![0u8; want];
    let got = file.read(&mut buffer).await?;
    if got == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "file truncated while serving",
        ));
    }
    for limiter in rate_limiters {
        limiter.wait_for(got).await;
    }
    bandwidth.record(got as u64);
    buffer.truncate(got);
    Ok(Some(Bytes::from(buffer)))
}

/// Shared by both [`FilePending`](BodyState::FilePending) (after it opens
/// and seeks the file) and [`FileOpen`](BodyState::FileOpen) (every poll
/// after the first) so there is exactly one place that decides "yield a
/// chunk, keep going" vs. "done, let `guard` drop" vs. "error, then done."
async fn read_next(
    mut file: tokio::fs::File,
    remaining: u64,
    rate_limiters: Vec<Arc<SessionRateLimiter>>,
    bandwidth: Arc<BandwidthMeter>,
    guard: SessionGuard,
) -> Option<(std::io::Result<Bytes>, BodyState)> {
    match read_file_chunk(&mut file, remaining, &rate_limiters, &bandwidth).await {
        Ok(Some(bytes)) => {
            let got = bytes.len() as u64;
            Some((
                Ok(bytes),
                BodyState::FileOpen {
                    file,
                    remaining: remaining - got,
                    rate_limiters,
                    bandwidth,
                    guard,
                },
            ))
        }
        // remaining == 0: body exhausted. Returning None here — rather than
        // yielding one last empty chunk — drops `guard` (owned by this
        // match arm's consumed state) right now, which is what actually
        // releases the transcode session.
        Ok(None) => None,
        Err(err) => Some((Err(err), BodyState::Finished { guard })),
    }
}

async fn next_body_chunk(state: BodyState) -> Option<(std::io::Result<Bytes>, BodyState)> {
    match state {
        BodyState::Bytes { bytes, guard } => Some((Ok(bytes), BodyState::Finished { guard })),
        BodyState::FilePending {
            path,
            offset,
            remaining,
            rate_limiters,
            bandwidth,
            guard,
        } => {
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(err) => return Some((Err(err), BodyState::Finished { guard })),
            };
            if let Err(err) = file.seek(std::io::SeekFrom::Start(offset)).await {
                return Some((Err(err), BodyState::Finished { guard }));
            }
            read_next(file, remaining, rate_limiters, bandwidth, guard).await
        }
        BodyState::FileOpen {
            file,
            remaining,
            rate_limiters,
            bandwidth,
            guard,
        } => read_next(file, remaining, rate_limiters, bandwidth, guard).await,
        BodyState::Finished { .. } => None,
    }
}

/// Turns a [`Resolved`] into a chunked byte stream — the QUIC transport
/// ([`handle_stream`]) and any HTTP transport both consume this, so the
/// 64 KiB chunking, per-chunk rate limiting, bandwidth accounting, and
/// session-release-on-drop logic is written and tested exactly once rather
/// than reimplemented per transport (see [`SessionGuard`] for why the
/// release specifically must not depend on the stream finishing normally).
pub fn stream_body(
    resolved: Resolved,
    service: &Arc<MediaService>,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    let guard = SessionGuard {
        manager: Arc::clone(service.transcode_manager()),
        session_id: resolved.session_id,
    };
    let initial = match resolved.body {
        Body::Bytes(bytes) => BodyState::Bytes {
            bytes: Bytes::from(bytes),
            guard,
        },
        Body::File {
            path,
            offset,
            len,
            rate_limiters,
        } => BodyState::FilePending {
            path,
            offset,
            remaining: len,
            rate_limiters,
            bandwidth: Arc::clone(service.bandwidth_meter()),
            guard,
        },
    };
    stream::unfold(initial, next_body_chunk)
}

/// Serve one accepted bidi stream: read the request, resolve it, stream the
/// body out via [`stream_body`]. Takes `&Arc<MediaService>` rather than
/// `&MediaService` specifically so `stream_body`'s returned stream — which
/// must be `'static` since it can outlive this function's own stack frame if
/// ever spawned/boxed independently — can clone its own owned `Arc` instead
/// of borrowing one it doesn't have.
pub async fn handle_stream(
    service: &Arc<MediaService>,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    is_lan: bool,
    client: &str,
    playback_owner: &str,
) -> Result<(), P2pError> {
    let request = read_request(&mut recv).await?;
    // Preparing HLS can legitimately take over a minute. If the TV closes
    // the stream or crashes during that wait, stop polling FFmpeg and drop
    // `plan()` immediately. Its pending-session guard then kills the encoder
    // and removes the capacity reservation instead of leaking a child-less
    // session until the idle timeout.
    let stopped = send.stopped();
    tokio::pin!(stopped);
    let resolved = tokio::select! {
        resolved = service.resolve_for_peer(
            &request,
            is_lan,
            client,
            playback_owner,
        ) => resolved,
        _ = &mut stopped => return Ok(()),
    };

    // A successful `/play` response is the only response that allocates a
    // new session. Hold a delivery guard until QUIC confirms the peer
    // acknowledged the complete response; write failure, STOP_SENDING, or a
    // connection loss releases a reservation for which the TV never got an
    // actionable session id.
    let negotiated_session_id =
        if request.path.starts_with("/play/") && resolved.header.status == 200 {
            match &resolved.body {
                Body::Bytes(bytes) => serde_json::from_slice::<PlaybackPlan>(bytes)
                    .ok()
                    .map(|plan| plan.session_id),
                Body::File { .. } => None,
            }
        } else {
            None
        };
    let mut delivery = PlaybackReservationGuard::new(
        Arc::clone(service.transcode_manager()),
        negotiated_session_id,
    );
    write_response_header(&mut send, &resolved.header).await?;
    let mut body = std::pin::pin!(stream_body(resolved, service));
    while let Some(chunk) = futures_util::StreamExt::next(&mut body).await {
        send.write_all(&chunk?).await?;
    }
    send.finish().ok();
    if matches!(send.stopped().await, Ok(None)) {
        delivery.disarm();
    }
    Ok(())
}

struct PlaybackReservationGuard {
    manager: Arc<TranscodeManager>,
    session_id: Option<String>,
}

impl PlaybackReservationGuard {
    fn new(manager: Arc<TranscodeManager>, session_id: Option<String>) -> Self {
        Self {
            manager,
            session_id,
        }
    }

    fn disarm(&mut self) {
        self.session_id = None;
    }
}

impl Drop for PlaybackReservationGuard {
    fn drop(&mut self) {
        if let Some(session_id) = self.session_id.take() {
            self.manager.release(&session_id);
        }
    }
}

/// Serve every request stream an already-established connection sends,
/// spawning a task per stream, until the peer closes it. Split out from
/// [`accept_loop`] so a connection that arrived some other way — a punched
/// connection from `apps/server`'s `punch_connect`, say, rather than
/// `endpoint.accept()` — gets exactly the same per-stream serving behavior.
pub async fn serve_connection(connection: quinn::Connection, service: Arc<MediaService>) {
    let remote = connection.remote_address();
    let is_lan = is_lan_ip(remote.ip());
    let fingerprint = swarm_p2p::endpoint::peer_fingerprint(&connection);
    let client = fingerprint
        .as_deref()
        .and_then(|fingerprint| service.client_name(fingerprint))
        .unwrap_or_else(|| remote.ip().to_string());
    let playback_owner = fingerprint.unwrap_or_else(|| remote.to_string());
    tracing::info!(%remote, is_lan, "peer connected");
    // Loop ends when accept_bi errors, i.e. the connection closed.
    while let Ok((send, recv)) = connection.accept_bi().await {
        let service = Arc::clone(&service);
        let client = client.clone();
        let playback_owner = playback_owner.clone();
        tokio::spawn(async move {
            if let Err(err) =
                handle_stream(&service, send, recv, is_lan, &client, &playback_owner).await
            {
                tracing::debug!(error = %err, "stream failed");
            }
        });
    }
}

/// Shared LAN/private-address check — used to decide whether the shared
/// upload-bandwidth budget applies (see [`TranscodeManager::should_throttle`])
/// for QUIC peers here, and reused as-is by callers outside this crate
/// (`apps/server`'s LAN pairing and, later, its HTTP media surface) so there
/// is exactly one definition rather than an independent copy per transport.
pub fn is_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|ip| ip.is_private() || ip.is_link_local() || ip.is_loopback())
        }
    }
}

/// Accept connections (already fingerprint-gated by the TLS layer) and spawn
/// a task per request stream.
pub async fn accept_loop(endpoint: quinn::Endpoint, service: Arc<MediaService>) {
    while let Some(incoming) = endpoint.accept().await {
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(err) => {
                    tracing::debug!(error = %err, "connection handshake failed");
                    return;
                }
            };
            serve_connection(connection, service).await;
        });
    }
}

#[cfg(test)]
mod catalog_delta_tests {
    use super::{catalog_delta, CatalogSnapshot};
    use swarm_core::peer::{CatalogEntry, MediaKind};

    fn entry(key: &str, title: &str) -> CatalogEntry {
        CatalogEntry {
            entry_key: key.into(),
            fingerprint: format!("fp-{key}"),
            kind: MediaKind::Movie,
            title: title.into(),
            size: 1,
            duration_secs: None,
            show_title: None,
            season: None,
            episode: None,
            artist: None,
            album: None,
            track_number: None,
            scraped_title: None,
            episode_title: None,
            genres: Vec::new(),
            video: None,
            audio: None,
            artwork_etag: None,
            year: None,
            cast: Vec::new(),
            overview: None,
            rating: None,
            community_rating: None,
            community_rating_votes: None,
            like_count: 0,
            skip_segments: Vec::new(),
            relative_path: None,
            parent_entry_key: None,
            extra_type: None,
            extra_title: None,
            extra_relative_path: None,
            extra_category_path: None,
        }
    }

    #[test]
    fn delta_reports_only_added_changed_and_removed_entries() {
        let previous = CatalogSnapshot {
            thumbprint: "old".into(),
            entries: vec![entry("a", "A"), entry("b", "B"), entry("c", "C")],
        };
        // `a` unchanged, `b` retitled, `c` removed, `d` new.
        let current = vec![entry("a", "A"), entry("b", "B2"), entry("d", "D")];

        let delta = catalog_delta(previous, "new".into(), current);

        assert_eq!(delta.thumbprint, "new");
        assert!(!delta.reset);
        let changed: Vec<&str> = delta.entries.iter().map(|e| e.entry_key.as_str()).collect();
        assert_eq!(changed, vec!["b", "d"]);
        assert_eq!(delta.removed, vec!["c".to_string()]);
    }

    #[test]
    fn identical_snapshot_produces_empty_delta() {
        let previous = CatalogSnapshot {
            thumbprint: "old".into(),
            entries: vec![entry("a", "A"), entry("b", "B")],
        };
        let delta = catalog_delta(
            previous,
            "new".into(),
            vec![entry("a", "A"), entry("b", "B")],
        );
        assert!(delta.entries.is_empty());
        assert!(delta.removed.is_empty());
    }
}

#[cfg(test)]
mod network_tests {
    use super::is_lan_ip;

    #[test]
    fn identifies_local_and_internet_addresses() {
        assert!(is_lan_ip("192.168.1.20".parse().unwrap()));
        assert!(is_lan_ip("10.0.0.8".parse().unwrap()));
        assert!(is_lan_ip("fe80::1234".parse().unwrap()));
        assert!(is_lan_ip("fc00::1234".parse().unwrap()));
        assert!(!is_lan_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_lan_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
