//! Upload-budgeted playback planning and FFmpeg-backed HLS sessions.
//!
//! A session reserves its peak delivery rate before any URL is handed to a
//! client. The sum of reservations can never exceed the configured usable
//! upload budget, so independent players cannot each consume the full uplink.

use crate::probe::AudioStreamOption;
use crate::store::EntryRecord;
use rand::RngCore;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use swarm_core::peer::{MediaKind, PlaybackMode, PlaybackPlan, PlaybackPreferences};
use tokio::process::{Child, Command};

/// How long negotiation waits for FFmpeg's first playlist while the encoder is
/// still making visible progress (writing segments). A large adaptive-ladder
/// transcode of high-bitrate 10-bit source off a slow network share can take
/// well over a minute to flush the first segment of every rendition, and
/// killing a healthy encoder at a tight deadline turned every such playback
/// into a dead-end "Getting your stream ready…" screen (#131).
const STARTUP_HARD_CAP: Duration = Duration::from_secs(120);
/// Give up sooner when FFmpeg is running but has written nothing new for this
/// long — a wedged decoder or a media root that dropped mid-read, rather than
/// a slow-but-working transcode. Comfortably longer than a transient SMB
/// remount so a brief share blip is ridden out instead of failing playback.
const STARTUP_STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Hover previews must fail fast instead of pinning a browse card (and a
/// transcode slot) for the full playback budget.
/// The extra couple of seconds over the original 10s/5s absorbs the short
/// silent pre-roll a split mid-file seek now decodes before flushing its first
/// segment (#226).
const PREVIEW_STARTUP_HARD_CAP: Duration = Duration::from_secs(13);
const PREVIEW_STARTUP_STALL_TIMEOUT: Duration = Duration::from_secs(7);
const AUDIO_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Bound on the keyframe lookup that precedes a split mid-file seek — a slow
/// share just falls back to a plain input seek rather than holding up playback.
const KEYFRAME_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);
const PREVIEW_DURATION_SECS: u64 = 32;
const MAX_HLS_VIDEO_RENDITIONS: usize = 3;

/// Which H.264 encoder the transcoder should use. `Auto` picks the hardware
/// VideoToolbox encoder on macOS when FFmpeg advertises it and it has not
/// failed recently; the two `Force*` variants pin the choice for operators
/// working around a driver bug or a CPU-starved host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoEncoderMode {
    #[default]
    Auto,
    Hardware,
    Software,
}

impl VideoEncoderMode {
    pub fn from_str_lenient(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "hardware" | "videotoolbox" | "hw" => Self::Hardware,
            "software" | "libx264" | "sw" | "x264" => Self::Software,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hardware => "hardware",
            Self::Software => "software",
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Hardware => 1,
            Self::Software => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Hardware,
            2 => Self::Software,
            _ => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TranscodeConfig {
    pub enabled: bool,
    pub ffmpeg_path: PathBuf,
    pub session_dir: PathBuf,
    /// Total measured/configured upload before reserving bandwidth for the
    /// rest of the household.
    pub max_upload_bps: u64,
    /// Percentage excluded from streaming allocations. Clamped to 0..=90.
    pub reserve_percent: u8,
    pub max_sessions: usize,
    pub idle_timeout: Duration,
    pub segment_duration_secs: u32,
    /// Operator override for encoder selection.
    pub video_encoder_mode: VideoEncoderMode,
    /// Hard ceiling on transcode output height regardless of what the client
    /// advertises. `0` means "no server-imposed cap" (source/client-limited).
    pub max_transcode_height: u32,
}

impl TranscodeConfig {
    pub fn usable_upload_bps(&self) -> u64 {
        let reserve = self.reserve_percent.min(90) as u64;
        self.max_upload_bps.saturating_mul(100 - reserve) / 100
    }

    pub fn disabled(session_dir: PathBuf) -> Self {
        Self {
            enabled: false,
            ffmpeg_path: PathBuf::from("ffmpeg"),
            session_dir,
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 2,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            video_encoder_mode: VideoEncoderMode::Auto,
            max_transcode_height: 0,
        }
    }
}

fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// How long a VideoToolbox failure suppresses hardware encoding in `Auto`
/// mode before the next session tries hardware again. Long enough to ride out
/// a transient session-pool exhaustion, short enough that a machine that has
/// recovered is not stuck on software for the rest of the process's life.
const VIDEO_TOOLBOX_RETRY_COOLDOWN: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    #[error("playback preferences are required")]
    MissingPreferences,
    #[error(
        "transcoding is disabled and this file cannot be direct-played within the upload budget"
    )]
    Disabled,
    #[error("server transcode capacity is full")]
    Capacity,
    #[error("not enough upload bandwidth is available for the lowest rendition")]
    Bandwidth,
    #[error("the client does not advertise HLS support")]
    Unsupported,
    #[error("could not prepare transcode workspace: {0}")]
    Workspace(#[source] std::io::Error),
    #[error("could not start ffmpeg: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("ffmpeg exited before producing a playlist: {0}")]
    Ffmpeg(String),
    #[error("ffmpeg did not produce a playlist in time")]
    StartupTimeout,
    #[error("browse preview was superseded by foreground playback")]
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rendition {
    name: &'static str,
    width: u32,
    height: u32,
    average_video_bps: u64,
    peak_video_bps: u64,
    audio_bps: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoEncoder {
    VideoToolbox,
    Libx264 { threads_per_rendition: usize },
    /// Remux only — the source video track is already something the client
    /// decodes, so it is copied bit-for-bit into the HLS container and no
    /// encoder runs. Set via `encoder_override`, never picked automatically.
    Copy,
}

#[derive(Debug, Clone, Copy)]
struct HlsReservation {
    reserved_bps: u64,
    enforce_budget: bool,
    budget_exempt: bool,
}

impl Rendition {
    fn peak_total(self) -> u64 {
        self.peak_video_bps + self.audio_bps
    }
}

/// One audio track ffmpeg will transcode into its own HLS rendition, shared
/// across every video rendition via a common `agroup` — see the "6 audio
/// tracks" follow-up on #55, where mapping only the single server-picked
/// track meant there was nothing for the client to actually switch between.
/// `name` is filesystem/URL-safe (used for both the output directory and the
/// HLS `NAME` attribute) and disambiguated when more than one track shares a
/// language; `language` is the raw ffprobe tag, kept separate so duplicate
/// tracks in the same language still report that same `LANGUAGE` attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioMapEntry {
    source_map: String,
    name: String,
    language: Option<String>,
    is_default: bool,
    /// ffprobe `codec_name` for this track — drives the copy-vs-transcode
    /// decision. Empty when the probe could not name it (transcode to AAC).
    codec: String,
    /// Channel count; `0` when unknown. `> 2` is what makes a track worth
    /// keeping instead of downmixing.
    channels: u32,
}

/// How a mid-file start position is handed to FFmpeg. A plain input seek is
/// fast but rebases a re-encoded video stream and a stream-copied audio track
/// to different origins (#226); splitting it into an input seek to the previous
/// keyframe plus an output seek for the remainder trims both to the same point.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SeekPlan {
    input_secs: f64,
    output_secs: f64,
}

impl SeekPlan {
    fn input_only(secs: f64) -> Self {
        Self {
            input_secs: secs.max(0.0),
            output_secs: 0.0,
        }
    }
}

fn format_seek_secs(secs: f64) -> String {
    format!("{secs:.3}")
}

