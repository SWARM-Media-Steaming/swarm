//! Device ↔ device application protocol, carried over hole-punched QUIC.
//!
//! One request per QUIC bidirectional stream: the initiator writes a single
//! JSON line ([`PeerRequest`]), the responder writes a JSON line
//! ([`PeerResponseHeader`]) followed by the raw body bytes. Deliberately
//! HTTP-shaped so the loopback proxies in the TV client and the Tauri app are
//! dumb translators.

use crate::capability::CapabilityProfile;
use serde::{Deserialize, Serialize};

/// Request header line. `path` uses the fixed peer route vocabulary:
/// `/catalog/thumbprint`, `/catalog/manifest[.gz]`, `/art/{entry_key}/{kind}`,
/// `/play/{entry_key}` (playback negotiation), `/stream/{session}/media`
/// (budgeted direct play), `/hls/{session}/...` (transcode),
/// `/errors/report` (client-observed error triage — see
/// [`ClientErrorReport`]), `/notifications/{device_id}[/{error_id}/dismiss]`
/// (resolved-problem delivery), and `/likes/toggle` (see [`LikeToggle`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRequest {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ByteRange>,
    /// Entity tag for conditional catalog/artwork fetches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_match: Option<String>,
    /// Present only on `/play/{entry_key}`. Keeping playback negotiation in
    /// the authenticated peer plane means the media server can make the
    /// direct/remux/transcode decision without trusting the rendezvous tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback: Option<PlaybackPreferences>,
    /// Present only on `/errors/report`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_report: Option<ClientErrorReport>,
    /// Present only on `/likes/toggle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub like: Option<LikeToggle>,
}

/// A device's like/unlike of one asset, reported over the same authenticated
/// peer connection `ClientErrorReport` rides in on — same self-reported-
/// identity trust model, same "carried as a field on `PeerRequest`" shape.
/// `liked` carries the *desired end state* (not "flip whatever it currently
/// is"), so a D-pad button retry after a dropped response is naturally
/// idempotent — see `Library::set_like`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LikeToggle {
    pub device_id: String,
    pub device_name: String,
    pub entry_key: String,
    pub liked: bool,
}

/// A client-observed error (playback failure, unreachable server, etc.),
/// reported back over the same authenticated peer connection so it can be
/// triaged from the server's own swarm page instead of only ever being
/// visible in on-device logs nobody's looking at. `POST`-shaped despite the
/// otherwise GET-shaped peer route vocabulary — carried as a field on
/// [`PeerRequest`] rather than a raw body, matching how `playback` already
/// rides along on `/play/{entry_key}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientErrorReport {
    pub device_id: String,
    pub device_name: String,
    /// The asset involved, when the error is tied to one (playback
    /// failures) rather than general (catalog unreachable, registration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub message: String,
    /// Free-form extra detail for triage — HTTP status, stack trace, the
    /// URL/path involved, etc. Not structured further: what's useful varies
    /// too much by error source to be worth a schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Client-observed wall-clock time, unix milliseconds.
    pub occurred_at_ms: i64,
}

/// What a player can consume and where it wants playback to begin. The
/// server intersects this with its own shared upload budget before returning
/// a [`PlaybackPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackPreferences {
    pub capabilities: CapabilityProfile,
    #[serde(default)]
    pub start_position_secs: u64,
    #[serde(default = "default_true")]
    pub prefer_direct: bool,
    /// Hover/browse previews always use the server's lightweight, short-lived
    /// HLS profile instead of reserving or exposing the full source stream.
    #[serde(default)]
    pub preview: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    Direct,
    Hls,
}

/// Cached lyrics attached only to music playback negotiation. Keeping this
/// out of the catalog manifest avoids making every catalog sync carry a
/// library's worth of lyric text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackLyrics {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_lyrics: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_lyrics: Option<String>,
    #[serde(default)]
    pub instrumental: bool,
}

/// One side-loaded subtitle track available for this playback session.
/// `path` is an authenticated peer path; clients route it through their
/// existing loopback proxy just like the media URL itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleTrack {
    pub id: String,
    pub language: String,
    pub label: String,
    pub source: String,
    pub path: String,
}

/// Body returned by `/play/{entry_key}`. `path` is another peer path, not a
/// public URL; the client feeds it through its authenticated loopback proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackPlan {
    pub mode: PlaybackMode,
    pub path: String,
    /// Hard server-side allocation for this session, including audio, in
    /// bits/sec. The client also uses it as its conservative ABR ceiling.
    pub max_bitrate: u64,
    /// Same id embedded in `path` (`/stream/{id}/...` or `/hls/{id}/...`),
    /// surfaced explicitly so the client can release this session's
    /// bandwidth reservation via `/stop/{id}` without parsing `path`.
    pub session_id: String,
    /// Present for music when the server has cached an LRCLIB result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<TrackLyrics>,
    /// Completed side-loaded subtitle tracks. Partial transcription output
    /// is never included here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitles: Vec<SubtitleTrack>,
}

