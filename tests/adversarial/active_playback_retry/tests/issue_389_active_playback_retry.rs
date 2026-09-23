//! Issue #389: CI on `ai-main` failed in
//! `playback_negotiation_returns_a_budgeted_direct_session_with_range_support`
//! with "a retry must not supersede a plan after the TV has opened it"
//! (opened `/stream/{id}/media` returned 404, expected 200).
//!
//! Expected behavior, derived from that failing contract, issue #358's
//! crash-recovery rules, and playback session invariants — not from the
//! current implementation:
//!
//! 1. An unclaimed reservation (the TV never opened the plan) from the
//!    same owner is garbage: a retry replaces it. The abandoned path is
//!    404 so it cannot keep consuming upload budget.
//! 2. Once the TV has opened the plan — the first playlist/media request
//!    landed and the response body is still being streamed — a same-device
//!    `/play` retry must leave that path at 200. The CI failure was this
//!    case: a range GET had already succeeded, then another negotiation
//!    from the same fingerprint 404'd the live stream.
//! 3. If admission cannot fit a second reservation, the retry is 429 (or
//!    any non-200). It must not free the live session to make room.
//! 4. A hard crash after the claim drops the body, `in_use` returns to 0,
//!    and `/stop` never arrives. That orphan is #358: the same owner's
//!    next `/play` must reap it immediately rather than wait out the
//!    five-minute idle timeout. Issue #389 must not undo that.
//! 5. Overlapping range requests on one session (two live bodies) still
//!    count as opened. Dropping one of them must not make the session
//!    reapable while the other body is still streaming.
//! 6. Another TV's opened or claimed session is never taken to recover
//!    this one. A hover preview, a missing-preferences `/play`, an
//!    unknown entry key, and a bogus stream path must not reap either.
//! 7. Music is different: `preloadNextTrack` keeps the playing track
//!    claimed while it negotiates the next one. An episode `/play` must
//!    not reap a live track session, and a track `/play` must not reap a
//!    live episode.

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{
    AudioStreamInfo, ByteRange, MediaKind, PeerRequest, PlaybackMode, PlaybackPlan,
    PlaybackPreferences, VideoStreamInfo,
};
use swarm_media::serve::{stream_body, Body, MediaService, Resolved};
use swarm_media::store::{EntryRecord, Library};
use swarm_media::transcode::TranscodeConfig;
use tokio::process::Command;

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
        "swarm-adv-389-{tag}-{}-{n}",
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

/// Peak for these fixtures is 1_000_000 bps (size*8/duration * 5/4).
fn movie_entry(entry_key: &str, relative_path: &str, title: &str) -> EntryRecord {
    video_entry(entry_key, relative_path, title, MediaKind::Movie, None, None)
}

fn episode_entry(entry_key: &str, relative_path: &str, title: &str) -> EntryRecord {
    video_entry(
        entry_key,
        relative_path,
        title,
        MediaKind::Episode,
        Some("Forensic Files".into()),
        Some(4),
    )
}

fn video_entry(
    entry_key: &str,
    relative_path: &str,
    title: &str,
    kind: MediaKind,
    show_title: Option<String>,
    season: Option<u32>,
) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind,
        title: title.into(),
        size: 1_000_000,
        modified_time: 0,
        fingerprint: format!("fp-{entry_key}"),
        artist: None,
        album: None,
        track_number: None,
        show_title: show_title.clone(),
        season,
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
        scraped_title: show_title,
        episode_title: Some(title.into()),
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