fn sanitize_audio_tag(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Builds one [AudioMapEntry] per probed audio track, falling back to
/// ffmpeg's own `0:a:0` default-track selection when the probe found nothing
/// (ffprobe missing/failed, or a container ffprobe can't read). Always
/// returns at least one entry with `is_default` set, even on that fallback.
fn audio_map_entries(options: Vec<AudioStreamOption>) -> Vec<AudioMapEntry> {
    if options.is_empty() {
        return vec![AudioMapEntry {
            source_map: "0:a:0".to_string(),
            name: "und".to_string(),
            language: None,
            is_default: true,
            codec: String::new(),
            channels: 0,
        }];
    }
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut entries: Vec<AudioMapEntry> = options
        .into_iter()
        .map(|option| {
            let base = option
                .language
                .as_deref()
                .and_then(sanitize_audio_tag)
                .unwrap_or_else(|| "und".to_string());
            let count = seen.entry(base.clone()).or_insert(0);
            let name = if *count == 0 {
                base.clone()
            } else {
                format!("{base}{count}")
            };
            *count += 1;
            AudioMapEntry {
                source_map: format!("0:{}", option.index),
                name,
                language: option.language,
                is_default: option.is_preferred,
                codec: option.codec,
                channels: option.channels,
            }
        })
        .collect();
    if !entries.iter().any(|entry| entry.is_default) {
        if let Some(first) = entries.first_mut() {
            first.is_default = true;
        }
    }
    entries
}

const LADDER: [Rendition; 4] = [
    Rendition {
        name: "1080p",
        width: 1920,
        height: 1080,
        average_video_bps: 6_000_000,
        peak_video_bps: 8_000_000,
        audio_bps: 192_000,
    },
    Rendition {
        name: "720p",
        width: 1280,
        height: 720,
        average_video_bps: 3_000_000,
        peak_video_bps: 4_000_000,
        audio_bps: 160_000,
    },
    Rendition {
        name: "480p",
        width: 854,
        height: 480,
        average_video_bps: 1_400_000,
        peak_video_bps: 2_000_000,
        audio_bps: 128_000,
    },
    Rendition {
        name: "360p",
        width: 640,
        height: 360,
        average_video_bps: 700_000,
        peak_video_bps: 1_000_000,
        audio_bps: 96_000,
    },
];

/// Browse cards are only 347x195 dp. A single inexpensive rendition gets a
/// useful first frame to the TV faster than starting the full adaptive ladder,
/// while retaining enough detail for the enlarged card on a 4K display.
const PREVIEW_LADDER: [Rendition; 3] = [
    Rendition {
        name: "preview-540p",
        width: 960,
        height: 540,
        average_video_bps: 1_000_000,
        peak_video_bps: 1_400_000,
        audio_bps: 96_000,
    },
    Rendition {
        name: "preview-480p",
        width: 854,
        height: 480,
        average_video_bps: 850_000,
        peak_video_bps: 1_200_000,
        audio_bps: 96_000,
    },
    Rendition {
        name: "preview-360p",
        width: 640,
        height: 360,
        average_video_bps: 600_000,
        peak_video_bps: 900_000,
        audio_bps: 96_000,
    },
];

fn video_variants(
    source_height: u32,
    preferences: &PlaybackPreferences,
    client_limit: u64,
    server_height_cap: u32,
) -> Vec<Rendition> {
    // `0` means the operator set no server-side ceiling. Otherwise it caps
    // both dimensions (16:9 rungs, so a height cap bounds width too).
    let max_height = if server_height_cap == 0 {
        preferences.capabilities.max_height
    } else {
        preferences.capabilities.max_height.min(server_height_cap)
    };
    let max_width = if server_height_cap == 0 {
        preferences.capabilities.max_width
    } else {
        preferences
            .capabilities
            .max_width
            .min(server_height_cap.saturating_mul(16) / 9)
    };
    let eligible = |rendition: &&Rendition| {
        rendition.width <= max_width
            && rendition.height <= max_height
            // The lowest HLS rung is a 640x360 *bounding box*. A source can
            // legitimately be shorter than 360 while still being wider than
            // 640 (real example: The Aviator AVI is 688x288); FFmpeg's
            // force_original_aspect_ratio=decrease scales that file down to
            // fit without enlarging/distorting it. Rejecting solely because
            // 288 < 360 left no rendition and surfaced a false bandwidth 429.
            && rendition.height <= source_height.max(360)
            && rendition.peak_total() <= client_limit
    };
    if preferences.preview {
        // Encoding multiple ABR rungs costs more CPU and delays the first
        // segment. One appropriately-sized preview stream is intentional.
        PREVIEW_LADDER
            .iter()
            .filter(eligible)
            .copied()
            .take(1)
            .collect()
    } else {
        let mut variants: Vec<Rendition> = LADDER.iter().filter(eligible).copied().collect();
        // Preserve both ends of the adaptive range. When every rung is
        // eligible, 480p is the least valuable intermediate step: retaining
        // 1080p/720p/360p saves one simultaneous encoder while keeping the
        // best picture and the low-bandwidth escape hatch. Lower-resolution
        // sources and tighter bandwidth limits still retain all eligible
        // rungs up to this cap.
        while variants.len() > MAX_HLS_VIDEO_RENDITIONS {
            variants.remove(variants.len() - 2);
        }
        variants
    }
}

fn software_threads_per_rendition(available_threads: usize, rendition_count: usize) -> usize {
    let total_video_budget = (available_threads / 2).max(1);
    (total_video_budget / rendition_count.max(1)).max(1)
}

fn encoder_listing_has_videotoolbox(listing: &[u8]) -> bool {
    String::from_utf8_lossy(listing).lines().any(|line| {
        let mut fields = line.split_ascii_whitespace();
        fields.next().is_some_and(|flags| flags.starts_with('V'))
            && fields.next() == Some("h264_videotoolbox")
    })
}

#[derive(Debug)]
enum SessionKind {
    Direct {
        entry_key: String,
    },
    Hls {
        directory: PathBuf,
        child: Option<Child>,
    },
}

#[derive(Debug)]
struct Session {
    kind: SessionKind,
    /// Browse previews are opportunistic. A foreground request may cancel
    /// them immediately so an enhancement never holds playback capacity.
    preview: bool,
    cancelled: Arc<AtomicBool>,
    reserved_bps: u64,
    /// LAN sessions never consume the internet-uplink budget, including if
    /// the global preference is toggled while they are active.
    budget_exempt: bool,
    rate_limiter: Arc<SessionRateLimiter>,
    last_access: Instant,
    in_use: usize,
    /// Whether this reservation is for a music track rather than an
    /// episode/movie. Used only by [`TranscodeManager::cancel_stale_claimed_for_owner`]
    /// to tell a deliberate ahead-of-time track preload (which legitimately
    /// keeps two claimed sessions alive for one owner) apart from an
    /// episode/movie session, which never does.
    track: bool,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, Session>,
    /// Stable peer fingerprint that reserved each session. Kept outside
    /// `Session` so low-level/unit callers that do not have authenticated
    /// transport context retain the existing behavior.
    owners: HashMap<String, String>,
    /// A reservation becomes claimed on its first playlist/media request.
    /// Until then, a retry from the same peer may safely supersede it: the
    /// peer never received (or never acted on) the old playback plan.
    claimed: HashSet<String>,
}

/**
 * Count ffmpeg processes that are actually still running. Completed HLS
 * sessions retain their generated files for playback, but no longer consume
 * a transcode slot; a child-less HLS session is still starting and does.
 */
fn active_hls_processes(state: &mut State) -> usize {
    state
        .sessions
        .values_mut()
        .map(|session| {
            if session.cancelled.load(Ordering::Relaxed) {
                return false;
            }
            match &mut session.kind {
                SessionKind::Hls {
                    child: Some(child), ..
                } => match child.try_wait() {
                    Ok(Some(_)) => false,
                    Ok(None) | Err(_) => true,
                },
                SessionKind::Hls { child: None, .. } => true,
                SessionKind::Direct { .. } => false,
            }
        })
        .filter(|active| *active)
        .count()
}

pub struct SessionFile {
    pub path: PathBuf,
    pub rate_limiter: Arc<SessionRateLimiter>,
    pub session_id: String,
}

/// A shared per-session pacing clock. All concurrent HLS playlist, init, and
/// segment requests reserve time on this one clock, so opening several QUIC
/// streams cannot multiply a session's upload allocation.
#[derive(Debug)]
pub struct SessionRateLimiter {
    rate_bps: std::sync::atomic::AtomicU64,
    next_available: tokio::sync::Mutex<tokio::time::Instant>,
}

impl SessionRateLimiter {
    fn new(rate_bps: u64) -> Self {
        Self {
            rate_bps: std::sync::atomic::AtomicU64::new(rate_bps),
            next_available: tokio::sync::Mutex::new(tokio::time::Instant::now()),
        }
    }

    pub fn rate_bps(&self) -> u64 {
        self.rate_bps.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Live-updates the pacing rate — used when the auto-measured upload
    /// baseline (see `bandwidth` module in apps/server) changes, so an
    /// already-running session's throttle reflects it immediately rather
    /// than only affecting sessions negotiated after the update.
    pub fn set_rate_bps(&self, rate_bps: u64) {
        self.rate_bps
            .store(rate_bps, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn wait_for(&self, bytes: usize) {
        let rate_bps = self.rate_bps();
        if rate_bps == 0 || bytes == 0 {
            return;
        }
        let now = tokio::time::Instant::now();
        let send_at = {
            let mut next = self.next_available.lock().await;
            if *next < now {
                *next = now;
            }
            let send_at = *next;
            *next += Duration::from_secs_f64(bytes as f64 * 8.0 / rate_bps as f64);
            send_at
        };
        if send_at > now {
            tokio::time::sleep_until(send_at).await;
        }
    }
}

/// See [`TranscodeManager::activity`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct TranscodeActivity {
    /// HLS sessions with a live ffmpeg process right now.
    pub transcode_sessions: usize,
    /// Direct-play sessions (a bandwidth reservation, no ffmpeg process).
    pub direct_sessions: usize,
    /// Upload bandwidth reserved across every session, bits/sec.
    pub reserved_bps: u64,
}

pub struct TranscodeManager {
    config: TranscodeConfig,
    /// Overrides `config.max_upload_bps` once a real measurement lands —
    /// see `set_max_upload_bps`. Starts equal to `config.max_upload_bps`,
    /// so behavior is unchanged until something actually updates it.
    max_upload_bps: std::sync::atomic::AtomicU64,
    upload_budget_enabled: std::sync::atomic::AtomicBool,
    state: Mutex<State>,
    global_rate_limiter: Arc<SessionRateLimiter>,
    video_toolbox_available: tokio::sync::OnceCell<bool>,
    /// Unix-millis of the last VideoToolbox failure; `0` = never failed. In
    /// `Auto` mode hardware is suppressed only while this is within
    /// `VIDEO_TOOLBOX_RETRY_COOLDOWN` of now, so a transient failure no longer
    /// pins the process to software until restart.
    video_toolbox_failed_at: std::sync::atomic::AtomicU64,
    /// Live copy of `VideoEncoderMode` (as `u8`), so the dashboard toggle
    /// takes effect on the next session without a restart.
    video_encoder_mode: std::sync::atomic::AtomicU8,
    /// Live copy of `TranscodeConfig::max_transcode_height`.
    max_transcode_height: std::sync::atomic::AtomicU32,
    /// Live copy of `TranscodeConfig::segment_duration_secs`; applies to
    /// sessions started after a change (in-flight sessions keep their value).
    segment_seconds: std::sync::atomic::AtomicU32,
}

impl TranscodeManager {
    pub fn new(config: TranscodeConfig) -> Arc<Self> {
        cleanup_stale_session_dirs(&config.session_dir);
        let global_rate_limiter = Arc::new(SessionRateLimiter::new(config.usable_upload_bps()));
        let max_upload_bps = std::sync::atomic::AtomicU64::new(config.max_upload_bps);
        let video_encoder_mode =
            std::sync::atomic::AtomicU8::new(config.video_encoder_mode.to_u8());
        let max_transcode_height =
            std::sync::atomic::AtomicU32::new(config.max_transcode_height);
        let segment_seconds =
            std::sync::atomic::AtomicU32::new(config.segment_duration_secs.max(2));
        let manager = Arc::new(Self {
            config,
            max_upload_bps,
            upload_budget_enabled: std::sync::atomic::AtomicBool::new(true),
            state: Mutex::new(State::default()),
            global_rate_limiter,
            video_toolbox_available: tokio::sync::OnceCell::new(),
            video_toolbox_failed_at: std::sync::atomic::AtomicU64::new(0),
            video_encoder_mode,
            max_transcode_height,
            segment_seconds,
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&manager);
            handle.spawn(async move { cleanup_loop(weak).await });
        }
        manager
    }

    pub fn config(&self) -> &TranscodeConfig {
        &self.config
    }

    /// The live usable streaming budget — `config.usable_upload_bps()`
    /// recomputed against whatever `max_upload_bps` currently holds
    /// (the configured/default value until a real measurement overrides
    /// it), not the static config value alone.
    pub fn usable_upload_bps(&self) -> u64 {
        let max = self
            .max_upload_bps
            .load(std::sync::atomic::Ordering::Relaxed);
        let reserve = self.config.reserve_percent.min(90) as u64;
        max.saturating_mul(100 - reserve) / 100
    }

    /// Called with a freshly measured real upload rate (see the
    /// `bandwidth` module) — updates both the admission-control budget
    /// (`usable_upload_bps`, gates *new* session negotiation) and the
    /// live global pacing rate (`global_rate_limiter`, throttles bytes
    /// actually being sent by sessions already in flight), so a change
    /// takes effect immediately rather than only for future sessions.
    pub fn set_max_upload_bps(&self, bps: u64) {
        self.max_upload_bps
            .store(bps, std::sync::atomic::Ordering::Relaxed);
        self.global_rate_limiter
            .set_rate_bps(if self.upload_budget_enabled() {
                self.usable_upload_bps()
            } else {
                0
            });
    }

    /// Live operator override for encoder selection (dashboard toggle).
    pub fn set_video_encoder_mode(&self, mode: VideoEncoderMode) {
        self.video_encoder_mode
            .store(mode.to_u8(), std::sync::atomic::Ordering::Relaxed);
        // A deliberate mode change is a fresh start — clear any lingering
        // hardware-failure suppression so `Auto`/`Hardware` retries at once.
        self.video_toolbox_failed_at
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn video_encoder_mode(&self) -> VideoEncoderMode {
        VideoEncoderMode::from_u8(
            self.video_encoder_mode
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Live cap on transcode output height; `0` disables the server-imposed
    /// ceiling. Only affects sessions negotiated after the change.
    pub fn set_max_transcode_height(&self, height: u32) {
        self.max_transcode_height
            .store(height, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn max_transcode_height(&self) -> u32 {
        self.max_transcode_height
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Live HLS segment length in seconds (clamped to >= 2). Applies to
    /// sessions started after the change; in-flight FFmpeg keeps its value.
    pub fn set_hls_segment_seconds(&self, seconds: u32) {
        self.segment_seconds
            .store(seconds.max(2), std::sync::atomic::Ordering::Relaxed);
    }

    fn segment_seconds(&self) -> u32 {
        self.segment_seconds
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(2)
    }

    pub fn upload_budget_enabled(&self) -> bool {
        self.upload_budget_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Enables/disables both admission control and byte pacing. LAN
    /// connections are separately exempted by the request-serving layer.
    pub fn set_upload_budget_enabled(&self, enabled: bool) {
        self.upload_budget_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        self.global_rate_limiter
            .set_rate_bps(if enabled { self.usable_upload_bps() } else { 0 });
        for session in self.state.lock().unwrap().sessions.values() {
            session
                .rate_limiter
                .set_rate_bps(if enabled { session.reserved_bps } else { 0 });
        }
    }

    pub fn should_throttle(&self, is_lan: bool) -> bool {
        self.upload_budget_enabled() && !is_lan
    }

    pub fn active_sessions(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .sessions
            .values()
            .filter(|session| !session.cancelled.load(Ordering::Relaxed))
            .count()
    }

    pub fn reserved_bps(&self) -> u64 {
        self.state
            .lock()
            .unwrap()
            .sessions
            .values()
            .filter(|session| !session.cancelled.load(Ordering::Relaxed))
            .map(|session| session.reserved_bps)
            .sum()
    }

    pub fn global_rate_limiter(&self) -> Arc<SessionRateLimiter> {
        Arc::clone(&self.global_rate_limiter)
    }

    /// A point-in-time summary of what the transcoder is doing right now,
    /// for the dashboard's live "Transcoding" graph (see
    /// `apps/server`'s `transcode_activity` module). `transcode_sessions`
    /// counts only sessions with a live ffmpeg process — a completed HLS
    /// job that still serves its generated segments, and every direct-play
    /// session, are excluded from it (direct play is reported separately).
    pub fn activity(&self) -> TranscodeActivity {
        let mut state = self.state.lock().unwrap();
        let transcode_sessions = active_hls_processes(&mut state);
        let direct_sessions = state
            .sessions
            .values()
            .filter(|session| {
                !session.cancelled.load(Ordering::Relaxed)
                    && matches!(session.kind, SessionKind::Direct { .. })
            })
            .count();
        let reserved_bps = state
            .sessions
            .values()
            .filter(|session| !session.cancelled.load(Ordering::Relaxed))
            .map(|session| session.reserved_bps)
            .sum();
        TranscodeActivity {
            transcode_sessions,
            direct_sessions,
            reserved_bps,
        }
    }

    /// Select direct play when both the device and the shared uplink can
    /// safely carry the source. Otherwise reserve the highest eligible HLS
    /// rung and start an adaptive ladder containing it and every lower rung.
    pub async fn plan(
        &self,
        entry: &EntryRecord,
        media_path: &Path,
        preferences: &PlaybackPreferences,
        is_lan: bool,
        owner: Option<&str>,
    ) -> Result<PlaybackPlan, TranscodeError> {
        self.expire_idle();
        // A hover preview is an enhancement, not a prerequisite for actual
        // playback. Withdraw it before admission/direct-play planning so it
        // cannot consume the final ffmpeg slot or upload reservation while a
        // viewer waits on the playback screen.
        if !preferences.preview {
            self.cancel_previews();
            if let Some(owner) = owner {
                self.cancel_unclaimed_for_owner(owner);
                if entry.kind != MediaKind::Track {
                    self.cancel_stale_claimed_for_owner(owner);
                }
            }
        }
        let enforce_budget = self.should_throttle(is_lan);
        let available = if enforce_budget {
            self.available_bps()?
        } else {
            u64::MAX
        };
        let client_limit = preferences.capabilities.max_bitrate.min(available);

        if preferences.prefer_direct && !preferences.preview {
            if let Some(source_peak) = direct_peak_bps(entry) {
                if source_peak <= client_limit
                    && direct_compatible(entry, preferences)
                    && self
                        .direct_play_audio_is_viable(
                            media_path,
                            entry,
                            &preferences.capabilities.audio_codecs,
                        )
                        .await
                {
                    let id = self.reserve(
                        SessionKind::Direct {
                            entry_key: entry.entry_key.clone(),
                        },
                        source_peak,
                        enforce_budget,
                        is_lan,
                        owner,
                        entry.kind == MediaKind::Track,
                    )?;
                    return Ok(PlaybackPlan {
                        mode: PlaybackMode::Direct,
                        path: format!("/stream/{id}/media"),
                        max_bitrate: source_peak,
                        session_id: id,
                        lyrics: None,
                        subtitles: Vec::new(),
                    });
                }
            }
        }

        if !self.config.enabled {
            return Err(TranscodeError::Disabled);
        }
        if !preferences
            .capabilities
            .containers
            .iter()
            .any(|container| container.eq_ignore_ascii_case("hls"))
        {
            return Err(TranscodeError::Unsupported);
        }

        let client_audio_codecs = preferences.capabilities.audio_codecs.clone();

        if entry.kind == MediaKind::Track {
            let audio_bps = 192_000u64.min(client_limit);
            if audio_bps < 96_000 {
                return Err(TranscodeError::Bandwidth);
            }
            return self
                .start_hls(
                    entry,
                    media_path,
                    preferences.start_position_secs,
                    &[],
                    HlsReservation {
                        reserved_bps: audio_bps,
                        enforce_budget,
                        budget_exempt: is_lan,
                    },
                    preferences.preview,
                    owner,
                    None,
                    &client_audio_codecs,
                )
                .await;
        }

        // The client can already decode this video track as-is — only the
        // container or an audio codec forced a transcode. Copy the video into
        // HLS instead of burning an encoder on it. Restricted to LAN: a remote
        // client still wants a scaled adaptive ladder for bandwidth
        // adaptivity, whereas on LAN bandwidth is a non-issue and the only
        // thing a transcode buys is wasted CPU. Never for previews, which want
        // the smallest possible first segment.
        if is_lan
            && !preferences.preview
            && remux_video_compatible(entry, preferences, client_limit)
        {
            let video = entry.video.as_ref().expect("remux_video_compatible checked video");
            let reserved_bps = direct_peak_bps(entry).unwrap_or(client_limit);
            let source_rendition = Rendition {
                name: "source",
                width: video.width,
                height: video.height,
                average_video_bps: reserved_bps,
                peak_video_bps: reserved_bps,
                audio_bps: 192_000,
            };
            return self
                .start_hls(
                    entry,
                    media_path,
                    preferences.start_position_secs,
                    std::slice::from_ref(&source_rendition),
                    HlsReservation {
                        reserved_bps,
                        enforce_budget,
                        budget_exempt: is_lan,
                    },
                    preferences.preview,
                    owner,
                    Some(VideoEncoder::Copy),
                    &client_audio_codecs,
                )
                .await;
        }

        let source_height = entry
            .video
            .as_ref()
            .map(|video| video.height)
            .unwrap_or(preferences.capabilities.max_height);
        let mut variants = video_variants(
            source_height,
            preferences,
            client_limit,
            self.max_transcode_height(),
        );
        if variants.is_empty() {
            return Err(TranscodeError::Bandwidth);
        }
        // An on-LAN player does not need the full upload ladder, but it does
        // need somewhere to adapt down to when the encoder can't keep
        // real-time — a single rung dead-ends in a hard playback error
        // instead of degrading. Keep the best rung plus the lowest as an
        // escape hatch; drop the intermediate ones.
        if is_lan && !preferences.preview && variants.len() > 2 {
            let lowest = variants[variants.len() - 1];
            variants.truncate(1);
            variants.push(lowest);
        }
        // LADDER is high-to-low. The first entry is the reservation ceiling;
        // all remaining entries let ExoPlayer adapt downward without using
        // additional network bandwidth.
        let reserved_bps = variants[0].peak_total();
        self.start_hls(
            entry,
            media_path,
            preferences.start_position_secs,
            &variants,
            HlsReservation {
                reserved_bps,
                enforce_budget,
                budget_exempt: is_lan,
            },
            preferences.preview,
            owner,
            None,
            &client_audio_codecs,
        )
        .await
    }

    /// Direct play hands the raw container to the client, which then selects an
    /// audio track on its own — so both language *and* codec support have to
    /// hold for every embedded track, not just the one [direct_compatible]
    /// already checked (that only inspects the scan-time summary's single
    /// first-track codec).
    ///
    /// Codec check: a second track in a codec the client can't decode (DTS,
    /// MP2, …) is never re-validated, so direct play would silently serve a
    /// file whose non-default track the client's `Tracks` API reports
    /// unsupported and drops from the switcher entirely — no error, just a
    /// missing option (#278: a Fire TV played an untagged Spanish/English
    /// MKV's Spanish default fine but had no way to reach the English track
    /// because it decoded in a codec direct play never checked).
    ///
    /// Language check: previews and full playback are both meant to start in
    /// English whenever the source has an English track (regression #191).
    /// The client asks Media3 for English, but with several embedded audio
    /// streams a player still falls back to the container-order/default track
    /// when nothing is a language match — so an English track that is tagged
    /// but not first would play in the wrong language.
    ///
    /// Returns `false` when either check fails, so planning falls through to
    /// the remux/transcode path: HLS re-encodes every track into something
    /// the client's advertised codecs can always take
    /// (`choose_audio_delivery`) and pins the English track as the
    /// `DEFAULT=YES` rendition (see `var_stream_map` in
    /// `spawn_ffmpeg_attempt`). A single audio stream, all tracks already
    /// client-decodable with an English track that's first or absent, or any
    /// probe failure (ffprobe missing, slow share) all return `true` — direct
    /// play behaves as before.
    async fn direct_play_audio_is_viable(
        &self,
        media_path: &Path,
        entry: &EntryRecord,
        client_audio_codecs: &[String],
    ) -> bool {
        if entry.kind == MediaKind::Track {
            return true;
        }
        let options = tokio::time::timeout(
            AUDIO_PROBE_TIMEOUT,
            crate::probe::list_audio_streams(&self.config.ffmpeg_path, media_path),
        )
        .await
        .unwrap_or_default();
        if options.len() <= 1 {
            return true;
        }
        // With multiple anonymous tracks the client cannot express an
        // English preference and container/default ordering is not useful
        // evidence (the reported Daria files are Spanish first/default and
        // English second, with neither track tagged). Force HLS so every
        // stream becomes an explicit selectable rendition instead of relying
        // on the device demuxer's treatment of anonymous tracks.
        if options.iter().any(|option| option.language.is_none()) {
            return false;
        }
        let all_tracks_decodable = options.iter().all(|option| {
            !option.codec.is_empty()
                && client_audio_codecs
                    .iter()
                    .any(|token| codec_matches(token, &canonical_audio_codec(&option.codec)))
        });
        if !all_tracks_decodable {
            return false;
        }
        let preferred_is_tagged_english = options
            .iter()
            .find(|option| option.is_preferred)
            .and_then(|option| option.language.as_deref())
            .is_some_and(crate::probe::is_english);
        let preferred_is_first = matches!(
            options.iter().position(|option| option.is_preferred),
            Some(0) | None
        );
        !(preferred_is_tagged_english && !preferred_is_first)
    }

    pub fn open_direct(&self, session_id: &str) -> Option<(String, Arc<SessionRateLimiter>)> {
        let mut state = self.state.lock().unwrap();
        let opened = {
            let session = state.sessions.get_mut(session_id)?;
            let SessionKind::Direct { entry_key } = &session.kind else {
                return None;
            };
            let entry_key = entry_key.clone();
            session.last_access = Instant::now();
            session.in_use += 1;
            (entry_key, Arc::clone(&session.rate_limiter))
        };
        state.claimed.insert(session_id.to_string());
        Some(opened)
    }

    pub fn open_hls(&self, session_id: &str, relative_path: &str) -> Option<SessionFile> {
        if !safe_hls_path(relative_path) {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        let (path, rate_limiter) = {
            let session = state.sessions.get_mut(session_id)?;
            if session.cancelled.load(Ordering::Relaxed) {
                return None;
            }
            let SessionKind::Hls { directory, child } = &mut session.kind else {
                return None;
            };
            let path = directory.join(relative_path);
            // Once ffmpeg has exited, make sure every media playlist carries an
            // `#EXT-X-ENDLIST` tag. A clean exit already writes one, but a
            // mid-stream ffmpeg failure (for example a transient VideoToolbox
            // session-pool error) leaves the last-written playlist open-ended for
            // good. ExoPlayer keeps reloading an open-ended playlist and, once it
            // stops advancing, aborts playback with `PlaylistStuckException`
            // instead of ending cleanly (#126). Terminating the playlist turns
            // that into an ordinary end-of-stream.
            if relative_path.ends_with(".m3u8") {
                let exited = child
                    .as_mut()
                    .map(|child| matches!(child.try_wait(), Ok(Some(_)) | Err(_)))
                    .unwrap_or(false);
                if exited {
                    finalize_hls_playlists(directory);
                }
            }
            session.last_access = Instant::now();
            session.in_use += 1;
            (path, Arc::clone(&session.rate_limiter))
        };
        state.claimed.insert(session_id.to_string());
        Some(SessionFile {
            path,
            rate_limiter,
            session_id: session_id.to_string(),
        })
    }

    pub fn finish_use(&self, session_id: &str) {
        if let Some(session) = self.state.lock().unwrap().sessions.get_mut(session_id) {
            session.in_use = session.in_use.saturating_sub(1);
            session.last_access = Instant::now();
        }
    }

    /// Client-initiated early release (`/stop/{id}`) — the player screen was
    /// torn down (back-press, or moving on to the next entry), so free this
    /// session's bandwidth reservation now instead of leaving it held for
    /// the full `idle_timeout`, which would otherwise reject a same-device
    /// replay attempt with `not enough upload bandwidth` for however long
    /// remained. Unconditional unlike `expire_idle` (no `in_use == 0`
    /// gate): the client is telling us it's done, not merely paused between
    /// segment requests.
    pub fn release(&self, session_id: &str) {
        self.remove_session(session_id);
    }

    // Bandwidth only — deliberately not gated on max_sessions here. This is
    // called once at the top of plan() before direct-vs-HLS is decided, and
    // max_sessions models concurrent *ffmpeg processes*, which a direct-play
    // session never spawns. Gating it here blocked plain direct-play-eligible
    // files (confirmed live: a music track that never needed a transcode at
    // all got a flat 429 "transcode capacity is full" because two earlier,
    // unrelated movie sessions hadn't hit their 5-minute idle expiry yet).
    fn available_bps(&self) -> Result<u64, TranscodeError> {
        let state = self.state.lock().unwrap();
        let reserved: u64 = state
            .sessions
            .values()
            .filter(|session| !session.budget_exempt && !session.cancelled.load(Ordering::Relaxed))
            .map(|session| session.reserved_bps)
            .sum();
        Ok(self.usable_upload_bps().saturating_sub(reserved))
    }

    /// Only ever called for `SessionKind::Direct` (see `plan()`) — no
    /// max_sessions check for the same reason as `available_bps()` above;
    /// direct play is bandwidth-limited only, never process-limited.
    fn reserve(
        &self,
        kind: SessionKind,
        reserved_bps: u64,
        enforce_budget: bool,
        budget_exempt: bool,
        owner: Option<&str>,
        track: bool,
    ) -> Result<String, TranscodeError> {
        let mut state = self.state.lock().unwrap();
        let already_reserved: u64 = state
            .sessions
            .values()
            .filter(|session| !session.budget_exempt && !session.cancelled.load(Ordering::Relaxed))
            .map(|session| session.reserved_bps)
            .sum();
        if enforce_budget
            && already_reserved.saturating_add(reserved_bps) > self.usable_upload_bps()
        {
            return Err(TranscodeError::Bandwidth);
        }
        let id = session_id();
        state.sessions.insert(
            id.clone(),
            Session {
                kind,
                preview: false,
                cancelled: Arc::new(AtomicBool::new(false)),
                reserved_bps,
                budget_exempt,
                rate_limiter: Arc::new(SessionRateLimiter::new(reserved_bps)),
                last_access: Instant::now(),
                in_use: 0,
                track,
            },
        );
        if let Some(owner) = owner {
            state.owners.insert(id.clone(), owner.to_string());
        }
        Ok(id)
    }

    fn cancel_previews(&self) {
        let previews = self
            .state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .filter(|(_, session)| session.preview)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in previews {
            self.remove_session(&id);
        }
    }

    /// A foreground retry from one authenticated TV replaces only that TV's
    /// reservations which were never opened. This is the gap where a lost
    /// `/play` response or client crash used to leave FFmpeg consuming the
    /// transcode limit even though the TV had no session id with which to
    /// call `/stop`. Claimed streams and every other TV are untouched.
    fn cancel_unclaimed_for_owner(&self, owner: &str) {
        let abandoned = {
            let state = self.state.lock().unwrap();
            state
                .owners
                .iter()
                .filter(|(id, session_owner)| {
                    session_owner.as_str() == owner && !state.claimed.contains(*id)
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        };
        for id in abandoned {
            self.remove_unclaimed_session_for_owner(&id, owner);
        }
    }

    fn remove_unclaimed_session_for_owner(&self, id: &str, owner: &str) {
        let removed = {
            let mut state = self.state.lock().unwrap();
            // Recheck while holding the removal lock: the player may have
            // opened and claimed this session after the candidate list was
            // collected but before this iteration reached it.
            if state.claimed.contains(id) || state.owners.get(id).map(String::as_str) != Some(owner)
            {
                None
            } else {
                state.owners.remove(id);
                state.sessions.remove(id)
            }
        };
        Self::cleanup_removed_session(removed);
    }

    /// Companion to [`Self::cancel_unclaimed_for_owner`] for the one gap it
    /// deliberately leaves open: a session the peer *did* claim (its first
    /// playlist/segment request landed) but then never released. That gap is
    /// normally fine — a live client keeps re-requesting segments, so a
    /// truly-in-use claimed session is never mistaken for garbage — but a
    /// hard client crash right after the claim (#358: reported as the app
    /// crashing on the season/episode boundary while advancing to the next
    /// episode) leaves it claimed forever, with no `/stop` ever coming to
    /// free it. Until `idle_timeout` (five minutes) finally reaps it, that
    /// orphaned reservation keeps consuming a `max_sessions` slot, so
    /// retrying the very episode that crashed can fail admission with
    /// "capacity full" even though nothing is actually still playing.
    ///
    /// A fresh, non-preview, non-track request from the *same* owner is safe
    /// grounds to reap that owner's other claimed sessions immediately: the
    /// episode/movie flow always releases its previous session via `/stop`
    /// before negotiating the next one (see `playEntry`/`preloadNextEpisode`
    /// in the TV client), so any such session still claimed at this point
    /// belongs to an abandoned attempt, not a second stream this owner is
    /// legitimately still watching. Track sessions are excluded because
    /// `preloadNextTrack` intentionally keeps the currently-playing track's
    /// session claimed while it negotiates the next one ahead of a gapless
    /// transition — that pair of claimed sessions for one owner is expected,
    /// not orphaned.
    fn cancel_stale_claimed_for_owner(&self, owner: &str) {
        let stale = {
            let state = self.state.lock().unwrap();
            state
                .owners
                .iter()
                .filter(|(id, session_owner)| {
                    session_owner.as_str() == owner
                        && state.claimed.contains(*id)
                        && state
                            .sessions
                            .get(*id)
                            .is_some_and(|session| !session.track)
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        };
        for id in stale {
            self.remove_session(&id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_hls(
        &self,
        entry: &EntryRecord,
        media_path: &Path,
        start_position_secs: u64,
        variants: &[Rendition],
        reservation: HlsReservation,
        preview: bool,
        owner: Option<&str>,
        encoder_override: Option<VideoEncoder>,
        client_audio_codecs: &[String],
    ) -> Result<PlaybackPlan, TranscodeError> {
        let HlsReservation {
            reserved_bps,
            enforce_budget,
            budget_exempt,
        } = reservation;
        let cancelled = Arc::new(AtomicBool::new(false));
        std::fs::create_dir_all(&self.config.session_dir).map_err(TranscodeError::Workspace)?;
        let id = session_id();
        let directory = self.config.session_dir.join(&id);
        std::fs::create_dir_all(&directory).map_err(TranscodeError::Workspace)?;
        if variants.is_empty() {
            std::fs::create_dir_all(directory.join("vaudio")).map_err(TranscodeError::Workspace)?;
        } else {
            for rendition in variants {
                std::fs::create_dir_all(directory.join(format!("v{}", rendition.name)))
                    .map_err(TranscodeError::Workspace)?;
            }
        }

        {
            let mut state = self.state.lock().unwrap();
            // Only count other Hls sessions here — a concurrently-open Direct
            // session holds no ffmpeg process and shouldn't spend a slot of
            // this specifically process/CPU-oriented limit.
            let transcode_sessions = active_hls_processes(&mut state);
            if transcode_sessions >= self.config.max_sessions.max(1) {
                let _ = std::fs::remove_dir_all(&directory);
                return Err(TranscodeError::Capacity);
            }
            let already_reserved: u64 = state
                .sessions
                .values()
                .filter(|session| {
                    !session.budget_exempt && !session.cancelled.load(Ordering::Relaxed)
                })
                .map(|session| session.reserved_bps)
                .sum();
            if enforce_budget
                && already_reserved.saturating_add(reserved_bps) > self.usable_upload_bps()
            {
                let _ = std::fs::remove_dir_all(&directory);
                return Err(TranscodeError::Bandwidth);
            }
            state.sessions.insert(
                id.clone(),
                Session {
                    kind: SessionKind::Hls {
                        directory: directory.clone(),
                        child: None,
                    },
                    preview,
                    cancelled: Arc::clone(&cancelled),
                    reserved_bps,
                    budget_exempt,
                    rate_limiter: Arc::new(SessionRateLimiter::new(reserved_bps)),
                    last_access: Instant::now(),
                    in_use: 0,
                    track: entry.kind == MediaKind::Track,
                },
            );
            if let Some(owner) = owner {
                state.owners.insert(id.clone(), owner.to_string());
            }
        }

        // Async cancellation can happen at every await below (most notably
        // when the peer disconnects during a long startup). Without a drop
        // guard that leaves a child-less session in the map forever, and it
        // still counts against max_sessions. Arm cleanup until the child is
        // successfully installed and the plan is ready to return.
        let mut pending = PendingSessionGuard::new(self, id.clone());

        let result = self
            .spawn_ffmpeg(
                entry,
                media_path,
                start_position_secs,
                variants,
                reserved_bps,
                &directory,
                preview,
                encoder_override,
                client_audio_codecs,
                &cancelled,
            )
            .await;
        let child = match result {
            Ok(child) => child,
            Err(error) => {
                self.remove_session(&id);
                return Err(error);
            }
        };
        let mut child = Some(child);
        let installed = {
            let mut state = self.state.lock().unwrap();
            if let Some(Session {
                kind: SessionKind::Hls { child: slot, .. },
                cancelled,
                ..
            }) = state.sessions.get_mut(&id)
            {
                if !cancelled.load(Ordering::Relaxed) {
                    *slot = child.take();
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if !installed {
            if let Some(child) = child.as_mut() {
                let _ = child.start_kill();
            }
            self.remove_session(&id);
            return Err(TranscodeError::Superseded);
        }

        pending.disarm();

        Ok(PlaybackPlan {
            mode: PlaybackMode::Hls,
            path: format!("/hls/{id}/master.m3u8"),
            max_bitrate: reserved_bps,
            session_id: id,
            lyrics: None,
            subtitles: Vec::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_ffmpeg(
        &self,
        entry: &EntryRecord,
        media_path: &Path,
        start_position_secs: u64,
        variants: &[Rendition],
        reserved_bps: u64,
        directory: &Path,
        preview: bool,
        encoder_override: Option<VideoEncoder>,
        client_audio_codecs: &[String],
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Child, TranscodeError> {
        let scan_found_audio = entry.audio.is_some();
        // Audio language is routing metadata, not part of the codec summary
        // retained at scan time. Resolve every embedded audio stream (not
        // just one) when an HLS session is created, so both normal
        // transcodes and previews can offer the viewer every track the
        // container actually has — mapping only a single server-picked
        // track (the historic behavior) left nothing for the pause/playback
        // screen to switch between even when the source had six (#55). Bound
        // this metadata read tightly because a slow network share must not
        // hold a hover preview in negotiation.
        let audio_tracks: Vec<AudioMapEntry> = if entry.kind == MediaKind::Track {
            vec![AudioMapEntry {
                source_map: "0:a:0".to_string(),
                name: "audio".to_string(),
                language: None,
                is_default: true,
                codec: entry
                    .audio
                    .as_ref()
                    .map(|audio| audio.codec.clone())
                    .unwrap_or_default(),
                channels: entry.audio.as_ref().map(|audio| audio.channels).unwrap_or(0),
            }]
        } else {
            let options = tokio::time::timeout(
                AUDIO_PROBE_TIMEOUT,
                crate::probe::list_audio_streams(&self.config.ffmpeg_path, media_path),
            )
            .await
            .ok()
            .unwrap_or_default();
            // Trust a fresh probe over the scan-time summary. A library scanned
            // before ffprobe was reachable (macOS GUI `PATH`, #203) has an
            // empty `entry.audio` even for files that really do have sound, so
            // gating on it dropped audio from every HLS session. Skip audio
            // only when neither the scan nor this probe found any — a genuinely
            // silent video then still transcodes instead of failing on a dead
            // `0:a:0` map.
            if options.is_empty() && !scan_found_audio {
                Vec::new()
            } else {
                audio_map_entries(options)
            }
        };
        let has_audio = !audio_tracks.is_empty();

        if cancelled.load(Ordering::Relaxed) {
            return Err(TranscodeError::Superseded);
        }

        let encoder = match encoder_override {
            Some(encoder) => encoder,
            None => self.preferred_video_encoder(variants.len()).await,
        };
        if cancelled.load(Ordering::Relaxed) {
            return Err(TranscodeError::Superseded);
        }
        let seek = self
            .resolve_seek_plan(media_path, start_position_secs, variants, encoder)
            .await;
        let attempt = self
            .spawn_ffmpeg_attempt(
                media_path,
                seek,
                variants,
                reserved_bps,
                directory,
                preview,
                has_audio,
                &audio_tracks,
                encoder,
                client_audio_codecs,
                cancelled,
            )
            .await;
        if attempt.is_ok()
            || encoder != VideoEncoder::VideoToolbox
            || matches!(&attempt, Err(TranscodeError::Superseded))
        {
            return attempt;
        }

        // An encoder can be advertised by FFmpeg but unavailable at runtime
        // (for example, a temporarily exhausted VideoToolbox session pool).
        // Timestamp the failure so `Auto` mode backs off hardware for a
        // cooldown window (not forever), preserve its log, and retry this
        // session with the bounded portable encoder.
        self.video_toolbox_failed_at
            .store(now_unix_millis(), std::sync::atomic::Ordering::Relaxed);
        reset_hls_attempt(directory)?;
        self.spawn_ffmpeg_attempt(
            media_path,
            seek,
            variants,
            reserved_bps,
            directory,
            preview,
            has_audio,
            &audio_tracks,
            self.software_video_encoder(variants.len()),
            client_audio_codecs,
            cancelled,
        )
        .await
    }

    /// `true` while a recent VideoToolbox failure should still suppress
    /// hardware encoding in `Auto` mode.
    fn video_toolbox_in_cooldown(&self) -> bool {
        let failed_at = self
            .video_toolbox_failed_at
            .load(std::sync::atomic::Ordering::Relaxed);
        failed_at != 0
            && now_unix_millis().saturating_sub(failed_at)
                < VIDEO_TOOLBOX_RETRY_COOLDOWN.as_millis() as u64
    }

    async fn ffmpeg_lists_videotoolbox(&self) -> bool {
        *self
            .video_toolbox_available
            .get_or_init(|| async {
                let output = tokio::time::timeout(
                    Duration::from_secs(3),
                    Command::new(&self.config.ffmpeg_path)
                        .args(["-hide_banner", "-encoders"])
                        .output(),
                )
                .await;
                matches!(
                    output,
                    Ok(Ok(output)) if output.status.success()
                        && encoder_listing_has_videotoolbox(&output.stdout)
                )
            })
            .await
    }

    async fn preferred_video_encoder(&self, rendition_count: usize) -> VideoEncoder {
        if rendition_count == 0 {
            return self.software_video_encoder(rendition_count);
        }
        match self.video_encoder_mode() {
            VideoEncoderMode::Software => self.software_video_encoder(rendition_count),
            VideoEncoderMode::Hardware => {
                // Operator pinned hardware — honor it whenever FFmpeg has the
                // encoder at all, ignoring the auto-mode cooldown. A genuine
                // spawn failure still falls back per-attempt in `spawn_ffmpeg`.
                if self.ffmpeg_lists_videotoolbox().await {
                    VideoEncoder::VideoToolbox
                } else {
                    tracing::warn!(
                        "video encoder forced to hardware but this FFmpeg has no h264_videotoolbox; using libx264"
                    );
                    self.software_video_encoder(rendition_count)
                }
            }
            VideoEncoderMode::Auto => {
                if cfg!(target_os = "macos")
                    && !self.video_toolbox_in_cooldown()
                    && self.ffmpeg_lists_videotoolbox().await
                {
                    VideoEncoder::VideoToolbox
                } else {
                    self.software_video_encoder(rendition_count)
                }
            }
        }
    }

    fn software_video_encoder(&self, rendition_count: usize) -> VideoEncoder {
        let available_threads = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2);
        VideoEncoder::Libx264 {
            threads_per_rendition: software_threads_per_rendition(
                available_threads,
                rendition_count,
            ),
        }
    }

    /// Decide how a mid-file start position is fed to FFmpeg. A re-encoded video
    /// stream alongside a stream-copied audio track desyncs under a single input
    /// `-ss` (#226), so for that combination the seek is split into an input
    /// seek to the previous keyframe plus a short output seek. Copy/remux and
    /// audio-only outputs, or any probe failure, keep the plain input seek.
    async fn resolve_seek_plan(
        &self,
        media_path: &Path,
        start_position_secs: u64,
        variants: &[Rendition],
        encoder: VideoEncoder,
    ) -> SeekPlan {
        let plain = SeekPlan::input_only(start_position_secs as f64);
        if start_position_secs == 0 || variants.is_empty() || encoder == VideoEncoder::Copy {
            return plain;
        }
        match tokio::time::timeout(
            KEYFRAME_PROBE_TIMEOUT,
            crate::probe::keyframe_at_or_before(
                &self.config.ffmpeg_path,
                media_path,
                start_position_secs,
            ),
        )
        .await
        {
            Ok(Some(keyframe)) if keyframe > 0.0 && keyframe < start_position_secs as f64 => {
                SeekPlan {
                    input_secs: keyframe,
                    output_secs: start_position_secs as f64 - keyframe,
                }
            }
            _ => plain,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_ffmpeg_attempt(
        &self,
        media_path: &Path,
        seek: SeekPlan,
        variants: &[Rendition],
        reserved_bps: u64,
        directory: &Path,
        preview: bool,
        has_audio: bool,
        audio_tracks: &[AudioMapEntry],
        encoder: VideoEncoder,
        client_audio_codecs: &[String],
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Child, TranscodeError> {
        let log_path = directory.join("ffmpeg.log");
        let log = std::fs::File::create(&log_path).map_err(TranscodeError::Workspace)?;
        let mut command = Command::new(&self.config.ffmpeg_path);
        command
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("warning")
            .arg("-nostdin")
            .arg("-y");
        if seek.input_secs > 0.0 {
            command.arg("-ss").arg(format_seek_secs(seek.input_secs));
        }
        command.arg("-i").arg(media_path);
        if seek.output_secs > 0.0 {
            // Output seek: trims the re-encoded video and the copied audio to
            // the same point after the coarse input seek landed on a keyframe.
            command.arg("-ss").arg(format_seek_secs(seek.output_secs));
        }
        if preview {
            // The client displays 30 seconds. Stop the encoder shortly after
            // that window instead of racing through the remainder of a movie.
            command.arg("-t").arg(PREVIEW_DURATION_SECS.to_string());
        }

        let output_pattern = directory.join("v%v/index.m3u8");
        let segment_pattern = directory.join("v%v/segment_%06d.m4s");

        if variants.is_empty() {
            let audio = audio_tracks.first().cloned().unwrap_or(AudioMapEntry {
                source_map: "0:a:0".to_string(),
                name: "audio".to_string(),
                language: None,
                is_default: true,
                codec: String::new(),
                channels: 0,
            });
            command
                .arg("-vn")
                .arg("-map")
                .arg(&audio.source_map)
                .arg("-c:a:0")
                .arg("aac")
                .arg("-b:a:0")
                .arg(reserved_bps.to_string())
                .arg("-ac:a:0")
                .arg("2")
                .arg("-ar:a:0")
                .arg("48000");
        } else {
            // Each track's own subdirectory must exist before ffmpeg starts
            // writing into it, same as the per-rendition video directories
            // created by the caller (start_hls) — ffmpeg's hls muxer does
            // not create intermediate directories itself.
            for audio in audio_tracks {
                std::fs::create_dir_all(directory.join(format!("v{}", audio.name)))
                    .map_err(TranscodeError::Workspace)?;
            }
            if encoder == VideoEncoder::Copy {
                // Remux: the client already decodes this exact track, so it is
                // copied bit-for-bit — no scale filter, no encoder. `variants`
                // holds the single synthetic "source" rung.
                command
                    .arg("-map")
                    .arg("0:v:0")
                    .arg("-c:v:0")
                    .arg("copy");
            } else {
                let split_labels = (0..variants.len())
                    .map(|index| format!("[split{index}]"))
                    .collect::<String>();
                let mut filter = format!("[0:v:0]split={}{};", variants.len(), split_labels);
                for (index, rendition) in variants.iter().enumerate() {
                    filter.push_str(&format!(
                        "[split{index}]scale=w={}:h={}:force_original_aspect_ratio=decrease:force_divisible_by=2[v{index}];",
                        rendition.width, rendition.height
                    ));
                }
                filter.pop();
                command
                    .arg("-filter_complex_threads")
                    .arg("2")
                    .arg("-filter_complex")
                    .arg(filter);
                for (index, rendition) in variants.iter().enumerate() {
                    command.arg("-map").arg(format!("[v{index}]"));
                    command.arg(format!("-c:v:{index}")).arg(match encoder {
                        VideoEncoder::VideoToolbox => "h264_videotoolbox",
                        VideoEncoder::Libx264 { .. } => "libx264",
                        VideoEncoder::Copy => unreachable!("copy handled above"),
                    });
                    if let VideoEncoder::Libx264 {
                        threads_per_rendition,
                    } = encoder
                    {
                        command
                            .arg(format!("-preset:v:{index}"))
                            .arg(if preview { "ultrafast" } else { "veryfast" })
                            .arg(format!("-threads:v:{index}"))
                            .arg(threads_per_rendition.to_string());
                        if preview {
                            command.arg(format!("-tune:v:{index}")).arg("zerolatency");
                        }
                    }
                    command
                        .arg(format!("-profile:v:{index}"))
                        .arg("high")
                        .arg(format!("-level:v:{index}"))
                        .arg("4.1")
                        .arg(format!("-pix_fmt:v:{index}"))
                        .arg("yuv420p");
                    command
                        .arg(format!("-b:v:{index}"))
                        .arg(rendition.average_video_bps.to_string())
                        .arg(format!("-maxrate:v:{index}"))
                        .arg(rendition.peak_video_bps.to_string())
                        .arg(format!("-bufsize:v:{index}"))
                        .arg((rendition.peak_video_bps * 2).to_string());
                    if matches!(encoder, VideoEncoder::Libx264 { .. }) {
                        command.arg(format!("-sc_threshold:v:{index}")).arg("0");
                    }
                    command
                        .arg(format!("-force_key_frames:v:{index}"))
                        .arg(if preview {
                            "expr:gte(t,n_forced*1)"
                        } else {
                            "expr:gte(t,n_forced*2)"
                        });
                }
            }
            // Every embedded audio track is mapped exactly once and shared
            // across all video renditions through a common `agroup` below,
            // instead of the old one-audio-encode-per-video-rendition scheme
            // (#55). Each track is copied when the client can take it as-is,
            // kept as AC-3 5.1 when only the codec needs changing, or downmixed
            // to AAC stereo as the universal fallback.
            if has_audio {
                append_audio_track_args(
                    &mut command,
                    audio_tracks,
                    client_audio_codecs,
                    variants[0].audio_bps,
                );
            }
        }

        let stream_map = if variants.is_empty() {
            "a:0,name:audio".to_string()
        } else {
            let mut entries: Vec<String> = variants
                .iter()
                .enumerate()
                .map(|(index, rendition)| {
                    if has_audio {
                        format!("v:{index},agroup:aud,name:{}", rendition.name)
                    } else {
                        format!("v:{index},name:{}", rendition.name)
                    }
                })
                .collect();
            for (index, audio) in audio_tracks.iter().enumerate() {
                let mut audio_entry = format!("a:{index},agroup:aud,name:{}", audio.name);
                if let Some(language) = audio.language.as_deref().and_then(sanitize_audio_tag) {
                    audio_entry.push_str(&format!(",language:{language}"));
                }
                audio_entry.push_str(if audio.is_default {
                    ",default:yes"
                } else {
                    ",default:no"
                });
                entries.push(audio_entry);
            }
            entries.join(" ")
        };

        command
            .arg("-f")
            .arg("hls")
            .arg("-hls_time")
            .arg(if preview {
                "1".to_string()
            } else {
                self.segment_seconds().to_string()
            })
            .arg("-hls_init_time")
            .arg("1")
            .arg("-hls_list_size")
            .arg("0")
            .arg("-hls_playlist_type")
            .arg("event")
            .arg("-hls_segment_type")
            .arg("fmp4")
            .arg("-hls_fmp4_init_filename")
            .arg("init.mp4")
            .arg("-hls_flags")
            .arg("independent_segments+temp_file")
            .arg("-master_pl_name")
            .arg("master.m3u8")
            .arg("-var_stream_map")
            .arg(stream_map)
            .arg("-hls_segment_filename")
            .arg(segment_pattern)
            .arg(output_pattern)
            .kill_on_drop(true)
            .stdout(Stdio::null())
            .stderr(Stdio::from(log));

        let mut child = command.spawn().map_err(TranscodeError::Spawn)?;
        let master = directory.join("master.m3u8");
        let (hard_cap, stall_timeout) = if preview {
            (PREVIEW_STARTUP_HARD_CAP, PREVIEW_STARTUP_STALL_TIMEOUT)
        } else {
            (STARTUP_HARD_CAP, STARTUP_STALL_TIMEOUT)
        };
        let started = Instant::now();
        let mut last_progress = Instant::now();
        let mut last_output = hls_output_signature(directory);
        loop {
            if cancelled.load(Ordering::Relaxed) {
                let _ = child.kill().await;
                return Err(TranscodeError::Superseded);
            }
            if master.metadata().is_ok_and(|metadata| metadata.len() > 0) {
                return Ok(child);
            }
            if let Some(status) = child.try_wait().map_err(TranscodeError::Spawn)? {
                let detail = std::fs::read_to_string(&log_path)
                    .unwrap_or_else(|_| format!("exit status {status}"));
                return Err(TranscodeError::Ffmpeg(detail.trim().to_string()));
            }
            // A large adaptive-ladder transcode of high-bitrate source off a
            // slow share legitimately takes a while to flush the first segment
            // of every rendition, so wait as long as FFmpeg keeps writing
            // output — bounded only by an absolute cap. Bail early only when it
            // is alive but producing nothing, which is what a wedged decoder or
            // a media root that vanished mid-read looks like (#131).
            let output = hls_output_signature(directory);
            if output != last_output {
                last_output = output;
                last_progress = Instant::now();
            }
            if started.elapsed() >= hard_cap || last_progress.elapsed() >= stall_timeout {
                let _ = child.kill().await;
                return Err(TranscodeError::StartupTimeout);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn expire_idle(&self) {
        let now = Instant::now();
        let expired: Vec<String> = {
            let state = self.state.lock().unwrap();
            state
                .sessions
                .iter()
                .filter(|(_, session)| {
                    session.in_use == 0
                        && now.duration_since(session.last_access) >= self.config.idle_timeout
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in expired {
            self.remove_session(&id);
        }
    }

    fn remove_session(&self, id: &str) {
        let removed = {
            let mut state = self.state.lock().unwrap();
            state.owners.remove(id);
            state.claimed.remove(id);
            state.sessions.remove(id)
        };
        Self::cleanup_removed_session(removed);
    }

    fn cleanup_removed_session(removed: Option<Session>) {
        if let Some(Session {
            kind:
                SessionKind::Hls {
                    directory,
                    mut child,
                },
            cancelled,
            ..
        }) = removed
        {
            cancelled.store(true, Ordering::Relaxed);
            if let Some(child) = child.as_mut() {
                let _ = child.start_kill();
            }
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

/// Cancellation-safe ownership of a just-reserved HLS session. Tokio drops
/// the `start_hls` future when its request stream disappears; `Drop` must then
/// remove the reservation and terminate any encoder started so far.
struct PendingSessionGuard<'a> {
    manager: &'a TranscodeManager,
    session_id: String,
    armed: bool,
}

impl<'a> PendingSessionGuard<'a> {
    fn new(manager: &'a TranscodeManager, session_id: String) -> Self {
        Self {
            manager,
            session_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingSessionGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.manager.remove_session(&self.session_id);
        }
    }
}

impl Drop for TranscodeManager {
    fn drop(&mut self) {
        let sessions = std::mem::take(&mut self.state.lock().unwrap().sessions);
        for (_, session) in sessions {
            session.cancelled.store(true, Ordering::Relaxed);
            if let SessionKind::Hls {
                directory,
                mut child,
            } = session.kind
            {
                if let Some(child) = child.as_mut() {
                    let _ = child.start_kill();
                }
                let _ = std::fs::remove_dir_all(directory);
            }
        }
    }
}

fn reset_hls_attempt(directory: &Path) -> Result<(), TranscodeError> {
    let log_path = directory.join("ffmpeg.log");
    let hardware_log_path = directory.join("ffmpeg-videotoolbox.log");
    if hardware_log_path.exists() {
        std::fs::remove_file(&hardware_log_path).map_err(TranscodeError::Workspace)?;
    }
    if log_path.exists() {
        std::fs::rename(&log_path, &hardware_log_path).map_err(TranscodeError::Workspace)?;
    }

    for entry in std::fs::read_dir(directory).map_err(TranscodeError::Workspace)? {
        let entry = entry.map_err(TranscodeError::Workspace)?;
        let path = entry.path();
        if path == hardware_log_path {
            continue;
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path).map_err(TranscodeError::Workspace)?;
            std::fs::create_dir_all(&path).map_err(TranscodeError::Workspace)?;
        } else {
            std::fs::remove_file(path).map_err(TranscodeError::Workspace)?;
        }
    }
    Ok(())
}

/// A cheap fingerprint of everything FFmpeg has written into a session
/// directory so far — file count plus total bytes across the rendition
/// subdirectories, ignoring its own text log. Used only during startup to tell
/// a slow-but-advancing transcode apart from a wedged one; exact values never
/// matter, only whether it changed while FFmpeg was making progress.
fn hls_output_signature(directory: &Path) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![directory.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if entry.path().extension().and_then(|ext| ext.to_str()) != Some("log") {
                count += 1;
                bytes += entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            }
        }
    }
    (count, bytes)
}

/// Append `#EXT-X-ENDLIST` to every media playlist under `directory` that has
/// at least one segment but no end tag. Best-effort and idempotent: safe to
/// call on each playlist request once the session's ffmpeg process is gone.
fn finalize_hls_playlists(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let rendition_dir = entry.path();
        if rendition_dir.is_dir() {
            finalize_hls_playlist(&rendition_dir.join("index.m3u8"));
        }
    }
}

fn finalize_hls_playlist(playlist: &Path) {
    let Ok(contents) = std::fs::read_to_string(playlist) else {
        return;
    };
    if contents.contains("#EXT-X-ENDLIST") || !contents.contains("#EXTINF") {
        return;
    }
    let mut patched = contents;
    if !patched.ends_with('\n') {
        patched.push('\n');
    }
    patched.push_str("#EXT-X-ENDLIST\n");
    let tmp = playlist.with_file_name("index.m3u8.endlist");
    if std::fs::write(&tmp, patched.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, playlist);
    }
}

async fn cleanup_loop(manager: Weak<TranscodeManager>) {
    let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
    interval.tick().await;
    loop {
        interval.tick().await;
        let Some(manager) = manager.upgrade() else {
            return;
        };
        manager.expire_idle();
    }
}

fn cleanup_stale_session_dirs(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() == 32
            && name.chars().all(|ch| ch.is_ascii_hexdigit())
            && entry.path().is_dir()
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn direct_peak_bps(entry: &EntryRecord) -> Option<u64> {
    let duration = entry.duration_secs.filter(|duration| *duration > 0.0)?;
    let measured_average = (entry.size as f64 * 8.0 / duration) as u64;
    let stream_sum = entry
        .video
        .as_ref()
        .and_then(|video| video.bitrate)
        .unwrap_or(0)
        + entry
            .audio
            .as_ref()
            .and_then(|audio| audio.bitrate)
            .unwrap_or(0);
    Some(measured_average.max(stream_sum).saturating_mul(5) / 4)
}

fn direct_compatible(entry: &EntryRecord, preferences: &PlaybackPreferences) -> bool {
    let capabilities = &preferences.capabilities;
    let extension = entry
        .relative_path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let container = match extension.as_str() {
        "m4v" | "m4a" | "mov" => "mp4",
        other => other,
    };
    if !capabilities
        .containers
        .iter()
        .any(|supported| supported.eq_ignore_ascii_case(container))
    {
        return false;
    }
    if entry.kind != MediaKind::Track {
        if let Some(video) = &entry.video {
            if video.width > capabilities.max_width || video.height > capabilities.max_height {
                return false;
            }
            let canonical = canonical_video_codec(&video.codec);
            let Some(token) = capabilities
                .video_codecs
                .iter()
                .find(|supported| codec_matches(supported, &canonical))
            else {
                return false;
            };
            if !source_level_within(video.level.as_deref(), token) {
                return false;
            }
            if requires_video_reencode_for_client(video, Some(token), capabilities.hdr) {
                return false;
            }
        }
    }
    if let Some(audio) = &entry.audio {
        if !capabilities
            .audio_codecs
            .iter()
            .any(|supported| codec_matches(supported, &audio.codec))
        {
            return false;
        }
    }
    true
}

fn codec_matches(supported: &str, actual: &str) -> bool {
    supported
        .split([':', '@'])
        .next()
        .unwrap_or(supported)
        .eq_ignore_ascii_case(actual)
}

/// Fold ffprobe's various names for the same codec down to the token the
/// capability profile uses (`h264`, `hevc`).
fn canonical_video_codec(codec: &str) -> String {
    match codec.trim().to_ascii_lowercase().as_str() {
        "avc" | "avc1" | "h264" | "x264" => "h264".to_string(),
        "hevc" | "h265" | "hev1" | "hvc1" | "x265" => "hevc".to_string(),
        other => other.to_string(),
    }
}

/// `"4.2"`/`"42"`/`"5.1"` → `42`/`42`/`51`; `None` when unparseable.
fn parse_codec_level(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    if let Some((major, minor)) = raw.split_once('.') {
        Some(major.trim().parse::<u32>().ok()? * 10 + minor.trim().parse::<u32>().ok()?)
    } else {
        raw.parse::<u32>().ok()
    }
}

/// A client codec token carries an optional `@level` suffix (`h264:high@4.2`).
/// Permissive when either side's level is unknown — ExoPlayer copes or adapts.
fn source_level_within(source_level: Option<&str>, client_token: &str) -> bool {
    let Some(max) = client_token.rsplit('@').next().and_then(parse_codec_level) else {
        return true;
    };
    // `rsplit('@').next()` on a token with no `@` yields the whole token,
    // which `parse_codec_level` rejects — so we only get here with a real cap.
    match source_level.and_then(parse_codec_level) {
        Some(source) => source <= max,
        None => true,
    }
}

fn client_token_allows_high_bit_depth(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    ["main10", "main 10", "high10", "high 10", "10le", "hdr", "p10"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// `true` when re-encoding this stream is unavoidable for correctness — HDR
/// to an SDR-only client, or a bit depth the client's decoder profile can't
/// take. Used to bar both direct-play and remux.
fn requires_video_reencode_for_client(
    video: &swarm_core::peer::VideoStreamInfo,
    client_token: Option<&str>,
    client_hdr: bool,
) -> bool {
    if video.hdr == Some(true) && !client_hdr {
        return true;
    }
    if video.bit_depth.unwrap_or(8) > 8
        && !client_token.is_some_and(client_token_allows_high_bit_depth)
    {
        return true;
    }
    false
}

/// The client can already decode this exact video track (codec, resolution,
/// level, bit depth, HDR) — only the container or an audio codec is why
/// direct-play was refused. Copy the video into HLS instead of re-encoding.
fn remux_video_compatible(
    entry: &EntryRecord,
    preferences: &PlaybackPreferences,
    client_limit: u64,
) -> bool {
    let Some(video) = entry.video.as_ref() else {
        return false;
    };
    let canonical = canonical_video_codec(&video.codec);
    // fMP4 (our HLS segment type) carries H.264 and HEVC; nothing else.
    if !matches!(canonical.as_str(), "h264" | "hevc") {
        return false;
    }
    let caps = &preferences.capabilities;
    let Some(token) = caps
        .video_codecs
        .iter()
        .find(|token| codec_matches(token, &canonical))
    else {
        return false;
    };
    if video.width > caps.max_width || video.height > caps.max_height {
        return false;
    }
    if !source_level_within(video.level.as_deref(), token) {
        return false;
    }
    if requires_video_reencode_for_client(video, Some(token), caps.hdr) {
        return false;
    }
    matches!(direct_peak_bps(entry), Some(peak) if peak <= client_limit)
}

/// HLS fMP4 can carry these audio codecs untouched.
fn audio_codec_is_fmp4_muxable(codec: &str) -> bool {
    matches!(
        canonical_audio_codec(codec).as_str(),
        "aac" | "ac3" | "eac3"
    )
}

fn canonical_audio_codec(codec: &str) -> String {
    match codec.trim().to_ascii_lowercase().as_str() {
        "aac" | "aac_latm" | "mp4a" => "aac".to_string(),
        "ac3" | "ac-3" => "ac3".to_string(),
        "eac3" | "e-ac-3" | "ec-3" => "eac3".to_string(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioDelivery {
    /// `-c:a copy` — source track kept bit-for-bit.
    Copy,
    /// Transcode to AC-3 5.1 so a surround client keeps surround.
    Ac3Surround,
    /// Transcode to AAC-LC stereo — the universal fallback.
    AacStereo,
}

/// Preserve surround where the client can actually take it; downmix to AAC
/// stereo only when there is no better option or the probe told us nothing.
fn choose_audio_delivery(
    source_codec: &str,
    source_channels: u32,
    client_audio_codecs: &[String],
) -> AudioDelivery {
    let canonical = canonical_audio_codec(source_codec);
    let client_supports =
        |name: &str| client_audio_codecs.iter().any(|token| codec_matches(token, name));
    if source_channels > 2 && audio_codec_is_fmp4_muxable(&canonical) && client_supports(&canonical)
    {
        return AudioDelivery::Copy;
    }
    if source_channels > 2 && client_supports("ac3") {
        return AudioDelivery::Ac3Surround;
    }
    AudioDelivery::AacStereo
}

/// Appends `-map`/`-c:a`/… for every embedded audio track, one HLS audio
/// rendition each (shared across the video group via `var_stream_map`).
fn append_audio_track_args(
    command: &mut Command,
    audio_tracks: &[AudioMapEntry],
    client_audio_codecs: &[String],
    aac_bitrate: u64,
) {
    for (index, audio) in audio_tracks.iter().enumerate() {
        command.arg("-map").arg(&audio.source_map);
        match choose_audio_delivery(&audio.codec, audio.channels, client_audio_codecs) {
            AudioDelivery::Copy => {
                command.arg(format!("-c:a:{index}")).arg("copy");
            }
            AudioDelivery::Ac3Surround => {
                command
                    .arg(format!("-c:a:{index}"))
                    .arg("ac3")
                    .arg(format!("-b:a:{index}"))
                    .arg("640000")
                    .arg(format!("-ac:a:{index}"))
                    .arg("6")
                    .arg(format!("-ar:a:{index}"))
                    .arg("48000");
            }
            AudioDelivery::AacStereo => {
                command
                    .arg(format!("-c:a:{index}"))
                    .arg("aac")
                    .arg(format!("-b:a:{index}"))
                    .arg(aac_bitrate.to_string())
                    .arg(format!("-ac:a:{index}"))
                    .arg("2")
                    .arg(format!("-ar:a:{index}"))
                    .arg("48000");
            }
        }
    }
}

fn session_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn safe_hls_path(path: &str) -> bool {
    let extension_allowed = matches!(path.rsplit('.').next(), Some("m3u8" | "m4s" | "mp4"));
    extension_allowed
        && !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        })
}

pub fn hls_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "m3u8" => "application/vnd.apple.mpegurl",
        "m4s" => "video/iso.segment",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swarm_core::capability::CapabilityProfile;
    use swarm_core::peer::{AudioStreamInfo, MediaKind, VideoStreamInfo};

    fn entry() -> EntryRecord {
        EntryRecord {
            entry_key: "0123456789abcdef01234567".into(),
            relative_path: "movies/example.mkv".into(),
            kind: MediaKind::Movie,
            title: "Example".into(),
            size: 5_400_000_000,
            modified_time: 0,
            fingerprint: "fp".into(),
            artist: None,
            album: None,
            track_number: None,
            show_title: None,
            season: None,
            episode: None,
            year: None,
            duration_secs: Some(7_200.0),
            video: Some(VideoStreamInfo {
                codec: "h264".into(),
                width: 1920,
                height: 1080,
                level: Some("4.1".into()),
                bitrate: Some(5_800_000),
                ..Default::default()
            }),
            audio: Some(AudioStreamInfo {
                codec: "aac".into(),
                channels: 2,
                bitrate: Some(192_000),
            }),
            scraped_title: None,
            episode_title: None,
            genres: vec![],
            artwork_version: 0,
            cast: vec![],
            overview: None,
            rating: None,
            community_rating: None,
            community_rating_votes: None,
            parent_entry_key: None,
            extra_type: None,
            extra_title: None,
            extra_relative_path: None,
            extra_category_path: None,
        }
    }

    fn preferences() -> PlaybackPreferences {
        PlaybackPreferences {
            capabilities: CapabilityProfile::fire_tv_baseline(),
            start_position_secs: 0,
            prefer_direct: true,
            preview: false,
        }
    }

    #[test]
    fn upload_reserve_is_removed_before_allocating() {
        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: std::env::temp_dir().join("swarm-transcode-test"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 2,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        };
        assert_eq!(config.usable_upload_bps(), 7_000_000);
    }

    #[test]
    fn upload_budget_can_be_disabled_and_never_applies_on_lan() {
        let manager = TranscodeManager::new(TranscodeConfig::disabled(
            std::env::temp_dir().join("swarm-budget-toggle-test"),
        ));
        assert!(manager.should_throttle(false));
        assert!(!manager.should_throttle(true));
        manager.set_upload_budget_enabled(false);
        assert!(!manager.should_throttle(false));
        assert_eq!(manager.global_rate_limiter().rate_bps(), 0);
    }

    #[tokio::test]
    async fn completed_hls_jobs_no_longer_consume_transcode_capacity() {
        let mut completed_child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        assert!(completed_child.wait().await.unwrap().success());

        let session = |kind| Session {
            kind,
            preview: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            reserved_bps: 128_000,
            budget_exempt: true,
            rate_limiter: Arc::new(SessionRateLimiter::new(0)),
            last_access: Instant::now(),
            in_use: 0,
            track: false,
        };
        let mut state = State::default();
        state.sessions.insert(
            "finished".into(),
            session(SessionKind::Hls {
                directory: PathBuf::from("finished"),
                child: Some(completed_child),
            }),
        );
        assert_eq!(active_hls_processes(&mut state), 0);

        // `None` is the short interval after a session is reserved but
        // before spawn_ffmpeg installs its child handle; it must still hold
        // a slot so simultaneous negotiations cannot exceed the limit.
        state.sessions.insert(
            "starting".into(),
            session(SessionKind::Hls {
                directory: PathBuf::from("starting"),
                child: None,
            }),
        );
        state.sessions.insert(
            "direct".into(),
            session(SessionKind::Direct {
                entry_key: "track".into(),
            }),
        );
        assert_eq!(active_hls_processes(&mut state), 1);

        state
            .sessions
            .get("starting")
            .unwrap()
            .cancelled
            .store(true, Ordering::Relaxed);
        assert_eq!(active_hls_processes(&mut state), 0);
    }

    #[test]
    fn foreground_playback_preempts_only_preview_sessions() {
        let root =
            std::env::temp_dir().join(format!("swarm-preview-preemption-test-{}", session_id()));
        let manager = TranscodeManager::new(TranscodeConfig::disabled(root.clone()));
        let preview_cancelled = Arc::new(AtomicBool::new(false));
        let foreground_cancelled = Arc::new(AtomicBool::new(false));
        let session = |kind, preview, cancelled| Session {
            kind,
            preview,
            cancelled,
            reserved_bps: 128_000,
            budget_exempt: true,
            rate_limiter: Arc::new(SessionRateLimiter::new(0)),
            last_access: Instant::now(),
            in_use: 0,
            track: false,
        };
        {
            let mut state = manager.state.lock().unwrap();
            state.sessions.insert(
                "preview".into(),
                session(
                    SessionKind::Hls {
                        directory: root.join("preview"),
                        child: None,
                    },
                    true,
                    Arc::clone(&preview_cancelled),
                ),
            );
            state.sessions.insert(
                "foreground".into(),
                session(
                    SessionKind::Direct {
                        entry_key: "movie".into(),
                    },
                    false,
                    Arc::clone(&foreground_cancelled),
                ),
            );
        }

        manager.cancel_previews();

        assert!(preview_cancelled.load(Ordering::Relaxed));
        assert!(!foreground_cancelled.load(Ordering::Relaxed));
        assert_eq!(manager.active_sessions(), 1);
        assert_eq!(manager.reserved_bps(), 128_000);
    }

    #[test]
    fn retry_preempts_only_unclaimed_sessions_from_the_same_owner() {
        let manager = TranscodeManager::new(TranscodeConfig::disabled(
            std::env::temp_dir().join(format!("swarm-owner-preemption-test-{}", session_id())),
        ));
        let session = || Session {
            kind: SessionKind::Direct {
                entry_key: "movie".into(),
            },
            preview: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            reserved_bps: 128_000,
            budget_exempt: true,
            rate_limiter: Arc::new(SessionRateLimiter::new(0)),
            last_access: Instant::now(),
            in_use: 0,
            track: false,
        };
        {
            let mut state = manager.state.lock().unwrap();
            state.sessions.insert("abandoned".into(), session());
            state.sessions.insert("playing".into(), session());
            state.sessions.insert("other-tv".into(), session());
            state
                .owners
                .insert("abandoned".into(), "living-room".into());
            state.owners.insert("playing".into(), "living-room".into());
            state.owners.insert("other-tv".into(), "bedroom".into());
            state.claimed.insert("playing".into());
        }

        manager.cancel_unclaimed_for_owner("living-room");
        manager.remove_unclaimed_session_for_owner("playing", "living-room");
        manager.remove_unclaimed_session_for_owner("other-tv", "living-room");

        let state = manager.state.lock().unwrap();
        assert!(!state.sessions.contains_key("abandoned"));
        assert!(state.sessions.contains_key("playing"));
        assert!(state.sessions.contains_key("other-tv"));
        assert!(!state.owners.contains_key("abandoned"));
    }

    /// #358: a client that hard-crashes right after claiming its next-episode
    /// session (its first playlist/segment request landed, then the process
    /// died before ever calling `/stop`) used to leave that reservation
    /// consuming a `max_sessions` slot for the full idle timeout, so
    /// retrying the very episode that crashed kept failing with "capacity
    /// full". `cancel_stale_claimed_for_owner` should reap that owner's
    /// other claimed, non-track sessions — but never a claimed track session
    /// (the deliberate ahead-of-time preload from `preloadNextTrack`) and
    /// never another owner's claimed session.
    #[test]
    fn stale_claimed_non_track_sessions_are_reaped_for_the_same_owner() {
        let manager = TranscodeManager::new(TranscodeConfig::disabled(std::env::temp_dir().join(
            format!("swarm-stale-claimed-preemption-test-{}", session_id()),
        )));
        let session = |track| Session {
            kind: SessionKind::Direct {
                entry_key: "movie".into(),
            },
            preview: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            reserved_bps: 128_000,
            budget_exempt: true,
            rate_limiter: Arc::new(SessionRateLimiter::new(0)),
            last_access: Instant::now(),
            in_use: 0,
            track,
        };
        {
            let mut state = manager.state.lock().unwrap();
            state.sessions.insert("crashed-episode".into(), session(false));
            state.sessions.insert("preloaded-track".into(), session(true));
            state.sessions.insert("other-tv-episode".into(), session(false));
            state
                .owners
                .insert("crashed-episode".into(), "living-room".into());
            state
                .owners
                .insert("preloaded-track".into(), "living-room".into());
            state
                .owners
                .insert("other-tv-episode".into(), "bedroom".into());
            state.claimed.insert("crashed-episode".into());
            state.claimed.insert("preloaded-track".into());
            state.claimed.insert("other-tv-episode".into());
        }

        manager.cancel_stale_claimed_for_owner("living-room");

        let state = manager.state.lock().unwrap();
        assert!(!state.sessions.contains_key("crashed-episode"));
        assert!(state.sessions.contains_key("preloaded-track"));
        assert!(state.sessions.contains_key("other-tv-episode"));
        assert!(!state.owners.contains_key("crashed-episode"));
    }

    #[test]
    fn dropping_pending_hls_start_removes_its_capacity_reservation() {
        let root =
            std::env::temp_dir().join(format!("swarm-pending-session-test-{}", session_id()));
        let session_dir = root.join("pending");
        std::fs::create_dir_all(&session_dir).unwrap();
        let manager = TranscodeManager::new(TranscodeConfig::disabled(root));
        {
            let mut state = manager.state.lock().unwrap();
            state.sessions.insert(
                "pending".into(),
                Session {
                    kind: SessionKind::Hls {
                        directory: session_dir.clone(),
                        child: None,
                    },
                    preview: false,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    reserved_bps: 128_000,
                    budget_exempt: true,
                    rate_limiter: Arc::new(SessionRateLimiter::new(0)),
                    last_access: Instant::now(),
                    in_use: 0,
                    track: false,
                },
            );
        }

        drop(PendingSessionGuard::new(&manager, "pending".into()));

        assert_eq!(manager.active_sessions(), 0);
        assert!(!session_dir.exists());
    }

    #[test]
    fn direct_play_requires_container_codec_and_budget_compatibility() {
        let mut compatible = entry();
        compatible.relative_path = "movies/example.mp4".into();
        assert!(direct_compatible(&compatible, &preferences()));
        assert!(direct_peak_bps(&compatible).unwrap() > 6_000_000);

        let incompatible = entry();
        assert!(!direct_compatible(&incompatible, &preferences()));
    }

    #[test]
    fn direct_play_rejects_hdr_and_10bit_for_an_sdr_client() {
        let mut hdr = entry();
        hdr.relative_path = "movies/example.mp4".into();
        hdr.video.as_mut().unwrap().hdr = Some(true);
        assert!(
            !direct_compatible(&hdr, &preferences()),
            "an HDR stream must not direct-play to an SDR-only client"
        );

        let mut ten_bit = entry();
        ten_bit.relative_path = "movies/example.mp4".into();
        ten_bit.video.as_mut().unwrap().bit_depth = Some(10);
        assert!(!direct_compatible(&ten_bit, &preferences()));
    }

    #[test]
    fn remux_compatible_when_only_the_container_blocks_direct_play() {
        // entry() is H.264 1080p in an .mkv — direct-play is refused purely on
        // the container, which the client cannot demux but ffmpeg can remux.
        let mkv = {
            let mut entry = entry();
            entry.video.as_mut().unwrap().bitrate = Some(4_000_000);
            entry
        };
        assert!(!direct_compatible(&mkv, &preferences()));
        assert!(remux_video_compatible(&mkv, &preferences(), u64::MAX));
    }

    #[test]
    fn remux_rejected_for_a_codec_the_client_cannot_decode() {
        let mut mpeg4 = entry();
        mpeg4.video.as_mut().unwrap().codec = "mpeg4".into();
        assert!(!remux_video_compatible(&mpeg4, &preferences(), u64::MAX));

        // HEVC source, HEVC-capable client → remux is on the table.
        let mut hevc_entry = entry();
        hevc_entry.video.as_mut().unwrap().codec = "hevc".into();
        hevc_entry.video.as_mut().unwrap().level = Some("5.1".into());
        hevc_entry.video.as_mut().unwrap().bitrate = Some(6_000_000);
        let mut hevc_client = preferences();
        hevc_client.capabilities.video_codecs = vec!["hevc:main@5.1".into()];
        assert!(remux_video_compatible(
            &hevc_entry,
            &hevc_client,
            u64::MAX
        ));
    }

    #[test]
    fn source_level_check_is_permissive_only_when_a_bound_is_known() {
        assert!(source_level_within(Some("4.1"), "h264:high@4.2"));
        assert!(!source_level_within(Some("5.1"), "h264:high@4.2"));
        assert!(source_level_within(Some("9.9"), "h264")); // no @level on the token
        assert!(source_level_within(None, "h264:high@4.2")); // unknown source level
    }

    #[test]
    fn audio_delivery_keeps_surround_the_client_can_take_and_downmixes_otherwise() {
        let ac3 = vec!["aac".to_string(), "ac3".to_string()];
        // 5.1 AC-3 to an AC-3 client → copy it.
        assert_eq!(
            choose_audio_delivery("ac3", 6, &ac3),
            AudioDelivery::Copy
        );
        // 5.1 DTS to an AC-3 client → transcode to AC-3, not stereo AAC.
        assert_eq!(
            choose_audio_delivery("dts", 6, &ac3),
            AudioDelivery::Ac3Surround
        );
        // 5.1 anything to an AAC-only client → stereo downmix.
        assert_eq!(
            choose_audio_delivery("eac3", 6, &["aac".to_string()]),
            AudioDelivery::AacStereo
        );
        // Stereo source → always plain AAC stereo.
        assert_eq!(
            choose_audio_delivery("ac3", 2, &ac3),
            AudioDelivery::AacStereo
        );
        // Probe told us nothing → safe fallback.
        assert_eq!(
            choose_audio_delivery("", 0, &ac3),
            AudioDelivery::AacStereo
        );
    }

    #[test]
    fn lan_plan_keeps_a_low_fallback_rung() {
        // 720p source, remote client: full ladder of three.
        let mut source = entry();
        source.video.as_mut().unwrap().width = 1280;
        source.video.as_mut().unwrap().height = 720;
        let prefs = preferences();
        let remote = video_variants(720, &prefs, u64::MAX, 0);
        assert_eq!(remote.len(), 3);
        // The LAN pruning in plan() keeps the top rung plus the lowest.
        let mut lan = remote.clone();
        if lan.len() > 2 {
            let lowest = lan[lan.len() - 1];
            lan.truncate(1);
            lan.push(lowest);
        }
        assert_eq!(lan.len(), 2);
        assert_eq!(lan[0], remote[0]);
        assert_eq!(lan[1], remote[remote.len() - 1]);
    }

    #[test]
    fn preview_uses_one_lightweight_rendition() {
        let mut prefs = preferences();
        prefs.preview = true;
        prefs.prefer_direct = false;

        let variants = video_variants(1080, &prefs, u64::MAX, 0);
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].name, "preview-540p");
        assert_eq!(variants[0].height, 540);
        assert_eq!(variants[0].peak_total(), 1_496_000);

        prefs.capabilities.max_height = 480;
        let variants = video_variants(1080, &prefs, u64::MAX, 0);
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].name, "preview-480p");
    }

    #[test]
    fn full_hd_ladder_uses_three_spread_out_renditions() {
        let variants = video_variants(1080, &preferences(), u64::MAX, 0);

        assert_eq!(
            variants
                .iter()
                .map(|variant| variant.name)
                .collect::<Vec<_>>(),
            vec!["1080p", "720p", "360p"]
        );
    }

    #[test]
    fn lower_resolution_sources_keep_their_intermediate_rendition() {
        let variants = video_variants(480, &preferences(), u64::MAX, 0);

        assert_eq!(
            variants
                .iter()
                .map(|variant| variant.name)
                .collect::<Vec<_>>(),
            vec!["480p", "360p"]
        );
    }

    #[test]
    fn software_encoder_shares_half_the_host_threads_across_renditions() {
        assert_eq!(software_threads_per_rendition(8, 3), 1);
        assert_eq!(software_threads_per_rendition(16, 3), 2);
        assert_eq!(software_threads_per_rendition(4, 1), 2);
        assert_eq!(software_threads_per_rendition(1, 3), 1);
    }

    #[test]
    fn videotoolbox_detection_matches_an_encoder_field_only() {
        assert!(encoder_listing_has_videotoolbox(
            b" V....D h264_videotoolbox VideoToolbox H.264 Encoder\n"
        ));
        assert!(!encoder_listing_has_videotoolbox(
            b"description mentions h264_videotoolbox, but it is not an encoder field\n"
        ));
    }

    #[test]
    fn low_height_widescreen_video_still_gets_the_lowest_hls_rendition() {
        let prefs = preferences();

        let variants = video_variants(288, &prefs, u64::MAX, 0);

        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].name, "360p");
    }

    #[test]
    fn hls_paths_cannot_escape_the_session_directory() {
        assert!(safe_hls_path("v0/segment_000001.m4s"));
        assert!(!safe_hls_path("../secret"));
        assert!(!safe_hls_path("v0//segment.m4s"));
        assert!(!safe_hls_path("/absolute"));
        assert!(!safe_hls_path("ffmpeg.log"));
    }

    #[test]
    fn hls_output_signature_tracks_encoder_progress_but_ignores_the_log() {
        let root = std::env::temp_dir().join(format!("swarm-sig-{}", session_id()));
        let rendition = root.join("v360p");
        std::fs::create_dir_all(&rendition).unwrap();

        let empty = hls_output_signature(&root);

        // FFmpeg's own text log must not read as transcode progress — a
        // stalled decoder that keeps logging would otherwise look alive.
        std::fs::write(root.join("ffmpeg.log"), "warning: something\n").unwrap();
        assert_eq!(hls_output_signature(&root), empty);

        // A freshly written segment (in a nested rendition dir) is progress.
        std::fs::write(rendition.join("segment_000000.m4s"), b"abcd").unwrap();
        let after_first = hls_output_signature(&root);
        assert_ne!(after_first, empty);

        // So is that segment growing, or a second one landing.
        std::fs::write(rendition.join("segment_000000.m4s"), b"abcdefgh").unwrap();
        assert_ne!(hls_output_signature(&root), after_first);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finalize_hls_playlists_terminates_open_ended_media_playlists() {
        let root = std::env::temp_dir().join(format!("swarm-endlist-{}", session_id()));
        let open_ended = root.join("v1080p");
        let already_ended = root.join("v360p");
        let header_only = root.join("vaudio");
        std::fs::create_dir_all(&open_ended).unwrap();
        std::fs::create_dir_all(&already_ended).unwrap();
        std::fs::create_dir_all(&header_only).unwrap();

        let open_playlist = open_ended.join("index.m3u8");
        std::fs::write(
            &open_playlist,
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.000,\nsegment_000000.m4s\n",
        )
        .unwrap();
        let ended_playlist = already_ended.join("index.m3u8");
        let ended_body =
            "#EXTM3U\n#EXTINF:4.000,\nsegment_000000.m4s\n#EXT-X-ENDLIST\n".to_string();
        std::fs::write(&ended_playlist, &ended_body).unwrap();
        let header_playlist = header_only.join("index.m3u8");
        let header_body = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n".to_string();
        std::fs::write(&header_playlist, &header_body).unwrap();

        finalize_hls_playlists(&root);
        finalize_hls_playlists(&root);

        let patched = std::fs::read_to_string(&open_playlist).unwrap();
        assert!(patched.trim_end().ends_with("#EXT-X-ENDLIST"));
        assert_eq!(patched.matches("#EXT-X-ENDLIST").count(), 1);
        assert_eq!(std::fs::read_to_string(&ended_playlist).unwrap(), ended_body);
        assert_eq!(std::fs::read_to_string(&header_playlist).unwrap(), header_body);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn reservations_share_one_upload_budget() {
        let root = std::env::temp_dir().join(format!("swarm-budget-test-{}", session_id()));
        let manager = TranscodeManager::new(TranscodeConfig {
            enabled: false,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.clone(),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 3,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        });
        assert_eq!(manager.global_rate_limiter().rate_bps(), 7_000_000);
        let first = manager
            .reserve(
                SessionKind::Direct {
                    entry_key: "a".into(),
                },
                4_160_000,
                true,
                false,
                None,
                false,
            )
            .unwrap();
        let second = manager
            .reserve(
                SessionKind::Direct {
                    entry_key: "b".into(),
                },
                2_128_000,
                true,
                false,
                None,
                false,
            )
            .unwrap();
        assert_eq!(manager.reserved_bps(), 6_288_000);
        assert!(matches!(
            manager.reserve(
                SessionKind::Direct {
                    entry_key: "c".into()
                },
                1_096_000,
                true,
                false,
                None,
                false,
            ),
            Err(TranscodeError::Bandwidth)
        ));
        assert_eq!(manager.open_direct(&first).unwrap().1.rate_bps(), 4_160_000);
        manager.finish_use(&first);
        assert_eq!(
            manager.open_direct(&second).unwrap().1.rate_bps(),
            2_128_000
        );
        manager.finish_use(&second);
        drop(manager);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn activity_snapshot_separates_direct_sessions_from_transcodes() {
        let root = std::env::temp_dir().join(format!("swarm-activity-test-{}", session_id()));
        let manager = TranscodeManager::new(TranscodeConfig {
            enabled: false,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.clone(),
            max_upload_bps: 20_000_000,
            reserve_percent: 0,
            max_sessions: 3,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        });
        assert_eq!(manager.activity(), TranscodeActivity::default());

        manager
            .reserve(
                SessionKind::Direct {
                    entry_key: "a".into(),
                },
                3_000_000,
                true,
                false,
                None,
                false,
            )
            .unwrap();
        let activity = manager.activity();
        assert_eq!(activity.direct_sessions, 1);
        assert_eq!(activity.transcode_sessions, 0);
        assert_eq!(activity.reserved_bps, 3_000_000);

        drop(manager);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ffmpeg_hls_pipeline_smoke_test_when_ffmpeg_is_available() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("swarm-hls-smoke-{}", session_id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        let generated = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1280x720:rate=30:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                "-y",
            ])
            .arg(&source)
            .status()
            .await
            .unwrap();
        if !generated.success() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        };
        let manager = TranscodeManager::new(config);
        let mut source_entry = entry();
        source_entry.relative_path = "source.mp4".into();
        source_entry.size = source.metadata().unwrap().len();
        source_entry.duration_secs = Some(2.0);
        source_entry.video.as_mut().unwrap().width = 1280;
        source_entry.video.as_mut().unwrap().height = 720;
        source_entry.video.as_mut().unwrap().bitrate = Some(3_000_000);
        source_entry.audio.as_mut().unwrap().bitrate = Some(96_000);
        let mut prefs = preferences();
        prefs.prefer_direct = false;

        let plan = manager
            .plan(&source_entry, &source, &prefs, false, None)
            .await
            .unwrap();
        assert_eq!(plan.mode, PlaybackMode::Hls);
        let relative = plan.path.splitn(4, '/').nth(3).unwrap();
        let session = plan.path.split('/').nth(2).unwrap();
        let file = manager.open_hls(session, relative).unwrap();
        let master = std::fs::read_to_string(file.path).unwrap();
        assert!(master.contains("#EXTM3U"));
        assert!(master.contains("index.m3u8"));
        assert_eq!(master.matches("#EXT-X-STREAM-INF").count(), 3);
        manager.finish_use(session);

        manager.release(session);
        let lan_plan = manager
            .plan(&source_entry, &source, &prefs, true, None)
            .await
            .unwrap();
        let lan_relative = lan_plan.path.splitn(4, '/').nth(3).unwrap();
        let lan_session = lan_plan.path.split('/').nth(2).unwrap();
        let lan_file = manager.open_hls(lan_session, lan_relative).unwrap();
        let lan_master = std::fs::read_to_string(&lan_file.path).unwrap();
        assert_eq!(lan_master.matches("#EXT-X-STREAM-INF").count(), 1);
        // The source is H.264 the client can already decode — on LAN it is
        // remuxed (`-c:v copy`, single "source" rung), not re-encoded, so real
        // ffmpeg has to accept the copy command and flush a playable segment.
        assert!(
            lan_master.contains("vsource/index.m3u8"),
            "LAN plan for a client-compatible codec must remux, not transcode: {lan_master}"
        );
        let lan_source_playlist = std::fs::read_to_string(
            lan_file.path.parent().unwrap().join("vsource/index.m3u8"),
        )
        .unwrap();
        assert!(
            lan_source_playlist.contains("#EXTINF"),
            "real ffmpeg must accept `-c:v copy` and flush a segment"
        );
        manager.finish_use(lan_session);
        manager.release(lan_session);

        prefs.preview = true;
        let preview_plan = manager
            .plan(&source_entry, &source, &prefs, false, None)
            .await
            .unwrap();
        assert_eq!(preview_plan.mode, PlaybackMode::Hls);
        assert_eq!(preview_plan.max_bitrate, 1_496_000);
        let preview_relative = preview_plan.path.splitn(4, '/').nth(3).unwrap();
        let preview_session = preview_plan.path.split('/').nth(2).unwrap();
        let preview_file = manager.open_hls(preview_session, preview_relative).unwrap();
        let preview_master = std::fs::read_to_string(preview_file.path).unwrap();
        assert!(preview_master.contains("preview-540p/index.m3u8"));
        assert_eq!(preview_master.matches("#EXT-X-STREAM-INF").count(), 1);
        manager.finish_use(preview_session);
        drop(manager);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression coverage for the #55 follow-up ("6 audio tracks and 4
    /// subtitle tracks, but the player only offers Auto/Unknown") — the
    /// single-audio-stream smoke test above can't catch a `var_stream_map`
    /// syntax mistake or a missing per-track output directory, since ffmpeg
    /// is happy either way when there is only one track to map. This drives
    /// spawn_ffmpeg with a source that actually has two, so a real ffmpeg
    /// binary has to accept the multi-track command and produce a
    /// selectable rendition per language.
    #[tokio::test]
    async fn ffmpeg_hls_master_playlist_exposes_every_embedded_audio_track() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("swarm-hls-multi-audio-{}", session_id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        let generated = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=220:duration=2",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:a:1",
                "language=jpn",
                "-shortest",
                "-y",
            ])
            .arg(&source)
            .status()
            .await
            .unwrap();
        if !generated.success() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        };
        let manager = TranscodeManager::new(config);
        let mut source_entry = entry();
        source_entry.relative_path = "source.mp4".into();
        source_entry.size = source.metadata().unwrap().len();
        source_entry.duration_secs = Some(2.0);
        source_entry.video.as_mut().unwrap().width = 640;
        source_entry.video.as_mut().unwrap().height = 360;
        source_entry.video.as_mut().unwrap().bitrate = Some(900_000);
        source_entry.audio.as_mut().unwrap().bitrate = Some(96_000);
        let mut prefs = preferences();
        prefs.prefer_direct = false;

        let plan = manager
            .plan(&source_entry, &source, &prefs, false, None)
            .await
            .unwrap();
        assert_eq!(plan.mode, PlaybackMode::Hls);
        let relative = plan.path.splitn(4, '/').nth(3).unwrap();
        let session = plan.path.split('/').nth(2).unwrap();
        let file = manager.open_hls(session, relative).unwrap();
        let master = std::fs::read_to_string(&file.path).unwrap();

        assert_eq!(master.matches("#EXT-X-MEDIA:TYPE=AUDIO").count(), 2);
        let eng_line = master
            .lines()
            .find(|line| line.contains("LANGUAGE=\"eng\""))
            .expect("English audio rendition missing from master playlist");
        assert!(
            eng_line.contains("DEFAULT=YES"),
            "English should win the same preferred-track tie-break used for direct play/single-track HLS: {eng_line}"
        );
        let jpn_line = master
            .lines()
            .find(|line| line.contains("LANGUAGE=\"jpn\""))
            .expect("Japanese audio rendition missing from master playlist");
        assert!(jpn_line.contains("DEFAULT=NO"), "{jpn_line}");

        // Prove ffmpeg actually produced a real, independently selectable
        // rendition for each track, not just a playlist that references one.
        let session_dir = root.join("sessions").join(session);
        assert!(session_dir.join("veng").join("index.m3u8").is_file());
        assert!(session_dir.join("vjpn").join("index.m3u8").is_file());

        manager.finish_use(session);
        drop(manager);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression coverage for #203 ("movie is playing but there is no
    /// audio"). A library scanned while ffprobe was unreachable (macOS GUI
    /// `PATH`) stores entries with no audio summary at all; the HLS session
    /// must still re-probe the real file and map its sound instead of
    /// trusting that empty summary and shipping a video-only playlist.
    #[tokio::test]
    async fn hls_maps_audio_even_when_the_scan_recorded_no_audio_stream() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("swarm-hls-203-{}", session_id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        let generated = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                "-y",
            ])
            .arg(&source)
            .status()
            .await
            .unwrap();
        if !generated.success() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        };
        let manager = TranscodeManager::new(config);
        let mut source_entry = entry();
        source_entry.relative_path = "source.mp4".into();
        source_entry.size = source.metadata().unwrap().len();
        source_entry.duration_secs = Some(2.0);
        source_entry.video.as_mut().unwrap().width = 640;
        source_entry.video.as_mut().unwrap().height = 360;
        source_entry.video.as_mut().unwrap().bitrate = Some(900_000);
        // The scan never reached ffprobe, so it stored nothing about the audio.
        source_entry.audio = None;
        let mut prefs = preferences();
        prefs.prefer_direct = false;

        let plan = manager
            .plan(&source_entry, &source, &prefs, false, None)
            .await
            .unwrap();
        assert_eq!(plan.mode, PlaybackMode::Hls);
        let relative = plan.path.splitn(4, '/').nth(3).unwrap();
        let session = plan.path.split('/').nth(2).unwrap();
        let file = manager.open_hls(session, relative).unwrap();
        let master = std::fs::read_to_string(&file.path).unwrap();
        assert!(
            master.contains("#EXT-X-MEDIA:TYPE=AUDIO"),
            "HLS master must still carry an audio rendition: {master}"
        );
        // The sine track carries no language tag, so `audio_map_entries` names
        // it "und" — prove ffmpeg actually produced that rendition on disk.
        let session_dir = root.join("sessions").join(session);
        assert!(
            session_dir.join("vund").join("index.m3u8").is_file(),
            "an audio rendition playlist should exist on disk"
        );

        manager.finish_use(session);
        drop(manager);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression coverage for #226 ("audio is off on some video previews"). A
    /// mid-file preview seek that re-encodes video while stream-copying an
    /// AC-3 5.1 track used to hand ffmpeg a single input `-ss`, which rebased
    /// the re-encoded video to the previous keyframe but the copied audio to
    /// the exact seek point — leaving the preview several seconds out of sync.
    /// The split input+output seek must line the two tracks back up.
    #[tokio::test]
    async fn preview_mid_file_seek_keeps_copied_audio_in_sync() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("swarm-hls-226-{}", session_id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mkv");
        // ~4-second GOP so a non-keyframe seek target lands well past the
        // keyframe ffmpeg would fast-seek to, and AC-3 5.1 audio so the client
        // baseline (which lists `ac3`) takes the stream-copy delivery path.
        let generated = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1280x720:rate=24:duration=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=30",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "96",
                "-keyint_min",
                "96",
                "-sc_threshold",
                "0",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "ac3",
                "-b:a",
                "448k",
                "-ac",
                "6",
                "-shortest",
                "-y",
            ])
            .arg(&source)
            .status()
            .await
            .unwrap();
        if !generated.success() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        };
        let manager = TranscodeManager::new(config);
        let mut source_entry = entry();
        source_entry.relative_path = "source.mkv".into();
        source_entry.size = source.metadata().unwrap().len();
        source_entry.duration_secs = Some(30.0);
        source_entry.video.as_mut().unwrap().width = 1280;
        source_entry.video.as_mut().unwrap().height = 720;
        source_entry.video.as_mut().unwrap().bitrate = Some(3_000_000);
        source_entry.audio = Some(AudioStreamInfo {
            codec: "ac3".into(),
            channels: 6,
            bitrate: Some(448_000),
        });
        let mut prefs = preferences();
        prefs.prefer_direct = false;
        prefs.preview = true;
        // A non-keyframe offset roughly a full GOP past the previous keyframe.
        prefs.start_position_secs = 15;

        let plan = manager
            .plan(&source_entry, &source, &prefs, false, None)
            .await
            .unwrap();
        assert_eq!(plan.mode, PlaybackMode::Hls);
        let session = plan.path.split('/').nth(2).unwrap();
        let session_dir = root.join("sessions").join(session);

        let first_pts = |dir: &str| -> f64 {
            let playlist = std::fs::read_to_string(session_dir.join(dir).join("index.m3u8"))
                .unwrap_or_else(|_| panic!("missing {dir}/index.m3u8"));
            let init = playlist
                .lines()
                .find_map(|line| line.strip_prefix("#EXT-X-MAP:URI=\""))
                .and_then(|rest| rest.split('"').next())
                .expect("playlist has no #EXT-X-MAP");
            let segment = playlist
                .lines()
                .find(|line| line.ends_with(".m4s"))
                .expect("playlist has no media segment");
            let mut bytes = std::fs::read(session_dir.join(dir).join(init)).unwrap();
            bytes.extend(std::fs::read(session_dir.join(dir).join(segment)).unwrap());
            let probe_file = session_dir.join(format!("{dir}-probe.mp4"));
            std::fs::write(&probe_file, &bytes).unwrap();
            let output = std::process::Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "packet=pts_time",
                    "-of",
                    "csv=p=0",
                    "-read_intervals",
                    "%+#1",
                ])
                .arg(&probe_file)
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout)
                .split([',', '\n'])
                .find_map(|field| field.trim().parse::<f64>().ok())
                .expect("ffprobe reported no packet timestamp")
        };

        let video_start = first_pts("vpreview-540p");
        let audio_start = first_pts("vund");
        assert!(
            (video_start - audio_start).abs() < 0.5,
            "preview audio and video must start together: video={video_start} audio={audio_start}"
        );

        manager.finish_use(session);
        drop(manager);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression coverage for #191 ("previews and videos should default to
    /// english"). A direct-playable container with several embedded audio
    /// tracks is only handed straight to the client when English is the track
    /// a player would pick anyway; otherwise planning must fall through to
    /// HLS, where the server pins English as `DEFAULT=YES`.
    #[tokio::test]
    async fn direct_play_is_declined_when_english_audio_is_not_the_first_track() {
        if Command::new("ffprobe")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("swarm-direct-en-{}", session_id()));
        std::fs::create_dir_all(&root).unwrap();

        // Two title-tagged audio tracks whose formal language is `und`, like
        // the Daria files from #278. `english_first` controls container order.
        let build_source = |name: &str, english_first: bool| {
            let path = root.join(name);
            let (title0, title1) = if english_first {
                ("English", "Spanish")
            } else {
                ("Spanish", "English")
            };
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x180:rate=30:duration=1",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=1",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=220:duration=1",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-map",
                    "2:a",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-metadata:s:a:0",
                    "language=und",
                    "-metadata:s:a:1",
                    "language=und",
                    "-metadata:s:a:0",
                    &format!("title={title0}"),
                    "-metadata:s:a:1",
                    &format!("title={title1}"),
                    "-shortest",
                    "-y",
                ])
                .arg(&path)
                .status()
                .unwrap();
            status.success().then_some(path)
        };

        let (Some(english_second), Some(english_first)) = (
            build_source("english_second.mp4", false),
            build_source("english_first.mp4", true),
        ) else {
            let _ = std::fs::remove_dir_all(&root);
            return;
        };

        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 100_000_000,
            reserve_percent: 0,
            max_sessions: 2,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        };
        let manager = TranscodeManager::new(config);

        let mut source_entry = entry();
        source_entry.relative_path = "movie.mp4".into();
        source_entry.duration_secs = Some(1.0);
        source_entry.video.as_mut().unwrap().width = 320;
        source_entry.video.as_mut().unwrap().height = 180;
        source_entry.video.as_mut().unwrap().bitrate = Some(200_000);
        source_entry.audio.as_mut().unwrap().bitrate = Some(96_000);
        let prefs = preferences(); // prefer_direct = true

        source_entry.size = english_second.metadata().unwrap().len();
        let declined = manager
            .plan(&source_entry, &english_second, &prefs, true, None)
            .await
            .unwrap();
        assert_eq!(
            declined.mode,
            PlaybackMode::Hls,
            "English is the second audio track — direct play would start in Spanish"
        );
        let declined_relative = declined.path.splitn(4, '/').nth(3).unwrap();
        let declined_file = manager
            .open_hls(&declined.session_id, declined_relative)
            .unwrap();
        let declined_master = std::fs::read_to_string(declined_file.path).unwrap();
        assert_eq!(declined_master.matches("#EXT-X-MEDIA:TYPE=AUDIO").count(), 2);
        let english = declined_master
            .lines()
            .find(|line| line.contains("LANGUAGE=\"eng\""))
            .expect("title-tagged English audio missing from HLS master");
        assert!(english.contains("DEFAULT=YES"), "{english}");
        assert!(
            declined_master.lines().any(|line| line.contains("LANGUAGE=\"spa\"")),
            "title-tagged Spanish audio missing from HLS master: {declined_master}"
        );

        source_entry.size = english_first.metadata().unwrap().len();
        let direct = manager
            .plan(&source_entry, &english_first, &prefs, true, None)
            .await
            .unwrap();
        assert_eq!(
            direct.mode,
            PlaybackMode::Direct,
            "English is already the first audio track — direct play is safe"
        );

        drop(manager);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Exact #278 shape: two supported audio streams with no language or
    /// title metadata. Even when the container is otherwise direct-playable,
    /// route it through HLS and prove both anonymous streams become distinct
    /// renditions. The TV labels these `Audio 1` and `Audio 2`.
    #[tokio::test]
    async fn untagged_multi_audio_is_hls_with_every_track_exposed() {
        if Command::new("ffprobe")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("swarm-untagged-audio-{}", session_id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("episode.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=220:duration=1",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-disposition:a:0",
                "default",
                "-disposition:a:1",
                "0",
                "-shortest",
                "-y",
            ])
            .arg(&path)
            .status()
            .unwrap();
        if !status.success() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let manager = TranscodeManager::new(TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 100_000_000,
            reserve_percent: 0,
            max_sessions: 2,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        });
        let mut source_entry = entry();
        source_entry.relative_path = "episode.mp4".into();
        source_entry.duration_secs = Some(1.0);
        source_entry.video.as_mut().unwrap().width = 320;
        source_entry.video.as_mut().unwrap().height = 180;
        source_entry.video.as_mut().unwrap().bitrate = Some(200_000);
        source_entry.audio.as_mut().unwrap().bitrate = Some(96_000);
        source_entry.size = path.metadata().unwrap().len();

        let plan = manager
            .plan(&source_entry, &path, &preferences(), true, None)
            .await
            .unwrap();
        assert_eq!(plan.mode, PlaybackMode::Hls);
        let relative = plan.path.splitn(4, '/').nth(3).unwrap();
        let master_file = manager.open_hls(&plan.session_id, relative).unwrap();
        let master = std::fs::read_to_string(master_file.path).unwrap();
        assert_eq!(
            master.matches("#EXT-X-MEDIA:TYPE=AUDIO").count(),
            2,
            "both anonymous audio streams must reach the player: {master}"
        );
        assert!(master.contains("NAME=\"audio_1\""), "{master}");
        assert!(master.contains("NAME=\"audio_2\""), "{master}");
        assert!(master.contains("URI=\"vund/index.m3u8\""), "{master}");
        assert!(master.contains("URI=\"vund1/index.m3u8\""), "{master}");

        manager.finish_use(&plan.session_id);
        drop(manager);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Direct play must also fall back to HLS when a *secondary* audio track
    /// uses a codec the client cannot decode at all. [direct_compatible] only
    /// checks the scan-time summary's single first-track codec, so an English
    /// track that is first (passing the language gate above) but encoded in a
    /// codec the client never advertises would otherwise reach the client's
    /// own demuxer, get marked unsupported by its `Tracks` API, and vanish
    /// from the audio picker with no error — the exact "Audio 1 shows, Audio
    /// 2 doesn't" follow-up reported for a Daria rip after #278's first pass
    /// only fixed language tagging, not codec support.
    #[tokio::test]
    async fn direct_play_is_declined_when_a_secondary_audio_track_codec_is_unsupported() {
        if Command::new("ffprobe")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("swarm-direct-codec-{}", session_id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("movie.mp4");
        // Track 0 (English, first, default) is AAC — the language gate alone
        // would call this file safe for direct play. Track 1 (Spanish) is
        // FLAC, a codec `CapabilityProfile::fire_tv_baseline` never lists.
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=220:duration=1",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a:0",
                "aac",
                "-c:a:1",
                "flac",
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:a:1",
                "language=spa",
                "-shortest",
                "-y",
            ])
            .arg(&path)
            .status()
            .unwrap();
        if !status.success() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let config = TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 100_000_000,
            reserve_percent: 0,
            max_sessions: 2,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        };
        let manager = TranscodeManager::new(config);

        let mut source_entry = entry();
        source_entry.relative_path = "movie.mp4".into();
        source_entry.duration_secs = Some(1.0);
        source_entry.video.as_mut().unwrap().width = 320;
        source_entry.video.as_mut().unwrap().height = 180;
        source_entry.video.as_mut().unwrap().bitrate = Some(200_000);
        source_entry.audio.as_mut().unwrap().bitrate = Some(96_000);
        source_entry.size = path.metadata().unwrap().len();
        let prefs = preferences(); // prefer_direct = true, fire_tv_baseline codecs

        let plan = manager
            .plan(&source_entry, &path, &prefs, true, None)
            .await
            .unwrap();
        assert_eq!(
            plan.mode,
            PlaybackMode::Hls,
            "second track is FLAC, which the client's capability profile can't decode — direct play would hide it from the picker"
        );

        drop(manager);
        let _ = std::fs::remove_dir_all(&root);
    }
}
