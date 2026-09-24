//! Issue #412: "Unsatisfiable Range request on a claimed direct-play session
//! leaks its in_use count forever" — found while testing #384: when a direct-
//! play stream receives an unsatisfiable Range request (start past end-of-file),
//! the session is not released and subsequent retries fail with 503 even after
//! idle_timeout elapses.
//!
//! Expected behavior, derived from the issue and from the hls() path's correct
//! handling (serve.rs:1212) — not from the current implementation:
//!
//! 1. An unsatisfiable Range request to a direct-play media stream must return
//!    416 Range Not Satisfiable, which is correct.
//! 2. The session acquired by the initial /play negotiation must be released
//!    by the unsatisfiable Range response, not leaked until idle_timeout.
//! 3. A same-owner retry of the same entry after an unsatisfiable Range
//!    response must succeed immediately, matching the behavior after any other
//!    completed request (successful or error status). Contrast with
//!    `same_entry_crash_retry`: a genuine crash (body dropped, no response
//!    sent) may need to wait for idle_timeout if the session stays claimed;
//!    an unsatisfiable Range is a *complete response* and must release the
//!    session synchronously.

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{AudioStreamInfo, ByteRange, MediaKind, PeerRequest, PlaybackPreferences, VideoStreamInfo};
use swarm_media::serve::{stream_body, Body, MediaService, Resolved};
use swarm_media::store::{EntryRecord, Library};
use swarm_media::transcode::TranscodeConfig;

static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    let n = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "swarm-adv-412-{tag}-{}-{n}",
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

fn unsatisfiable_range_request(path: String) -> PeerRequest {
    PeerRequest {
        path,
        range: Some(ByteRange::FromTo {
            start: 50_000_000,
            end: None,
        }),
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

fn normal_request(path: String) -> PeerRequest {
    PeerRequest {
        path,
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

async fn negotiate(service: &Arc<MediaService>, entry_key: &str, owner: &str) -> Resolved {
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

async fn claim_stream(service: &Arc<MediaService>, path: &str) {
    let opened = service.resolve(&normal_request(path.to_string())).await;
    assert_eq!(opened.header.status, 200, "stream must open");
    let body = stream_body(opened, service);
    let mut body = std::pin::pin!(body);
    assert!(
        body.next().await.is_some_and(|chunk| chunk.is_ok()),
        "stream must deliver at least one chunk"
    );
}

/// Issue #412: An unsatisfiable Range request must release the session.
/// After receiving 416 for an out-of-range request, a same-owner retry of
/// the same entry must succeed immediately, not wait for idle_timeout.
#[tokio::test]
async fn unsatisfiable_range_releases_session() {
    let fx = fixture("unsatisfiable-range").await;
    let movie = movie_entry("0123456789abcdef00000412", "movies/example.mp4", "Example");
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();

    // Only one 1 Mbps direct session fits: the unsatisfiable Range retry
    // can only succeed if the initial request's session is actually released.
    let idle_timeout = Duration::from_millis(150);
    let service = Arc::new(MediaService::with_transcoding(
        Arc::clone(&fx.library),
        fx.media_root.clone(),
        wan_direct_config(&fx.base, 1_000_000, idle_timeout),
    ));
    let owner = "tv-fingerprint";

    // 1. Negotiate the initial /play
    let initial_play = negotiate(&service, &movie.entry_key, owner).await;
    let stream_path = plan_path(&initial_play);

    // 2. Claim the stream (first media request)
    claim_stream(&service, &stream_path).await;

    // 3. Send unsatisfiable Range request to the same stream path
    let unsatisfiable_resp = service
        .resolve(&unsatisfiable_range_request(stream_path.clone()))
        .await;
    assert_eq!(
        unsatisfiable_resp.header.status, 416,
        "unsatisfiable range must return 416"
    );

    // 4. Issue #412: Retry /play for the same entry immediately.
    // With the bug, this would fail (503) because the session from step 2
    // is still in_use and hasn't been released by the 416 response.
    // With the fix, finish_use is called before returning 416, so this
    // succeeds immediately.
    let retry_play = negotiate(&service, &movie.entry_key, owner).await;
    assert_eq!(
        retry_play.header.status, 200,
        "issue #412: a same-owner /play after an unsatisfiable Range response \
         must succeed immediately (session must be released by the 416 response, \
         not leaked until idle_timeout)"
    );

    // 5. Verify the new stream can be opened
    let retry_path = plan_path(&retry_play);
    claim_stream(&service, &retry_path).await;
}

/// Companion test: verify that the session is specifically released by the
/// 416 response, not just by waiting for idle_timeout. This confirms the
/// immediate-retry expectation holds.
#[tokio::test]
async fn unsatisfiable_range_does_not_require_idle_timeout_wait() {
    let fx = fixture("unsatisfiable-immediate").await;
    let movie = movie_entry("0123456789abcdef00000413", "movies/example.mp4", "Example");
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();

    // Set a long idle_timeout to clearly distinguish the 416-response release
    // from any idle-timeout-based release.
    let idle_timeout = Duration::from_secs(30);
    let service = Arc::new(MediaService::with_transcoding(
        Arc::clone(&fx.library),
        fx.media_root.clone(),
        wan_direct_config(&fx.base, 1_000_000, idle_timeout),
    ));
    let owner = "tv-fingerprint";

    let initial_play = negotiate(&service, &movie.entry_key, owner).await;
    let stream_path = plan_path(&initial_play);

    claim_stream(&service, &stream_path).await;

    let unsatisfiable_resp = service
        .resolve(&unsatisfiable_range_request(stream_path))
        .await;
    assert_eq!(unsatisfiable_resp.header.status, 416);

    // Immediate retry, no wait. Should succeed if the 416 response released
    // the session.
    let immediate_retry = negotiate(&service, &movie.entry_key, owner).await;
    assert_eq!(
        immediate_retry.header.status, 200,
        "unsatisfiable range must release the session immediately, \
         not wait for the long idle_timeout"
    );
}