fn track_entry(entry_key: &str, relative_path: &str, title: &str) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Track,
        title: title.into(),
        size: 10_000,
        modified_time: 0,
        fingerprint: format!("fp-{entry_key}"),
        artist: Some("Artist".into()),
        album: Some("Album".into()),
        track_number: Some(1),
        show_title: None,
        season: None,
        episode: None,
        year: None,
        duration_secs: Some(180.0),
        video: None,
        audio: Some(AudioStreamInfo {
            codec: "aac".into(),
            channels: 2,
            bitrate: Some(128_000),
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

fn wan_direct_config(base: &Path, max_upload_bps: u64) -> TranscodeConfig {
    TranscodeConfig {
        enabled: false,
        ffmpeg_path: "ffmpeg".into(),
        session_dir: base.join("sessions"),
        max_upload_bps,
        reserve_percent: 0,
        max_sessions: 1,
        idle_timeout: Duration::from_secs(300),
        segment_duration_secs: 4,
        ..Default::default()
    }
}

fn lan_hls_config(base: &Path) -> TranscodeConfig {
    TranscodeConfig {
        enabled: true,
        ffmpeg_path: "ffmpeg".into(),
        session_dir: base.join("sessions"),
        max_upload_bps: 10_000_000,
        reserve_percent: 0,
        max_sessions: 1,
        idle_timeout: Duration::from_secs(300),
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

fn play_request(entry_key: &str, prefer_direct: bool, preview: bool) -> PeerRequest {
    PeerRequest {
        path: format!("/play/{entry_key}"),
        range: None,
        if_none_match: None,
        playback: Some(PlaybackPreferences {
            capabilities: CapabilityProfile::fire_tv_baseline(),
            start_position_secs: 0,
            prefer_direct,
            preview,
        }),
        error_report: None,
        like: None,
    }
}

fn request(path: String) -> PeerRequest {
    PeerRequest {
        path,
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

fn range_request(path: String, start: u64, end: u64) -> PeerRequest {
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

async fn negotiate(
    service: &MediaService,
    entry_key: &str,
    owner: &str,
    is_lan: bool,
    prefer_direct: bool,
) -> Resolved {
    service
        .resolve_for_peer(
            &play_request(entry_key, prefer_direct, false),
            is_lan,
            "Living room TV",
            owner,
        )
        .await
}

fn plan_from(resolved: &Resolved) -> PlaybackPlan {
    assert_eq!(
        resolved.header.status, 200,
        "playback negotiation must succeed"
    );
    let Body::Bytes(body) = &resolved.body else {
        panic!("playback plan must be JSON");
    };
    serde_json::from_slice(body).unwrap()
}

async fn open_live<'a>(
    service: &'a Arc<MediaService>,
    path: &str,
) -> impl futures_util::Stream<Item = std::io::Result<bytes::Bytes>> + 'a {
    let opened = service.resolve(&request(path.to_string())).await;
    assert_eq!(
        opened.header.status, 200,
        "opening the plan is what claims the reservation"
    );
    stream_body(opened, service)
}

/// First media request claims the reservation; dropping the body is the
/// crash: `/stop` never arrives and the row stays claimed until idle expiry
/// or a same-owner recovery `/play`.
async fn claim_then_crash(service: &Arc<MediaService>, path: &str) {
    let opened = service.resolve(&request(path.to_string())).await;
    assert_eq!(opened.header.status, 200, "the stream must actually open");
    {
        let body = stream_body(opened, service);
        let mut body = std::pin::pin!(body);
        let first = body.next().await;
        assert!(
            first.is_some_and(|chunk| chunk.is_ok()),
            "claimed playback must deliver at least one chunk before the crash"
        );
    }
}

async fn stream_status(service: &MediaService, path: &str) -> u16 {
    service
        .resolve(&request(path.to_string()))
        .await
        .header
        .status
}

fn require_ffmpeg() {
    let ok = std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !ok {
        panic!("ffmpeg is required for the LAN HLS path of issue #389");
    }
}

#[tokio::test]
async fn unclaimed_same_owner_retry_still_supersedes_the_abandoned_plan() {
    let fx = fixture("unclaimed").await;
    let movie = movie_entry(
        "0123456789abcdef00000389",
        "movies/example.mp4",
        "Example",
    );
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000));
    let owner = "tv-fingerprint";

    let abandoned = plan_from(&negotiate(&service, &movie.entry_key, owner, false, true).await);
    assert_eq!(abandoned.mode, PlaybackMode::Direct);

    let retry = negotiate(&service, &movie.entry_key, owner, false, true).await;
    assert_eq!(retry.header.status, 200);
    let plan = plan_from(&retry);
    assert_ne!(plan.session_id, abandoned.session_id);
    assert_eq!(
        stream_status(&service, &abandoned.path).await,
        404,
        "a retry must replace an unopened plan so it cannot keep the budget"
    );
}

#[tokio::test]
async fn opened_direct_stream_survives_a_same_owner_retry() {
    let fx = fixture("opened-direct").await;
    let movie = movie_entry(
        "0123456789abcdef00000389",
        "movies/example.mp4",
        "Example",
    );
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    // Two 1 Mbps direct sessions fit. The retry may 200; the live path
    // must remain readable either way.
    let service = service(&fx, wan_direct_config(&fx.base, 2_000_000));
    let owner = "tv-fingerprint";

    let plan = plan_from(&negotiate(&service, &movie.entry_key, owner, false, true).await);
    let opened = service
        .resolve(&range_request(plan.path.clone(), 500_000, 500_099))
        .await;
    assert_eq!((opened.header.status, opened.header.len), (206, 100));
    let body = stream_body(opened, &service);
    let mut body = std::pin::pin!(body);
    assert!(
        body.next().await.is_some_and(|chunk| chunk.is_ok()),
        "the TV has started reading the opened plan"
    );

    let retry = negotiate(&service, &movie.entry_key, owner, false, true).await;
    assert_eq!(
        retry.header.status, 200,
        "budget allows a second direct session, so the retry itself may succeed"
    );
    assert_eq!(
        stream_status(&service, &plan.path).await,
        200,
        "a retry must not supersede a plan after the TV has opened it"
    );
    assert_eq!(
        service.transcode_manager().reserved_bps(),
        2_000_000,
        "the live reservation stays on the books while its body is streaming"
    );
    drop(body);
}

#[tokio::test]
async fn opened_stream_is_not_stolen_when_the_retry_cannot_fit() {
    let fx = fixture("no-steal").await;
    let movie = movie_entry(
        "0123456789abcdef00000389",
        "movies/example.mp4",
        "Example",
    );
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000));
    let owner = "tv-fingerprint";

    let plan = plan_from(&negotiate(&service, &movie.entry_key, owner, false, true).await);
    let live = open_live(&service, &plan.path).await;
    let mut live = std::pin::pin!(live);
    assert!(live.next().await.is_some_and(|chunk| chunk.is_ok()));

    let retry = negotiate(&service, &movie.entry_key, owner, false, true).await;
    assert_ne!(
        retry.header.status, 200,
        "a second 1 Mbps reservation must not fit while the live one is held"
    );
    assert_eq!(
        stream_status(&service, &plan.path).await,
        200,
        "admission failure must not cancel the stream the TV is still reading"
    );
    assert_eq!(service.transcode_manager().reserved_bps(), 1_000_000);
    drop(live);
}

#[tokio::test]
async fn opened_episode_survives_a_retry_of_a_different_title() {
    let fx = fixture("other-title").await;
    let e4 = episode_entry(
        "0123456789abcdef00000004",
        "shows/Forensic Files/S04E04.mp4",
        "Season 4 Episode 4",
    );
    let e5 = episode_entry(
        "0123456789abcdef00000005",
        "shows/Forensic Files/S04E05.mp4",
        "Season 4 Episode 5",
    );
    write_file(&fx.media_root, &e4.relative_path, &vec![7u8; 1_000_000]);
    write_file(&fx.media_root, &e5.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&e4).await.unwrap();
    fx.library.upsert(&e5).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000));
    let owner = "living-room";

    let playing = plan_from(&negotiate(&service, &e4.entry_key, owner, false, true).await);
    let live = open_live(&service, &playing.path).await;
    let mut live = std::pin::pin!(live);
    assert!(live.next().await.is_some_and(|chunk| chunk.is_ok()));

    let next = negotiate(&service, &e5.entry_key, owner, false, true).await;
    assert_ne!(
        next.header.status, 200,
        "next-episode /play without /stop must not evict the episode still streaming"
    );
    assert_eq!(
        stream_status(&service, &playing.path).await,
        200,
        "S4E4 must keep serving while its body is still open"
    );
    drop(live);
}