/// HTTP-style byte range. `Suffix(n)` = last `n` bytes (`bytes=-n`);
/// `FromTo` end is inclusive and `None` means "to end of file".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteRange {
    FromTo { start: u64, end: Option<u64> },
    Suffix { last: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerResponseHeader {
    /// HTTP-alike status: 200, 206, 304, 404, 416, 500.
    pub status: u16,
    /// Body length that follows this header line; 0 for bodyless statuses.
    pub len: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Set on 206: the satisfied range and the full entity size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_range: Option<ContentRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRange {
    pub start: u64,
    /// Inclusive.
    pub end: u64,
    pub total: u64,
}

/// `GET /catalog/thumbprint` body — the whole-library version token. A client
/// re-syncs only when it changes (Batocera.Drone's thumbprint pattern).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogThumbprint {
    pub thumbprint: String,
    pub entry_count: u64,
}

/// `GET /catalog/manifest` body. Full listing today; `removed` supports the
/// delta form (`?since=<thumbprint>`) once servers track per-thumbprint
/// changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogManifest {
    pub thumbprint: String,
    pub entries: Vec<CatalogEntry>,
    #[serde(default)]
    pub removed: Vec<String>,
    /// A change response could not be based on the client's version and is
    /// therefore a complete replacement rather than a delta.
    #[serde(default)]
    pub reset: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Movie,
    Episode,
    Track,
}

/// A community-sourced playback segment from TheIntroDB. The same wire
/// shape covers every segment type the service exposes so clients can add
/// recap/credits/preview controls without another catalog migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipSegmentKind {
    Intro,
    Recap,
    Credits,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipSegment {
    pub kind: SkipSegmentKind,
    /// `None` means the segment begins with the media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
    /// `None` means the segment continues to the end of the media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u64>,
}

/// One asset as advertised by a server. Clients merge manifests from all
/// servers keyed on `fingerprint` — the same bytes on two servers collapse
/// into one catalog entry with multiple sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub entry_key: String,
    /// sample-fp-v1 content identity.
    pub fingerprint: String,
    pub kind: MediaKind,
    pub title: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    // Grouping fields are path-derived (never scraped) per the Drone rule;
    // scraped titles arrive as display overlay in `scraped_title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scraped_title: Option<String>,
    /// Scraper overlay for a TV episode/special's own title. Kept separate
    /// from `scraped_title`, which is the canonical show title used to merge
    /// differently named on-disk show folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_title: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoStreamInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioStreamInfo>,
    /// Changes when artwork changes; clients cache art against it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork_etag: Option<String>,
    /// Path/filename-derived release year, if a meaningful one was found —
    /// same path-derived, never-scraped status as the grouping fields above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    /// Scraper overlay, movies/episodes only — empty for tracks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<CastMember>,
    /// Synopsis — TMDb's own, or a manual override. `None` for tracks and
    /// anything not yet scraped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    /// US content rating — TMDb's own, or a manual override. `None` for
    /// tracks, anything not yet scraped, or without a US certification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    /// Provider community score normalized to 0–10. This is deliberately
    /// separate from the parental `rating` certification above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community_rating: Option<f64>,
    /// Provider vote count behind `community_rating`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community_rating_votes: Option<u64>,
    /// Number of distinct devices that currently have this liked
    /// (`entry_likes`, one row per device — see `Library::like_counts`).
    #[serde(default)]
    pub like_count: u32,
    /// IntroDB playback markers cached by the media server during metadata
    /// scraping. Intro is consumed by today's TV UI; the other kinds are
    /// retained now so the database integration does not discard them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_segments: Vec<SkipSegment>,
}

/// One TMDb credits-list entry, capped to roughly the first ten (billing
/// order) at scrape time — display-only, never a grouping key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastMember {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
}

