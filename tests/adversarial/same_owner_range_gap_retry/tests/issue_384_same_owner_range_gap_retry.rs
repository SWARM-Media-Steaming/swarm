//! Issue #384: "Same-owner /play 404s an already-claimed episode/movie
//! stream" — a second angle on the same symptom, distinct from the
//! continuously-open-body scenario already covered in
//! `tests/adversarial/same_entry_crash_retry`.
//!
//! `TranscodeManager::cancel_stale_claimed_for_owner` (crates/swarm-media/src
//! /transcode.rs) distinguishes a crashed claim from a live one purely by
//! `session.in_use == 0`. `in_use` is incremented for the duration of a
//! single `/stream/{id}/media` request's body and decremented back to zero
//! the instant that body finishes — including on ordinary, successful
//! completion, not only on abandonment. The `SessionGuard` docs in
//! `crates/swarm-media/src/serve.rs` say this explicitly: a client "seeking
//! or disconnecting mid-range-request" and finishing one range read is
//! "routine, not exceptional". Real progressive/direct-play HTTP clients
//! (ExoPlayer included) commonly issue a *sequence* of separate ranged GETs
//! against the same session id as playback advances or the user seeks, each
//! one fully completing before the next begins — so `in_use` legitimately
//! drops to 0 between two requests of a single, uninterrupted playback, with
//! no crash involved.
//!
//! Expected behavior, derived from the issue itself (a same-owner `/play`
//! must not 404 a stream the same owner is still watching) and from the
//! domain invariant that an ordinary completed range request is not a
//! crash: a same-owner `/play` negotiation that happens to land in the gap
//! between two range requests of the same still-being-watched session must
//! not sever that session — the next range request against the original
//! session id must keep working (200/206), exactly as it would have without
//! the intervening negotiation.
//!
//! This is not the crash case #389 protects (there `/stop` never arrives at
//! all); here every prior range request completed cleanly and the client is
//! about to issue its next one, which is the normal steady state of an
//! active playback, not an orphan.

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{
    AudioStreamInfo, ByteRange, MediaKind, PeerRequest, PlaybackPreferences, VideoStreamInfo,
};
use swarm_media::serve::{stream_body, Body, MediaService, Resolved};
use swarm_media::store::{EntryRecord, Library};
use swarm_media::transcode::TranscodeConfig;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    media_root: PathBuf,
    library: Arc<Library>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