#[tokio::test]
async fn overlapping_range_requests_keep_the_session_through_a_retry() {
    let fx = fixture("overlap").await;
    let movie = movie_entry(
        "0123456789abcdef00000389",
        "movies/example.mp4",
        "Example",
    );
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000));
    let owner = "tv-fingerprint";

    let plan = plan_from(&negotiate(&service, &movie.entry_key, owner, false, true).await);

    let first = service
        .resolve(&range_request(plan.path.clone(), 0, 99))
        .await;
    assert_eq!(first.header.status, 206);
    let first = stream_body(first, &service);
    let mut first = std::pin::pin!(first);
    assert!(first.next().await.is_some_and(|chunk| chunk.is_ok()));

    let second = service
        .resolve(&range_request(plan.path.clone(), 100, 199))
        .await;
    assert_eq!(second.header.status, 206);
    let second = stream_body(second, &service);
    let mut second = std::pin::pin!(second);
    assert!(second.next().await.is_some_and(|chunk| chunk.is_ok()));

    let retry = negotiate(&service, &movie.entry_key, owner, false, true).await;
    assert_ne!(retry.header.status, 200);
    assert_eq!(stream_status(&service, &plan.path).await, 200);

    drop(first);
    let retry_after_one_drop = negotiate(&service, &movie.entry_key, owner, false, true).await;
    assert_ne!(
        retry_after_one_drop.header.status, 200,
        "one remaining live range is still an opened plan"
    );
    assert_eq!(stream_status(&service, &plan.path).await, 200);

    drop(second);
}