/// ffprobe-derived stream facts captured at scan time; feeds the direct-play
/// decision without touching the file again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
    /// ffprobe `profile` — `"High"`, `"Main 10"`, `"Main"`. Lets the
    /// direct-play/remux check reject a 10-bit stream a client only decodes
    /// in 8-bit. Absent on library rows scanned before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Luma bit depth (`8`, `10`, `12`) derived from the pixel format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    /// `true` when the transfer characteristics are PQ or HLG — the stream
    /// must be tone-mapped for an SDR-only client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub codec: String,
    pub channels: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip_with_range() {
        let req = PeerRequest {
            path: "/media/030fe19c72f2665e6efd018a".into(),
            range: Some(ByteRange::FromTo {
                start: 1024,
                end: None,
            }),
            if_none_match: None,
            playback: None,
            error_report: None,
            like: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<PeerRequest>(&json).unwrap(), req);
    }

    #[test]
    fn like_toggle_roundtrip() {
        let req = PeerRequest {
            path: "/likes/toggle".into(),
            range: None,
            if_none_match: None,
            playback: None,
            error_report: None,
            like: Some(LikeToggle {
                device_id: "device-1".into(),
                device_name: "Living Room TV".into(),
                entry_key: "030fe19c72f2665e6efd018a".into(),
                liked: true,
            }),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<PeerRequest>(&json).unwrap(), req);
    }

    #[test]
    fn suffix_range_roundtrip() {
        let json = serde_json::to_string(&ByteRange::Suffix { last: 500 }).unwrap();
        assert_eq!(
            serde_json::from_str::<ByteRange>(&json).unwrap(),
            ByteRange::Suffix { last: 500 }
        );
    }

    #[test]
    fn playback_negotiation_roundtrip() {
        let request = PeerRequest {
            path: "/play/030fe19c72f2665e6efd018a".into(),
            range: None,
            if_none_match: None,
            playback: Some(PlaybackPreferences {
                capabilities: crate::capability::CapabilityProfile::fire_tv_baseline(),
                start_position_secs: 42,
                prefer_direct: true,
                preview: false,
            }),
            error_report: None,
            like: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<PeerRequest>(&json).unwrap(), request);

        let plan = PlaybackPlan {
            mode: PlaybackMode::Hls,
            path: "/hls/session/master.m3u8".into(),
            max_bitrate: 4_160_000,
            session_id: "session".into(),
            lyrics: Some(TrackLyrics {
                provider: "lrclib".into(),
                provider_id: Some(42),
                language: Some("en".into()),
                plain_lyrics: Some("First line".into()),
                synced_lyrics: Some("[00:01.00]First line".into()),
                instrumental: false,
            }),
            subtitles: vec![SubtitleTrack {
                id: "whisper-en".into(),
                language: "en".into(),
                label: "English — AI generated".into(),
                source: "whisper".into(),
                path: "/subtitles/entry/whisper-en.vtt".into(),
            }],
        };
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<PlaybackPlan>(&json).unwrap(), plan);
    }

    #[test]
    fn manifest_roundtrip() {
        let manifest = CatalogManifest {
            thumbprint: "ab".repeat(32),
            entries: vec![CatalogEntry {
                entry_key: "030fe19c72f2665e6efd018a".into(),
                fingerprint: "704ac5a4284267953aab77855e0e32aa".into(),
                kind: MediaKind::Movie,
                title: "Inception".into(),
                size: 4_700_000_000,
                duration_secs: Some(8880.0),
                show_title: None,
                season: None,
                episode: None,
                artist: None,
                album: None,
                track_number: None,
                scraped_title: Some("Inception (2010)".into()),
                episode_title: None,
                genres: vec!["Sci-Fi".into()],
                video: Some(VideoStreamInfo {
                    codec: "h264".into(),
                    width: 1920,
                    height: 1080,
                    level: Some("4.1".into()),
                    bitrate: Some(8_000_000),
                    ..Default::default()
                }),
                audio: Some(AudioStreamInfo {
                    codec: "aac".into(),
                    channels: 6,
                    bitrate: None,
                }),
                artwork_etag: Some("v1".into()),
                year: Some(2010),
                cast: vec![
                    CastMember {
                        name: "Leonardo DiCaprio".into(),
                        character: Some("Cobb".into()),
                    },
                    CastMember {
                        name: "Ellen Page".into(),
                        character: None,
                    },
                ],
                overview: Some(
                    "A thief who steals corporate secrets through dream-sharing technology.".into(),
                ),
                rating: Some("PG-13".into()),
                community_rating: Some(8.4),
                community_rating_votes: Some(36_000),
                like_count: 3,
                skip_segments: vec![SkipSegment {
                    kind: SkipSegmentKind::Intro,
                    start_ms: Some(30_000),
                    end_ms: Some(90_000),
                }],
            }],
            removed: vec![],
            reset: false,
        };
        let json = serde_json::to_string(&manifest).unwrap();
        assert_eq!(
            serde_json::from_str::<CatalogManifest>(&json).unwrap(),
            manifest
        );
    }

    #[test]
    fn response_header_serializes_206() {
        let header = PeerResponseHeader {
            status: 206,
            len: 1024,
            content_type: Some("video/x-matroska".into()),
            content_range: Some(ContentRange {
                start: 0,
                end: 1023,
                total: 4_700_000_000,
            }),
            etag: None,
        };
        let json = serde_json::to_string(&header).unwrap();
        assert_eq!(
            serde_json::from_str::<PeerResponseHeader>(&json).unwrap(),
            header
        );
    }
}