async fn fixture(tag: &str) -> Fixture {
    let n = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "swarm-adv-384-range-gap-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let library = Arc::new(
        Library::open(base.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    Fixture {
        base,
        media_root,
        library,
    }
}

fn write_file(root: &Path, relative: &str, bytes: &[u8]) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Peak for this fixture is 1_000_000 bps (size*8/duration * 5/4).
fn movie_entry(entry_key: &str, relative_path: &str, title: &str) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: title.into(),
        size: 1_000_000,
        modified_time: 0,
        fingerprint: format!("fp-{entry_key}"),
        artist: None,
        album: None,
        track_number: None,
        show_title: None,
        season: None,
        episode: None,
        year: None,
        duration_secs: Some(10.0),
        video: Some(VideoStreamInfo {
            codec: "h264".into(),
            width: 640,
            height: 360,
            level: Some("4.1".into()),
            bitrate: Some(700_000),
            ..Default::default()
        }),
        audio: Some(AudioStreamInfo {
            codec: "aac".into(),
            channels: 2,
            bitrate: Some(96_000),
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

fn wan_direct_config(base: &Path, max_upload_bps: u64, idle_timeout: Duration) -> TranscodeConfig {
    TranscodeConfig {
        enabled: false,
        ffmpeg_path: "ffmpeg".into(),
        session_dir: base.join("sessions"),
        max_upload_bps,
        reserve_percent: 0,
        max_sessions: 1,
        idle_timeout,
        segment_duration_secs: 4,
        ..Default::default()
    }
}

fn service(fx: &Fixture, config: TranscodeConfig) -> Arc<MediaService> {
    Arc::new(MediaService::with_transcoding(
        Arc::clone(&fx.library),
        fx.media_root.clone(),
        config,
    ))
}

fn play_request(entry_key: &str) -> PeerRequest {
    PeerRequest {
        path: format!("/play/{entry_key}"),
        range: None,
        if_none_match: None,
        playback: Some(PlaybackPreferences {
            capabilities: CapabilityProfile::fire_tv_baseline(),
            start_position_secs: 0,
            prefer_direct: true,
            preview: false,
        }),
        error_report: None,
        like: None,
    }
}

fn ranged_request(path: String, start: u64, end: u64) -> PeerRequest {
    PeerRequest {
        path,
        range: Some(ByteRange::FromTo {
            start,
            end: Some(end),
        }),
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

async fn negotiate(service: &MediaService, entry_key: &str, owner: &str) -> Resolved {
    service
        .resolve_for_peer(&play_request(entry_key), false, "Living room TV", owner)
        .await
}

fn plan_path(resolved: &Resolved) -> String {
    assert_eq!(
        resolved.header.status, 200,
        "playback negotiation must succeed"
    );
    let Body::Bytes(body) = &resolved.body else {
        panic!("playback plan must be JSON");
    };
    let plan: swarm_core::peer::PlaybackPlan = serde_json::from_slice(body).unwrap();
    plan.path
}

/// Issue a single ranged GET against `path` and fully drain its body to
/// completion, exactly as an ordinary progressive-playback client would
/// after successfully reading one buffered chunk. This is the "routine, not
/// exceptional" completion the `SessionGuard` docs describe — it is not a
/// crash and must not be treated as one.
async fn complete_one_range_request(service: &Arc<MediaService>, path: &str, start: u64, end: u64) -> u16 {
    let opened = service
        .resolve(&ranged_request(path.to_string(), start, end))
        .await;
    let status = opened.header.status;
    if status == 200 || status == 206 {
        let body = stream_body(opened, service);
        let mut body = std::pin::pin!(body);
        while let Some(chunk) = body.next().await {
            chunk.expect("chunk read must succeed");
        }
    }
    status
}

/// #384, a second reproduction: a same-owner `/play` retry that lands in the
/// gap between two range requests of one continuous, still-being-watched
/// playback must not sever the session those range requests are using.
#[tokio::test]
async fn same_owner_replay_between_range_requests_of_a_live_stream_does_not_sever_it() {
    let fx = fixture("range-gap").await;
    let movie = movie_entry("0123456789abcdef00000385", "movies/example.mp4", "Example");
    let bytes = vec![7u8; 1_000_000];
    write_file(&fx.media_root, &movie.relative_path, &bytes);
    fx.library.upsert(&movie).await.unwrap();
    // Budget for two direct sessions so the retry's own negotiation cannot
    // be blamed on bandwidth admission.
    let service = service(&fx, wan_direct_config(&fx.base, 2_000_000, Duration::from_secs(300)));
    let owner = "tv-fingerprint";

    let plan_a = negotiate(&service, &movie.entry_key, owner).await;
    let path = plan_path(&plan_a);

    // First ranged read completes cleanly — the ordinary steady state of an
    // active playback between two chunks, not an abandonment.
    let first_status = complete_one_range_request(&service, &path, 0, 99_999).await;
    assert!(
        first_status == 200 || first_status == 206,
        "first ranged read of a freshly negotiated direct session must succeed, got {first_status}"
    );

    // The client is between range requests here — nothing has crashed and
    // no /stop has been sent. A same-owner /play negotiation lands in this
    // window (a transport-level retry, a resumed-from-background app, or
    // simply overlapping requests are all realistic same-owner scenarios).
    let _retry = negotiate(&service, &movie.entry_key, owner).await;

    // The client's *next* range request against the *original* session path
    // is the real-world continuation of the same, uninterrupted playback.
    let second_status = complete_one_range_request(&service, &path, 100_000, 199_999).await;
    assert!(
        second_status == 200 || second_status == 206,
        "issue #384: a same-owner /play negotiated between two range \
         requests of the same ongoing (uncrashed) playback must not sever \
         that playback's session — the next range request on the original \
         session path got {second_status} instead of 200/206"
    );
}