#[tokio::test]
async fn dropped_claim_is_still_reaped_so_the_same_owner_is_not_stuck() {
    let fx = fixture("crash-reap").await;
    let movie = movie_entry(
        "0123456789abcdef00000389",
        "movies/example.mp4",
        "Example",
    );
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000));
    let owner = "tv-fingerprint";

    let first = plan_from(&negotiate(&service, &movie.entry_key, owner, false, true).await);
    claim_then_crash(&service, &first.path).await;

    let retry = negotiate(&service, &movie.entry_key, owner, false, true).await;
    assert_eq!(
        retry.header.status, 200,
        "issue #389 must not restore the five-minute wait after a crashed claim"
    );
    let retry_plan = plan_from(&retry);
    assert_ne!(retry_plan.session_id, first.session_id);
    assert_eq!(
        stream_status(&service, &first.path).await,
        404,
        "the orphaned claim must be gone so it cannot keep the slot"
    );
}

#[tokio::test]
async fn another_tvs_opened_stream_is_not_reaped() {
    let fx = fixture("other-tv").await;
    let a = movie_entry(
        "0123456789abcdef00000aa1",
        "movies/a.mp4",
        "A",
    );
    let b = movie_entry(
        "0123456789abcdef00000aa2",
        "movies/b.mp4",
        "B",
    );
    write_file(&fx.media_root, &a.relative_path, &vec![7u8; 1_000_000]);
    write_file(&fx.media_root, &b.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&a).await.unwrap();
    fx.library.upsert(&b).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 2_000_000));

    let bedroom = plan_from(&negotiate(&service, &a.entry_key, "bedroom", false, true).await);
    let live = open_live(&service, &bedroom.path).await;
    let mut live = std::pin::pin!(live);
    assert!(live.next().await.is_some_and(|chunk| chunk.is_ok()));

    let living = plan_from(
        &negotiate(&service, &b.entry_key, "living-room", false, true).await,
    );
    claim_then_crash(&service, &living.path).await;
    let retry = negotiate(&service, &b.entry_key, "living-room", false, true).await;
    assert_eq!(retry.header.status, 200);
    assert_eq!(
        stream_status(&service, &bedroom.path).await,
        200,
        "recovering the living-room crash must not stop the bedroom TV"
    );
    drop(live);
}

#[tokio::test]
async fn preview_and_malformed_play_do_not_kill_an_opened_stream() {
    let fx = fixture("malformed").await;
    let movie = movie_entry(
        "0123456789abcdef00000389",
        "movies/example.mp4",
        "Example",
    );
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 2_000_000));
    let owner = "tv-fingerprint";

    let plan = plan_from(&negotiate(&service, &movie.entry_key, owner, false, true).await);
    let live = open_live(&service, &plan.path).await;
    let mut live = std::pin::pin!(live);
    assert!(live.next().await.is_some_and(|chunk| chunk.is_ok()));

    let missing_prefs = service
        .resolve_for_peer(
            &request(format!("/play/{}", movie.entry_key)),
            false,
            "Living room TV",
            owner,
        )
        .await;
    assert_eq!(
        missing_prefs.header.status, 400,
        "POST /play without PlaybackPreferences is a client error"
    );

    let unknown = negotiate(&service, "0123456789abcdef00000bad", owner, false, true).await;
    assert_eq!(unknown.header.status, 404);

    let bogus = service
        .resolve(&request("/stream/not-a-session/media".into()))
        .await;
    assert_eq!(bogus.header.status, 404);

    let preview = service
        .resolve_for_peer(
            &play_request(&movie.entry_key, true, true),
            false,
            "Living room TV",
            owner,
        )
        .await;
    let _ = preview.header.status;

    assert_eq!(
        stream_status(&service, &plan.path).await,
        200,
        "client errors and previews must not reap an opened plan"
    );
    drop(live);
}

#[tokio::test]
async fn empty_owner_retry_does_not_cross_cancel_a_named_live_stream() {
    let fx = fixture("empty-owner").await;
    let movie = movie_entry(
        "0123456789abcdef00000389",
        "movies/example.mp4",
        "Example",
    );
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000));

    let plan = plan_from(
        &negotiate(&service, &movie.entry_key, "tv-fingerprint", false, true).await,
    );
    let live = open_live(&service, &plan.path).await;
    let mut live = std::pin::pin!(live);
    assert!(live.next().await.is_some_and(|chunk| chunk.is_ok()));

    let empty = negotiate(&service, &movie.entry_key, "", false, true).await;
    assert_ne!(
        empty.header.status, 200,
        "an empty owner must not inherit another TV's live reservation"
    );
    assert_eq!(stream_status(&service, &plan.path).await, 200);
    drop(live);
}

#[tokio::test]
async fn live_track_survives_an_episode_play_from_the_same_owner() {
    let fx = fixture("live-track").await;
    let track = track_entry(
        "0123456789abcdef00000aa1",
        "music/Artist/Album/01.m4a",
        "Track 1",
    );
    let episode = episode_entry(
        "0123456789abcdef00000004",
        "shows/Forensic Files/S04E04.mp4",
        "Season 4 Episode 4",
    );
    write_file(&fx.media_root, &track.relative_path, &vec![7u8; 10_000]);
    write_file(&fx.media_root, &episode.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&track).await.unwrap();
    fx.library.upsert(&episode).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 10_000_000));
    let owner = "living-room";

    let playing = plan_from(&negotiate(&service, &track.entry_key, owner, false, true).await);
    let live = open_live(&service, &playing.path).await;
    let mut live = std::pin::pin!(live);
    assert!(live.next().await.is_some_and(|chunk| chunk.is_ok()));

    let episode_play = negotiate(&service, &episode.entry_key, owner, false, true).await;
    assert_eq!(episode_play.header.status, 200);
    assert_eq!(
        stream_status(&service, &playing.path).await,
        200,
        "an episode /play must not reap a track the TV is still streaming"
    );
    drop(live);
}

#[tokio::test]
async fn live_episode_survives_a_track_play_from_the_same_owner() {
    let fx = fixture("live-episode").await;
    let track = track_entry(
        "0123456789abcdef00000aa1",
        "music/Artist/Album/01.m4a",
        "Track 1",
    );
    let episode = episode_entry(
        "0123456789abcdef00000004",
        "shows/Forensic Files/S04E04.mp4",
        "Season 4 Episode 4",
    );
    write_file(&fx.media_root, &track.relative_path, &vec![7u8; 10_000]);
    write_file(&fx.media_root, &episode.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&track).await.unwrap();
    fx.library.upsert(&episode).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 10_000_000));
    let owner = "living-room";

    let playing = plan_from(&negotiate(&service, &episode.entry_key, owner, false, true).await);
    let live = open_live(&service, &playing.path).await;
    let mut live = std::pin::pin!(live);
    assert!(live.next().await.is_some_and(|chunk| chunk.is_ok()));

    let track_play = negotiate(&service, &track.entry_key, owner, false, true).await;
    assert_eq!(track_play.header.status, 200);
    assert_eq!(
        stream_status(&service, &playing.path).await,
        200,
        "a track /play must not reap an episode the TV is still streaming"
    );
    drop(live);
}

#[tokio::test]
async fn ci_shape_range_resolve_without_stream_body_still_counts_as_opened() {
    // Exact shape of the failing CI assertion: `resolve` of a Range GET
    // (no `stream_body`) then a same-owner `/play`. The opened path must
    // stay 200. `stream_body` is the production release path; this locks
    // the contract the CI test already encodes.
    let fx = fixture("ci-shape").await;
    let movie = movie_entry(
        "0123456789abcdef00000389",
        "movies/example.mp4",
        "Example",
    );
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base, 10_000_000));
    let owner = "tv-fingerprint";

    let plan = plan_from(&negotiate(&service, &movie.entry_key, owner, false, true).await);
    let mut media_request = request(plan.path.clone());
    media_request.range = Some(ByteRange::FromTo {
        start: 500_000,
        end: Some(500_099),
    });
    let media = service.resolve(&media_request).await;
    assert_eq!((media.header.status, media.header.len), (206, 100));

    let active_retry = negotiate(&service, &movie.entry_key, owner, false, true).await;
    assert_eq!(active_retry.header.status, 200);
    assert_eq!(
        stream_status(&service, &plan.path).await,
        200,
        "a retry must not supersede a plan after the TV has opened it"
    );
    drop(media);
}

#[tokio::test]
async fn opened_hls_playlist_survives_a_same_owner_retry() {
    require_ffmpeg();

    let fx = fixture("hls-live").await;
    let relative = "shows/Forensic Files/S04E04.mp4";
    let source = fx.media_root.join(relative);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=10:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
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
    assert!(generated.success(), "failed to generate the HLS fixture");

    let mut e4 = episode_entry(
        "0123456789abcdef00000004",
        relative,
        "Season 4 Episode 4",
    );
    e4.size = source.metadata().unwrap().len();
    e4.duration_secs = Some(1.0);
    e4.video.as_mut().unwrap().width = 320;
    e4.video.as_mut().unwrap().height = 240;
    fx.library.upsert(&e4).await.unwrap();

    let service = service(&fx, lan_hls_config(&fx.base));
    let owner = "living-room";

    let first = plan_from(&negotiate(&service, &e4.entry_key, owner, true, false).await);
    assert_eq!(first.mode, PlaybackMode::Hls);
    let playlist = first.path.clone();
    let live = open_live(&service, &playlist).await;
    let mut live = std::pin::pin!(live);
    assert!(live.next().await.is_some_and(|chunk| chunk.is_ok()));

    let retry = negotiate(&service, &e4.entry_key, owner, true, false).await;
    // A 1s fixture may already have let ffmpeg exit, which frees the
    // transcode slot while the playlist files remain. The retry may then
    // 200 with a *new* session. Issue #389 forbids replacing the playlist
    // the TV still holds.
    if retry.header.status == 200 {
        let retry_plan = plan_from(&retry);
        assert_ne!(retry_plan.session_id, first.session_id);
    }
    assert_eq!(
        stream_status(&service, &playlist).await,
        200,
        "a retry must not supersede an HLS plan the TV has opened"
    );
    drop(live);
}
