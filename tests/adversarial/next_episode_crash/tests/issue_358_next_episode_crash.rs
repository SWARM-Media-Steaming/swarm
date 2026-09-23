//! Issue #358: tapping Next from Forensic Files S4E3 → S4E4 hard-crashes,
//! and navigating back to S4E4 then never loads a stream. S4E5 plays.
//!
//! Expected behavior, derived from the issue and playback session rules
//! (not from the current implementation):
//!
//! 1. Next-episode is a user-initiated play of a different episode. It must
//!    leave the client able to play that successor (and the original) —
//!    a hard crash is a failed next, not an acceptable outcome.
//! 2. Session discipline (`POST /stop`) is the *normal* cleanup path, but
//!    a dead client cannot call it. After the first playlist/segment request
//!    has claimed a reservation, idle expiry is five minutes. Retrying the
//!    crashed episode immediately must not fail with 429 Capacity/Bandwidth
//!    while that orphan still occupies the owner's slot.
//! 3. The same owner's *next* episode (S4E4 after a crash on the S4E3→S4E4
//!    handoff) and a later sibling (S4E5) must still negotiate 200. Another
//!    TV's claimed session must not be taken to make room.
//! 4. Music is different: `preloadNextTrack` keeps the playing track claimed
//!    while it negotiates the successor. A new track `/play` from that owner
//!    must not reap those claimed track sessions.
//! 5. A hover preview must not reap a claimed episode/movie session.
//! 6. Malformed `/play` (missing preferences, unknown key) is 400/404 and
//!    must not be required to recover anything.

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{
    AudioStreamInfo, MediaKind, PeerRequest, PlaybackMode, PlaybackPlan, PlaybackPreferences,
    VideoStreamInfo,
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
        "swarm-adv-358-{tag}-{}-{n}",
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
fn episode_entry(entry_key: &str, relative_path: &str, title: &str) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Episode,
        title: title.into(),
        size: 1_000_000,
        modified_time: 0,
        fingerprint: format!("fp-{entry_key}"),
        artist: None,
        album: None,
        track_number: None,
        show_title: Some("Forensic Files".into()),
        season: Some(4),
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
        scraped_title: Some("Forensic Files".into()),
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

fn wan_direct_config(base: &Path) -> TranscodeConfig {
    TranscodeConfig {
        enabled: false,
        ffmpeg_path: "ffmpeg".into(),
        session_dir: base.join("sessions"),
        // Usable = 1_000_000 bps: exactly one of these episode reservations.
        max_upload_bps: 1_000_000,
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
    assert_eq!(resolved.header.status, 200, "playback negotiation must succeed");
    let Body::Bytes(body) = &resolved.body else {
        panic!("playback plan must be JSON");
    };
    serde_json::from_slice(body).unwrap()
}

/// First playlist/segment request claims the reservation; dropping the
/// body is the crash: the client is gone, `/stop` never arrives, `in_use`
/// returns to 0, the row stays claimed until idle expiry.
async fn claim_then_crash(service: &Arc<MediaService>, path: &str) {
    let opened = service.resolve(&request(path.to_string())).await;
    assert_eq!(
        opened.header.status, 200,
        "the successor stream must actually open (this is the claim)"
    );
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

#[tokio::test]
async fn malformed_play_is_client_error_not_a_stream() {
    let fx = fixture("malformed").await;
    let e4 = episode_entry(
        "0123456789abcdef00000004",
        "shows/Forensic Files/S04E04.mp4",
        "Season 4 Episode 4",
    );
    write_file(&fx.media_root, &e4.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&e4).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base));

    let missing_prefs = service
        .resolve_for_peer(
            &request(format!("/play/{}", e4.entry_key)),
            false,
            "Living room TV",
            "living-room",
        )
        .await;
    assert_eq!(
        missing_prefs.header.status, 400,
        "POST /play without PlaybackPreferences is a client error"
    );

    let unknown = negotiate(&service, "0123456789abcdef00000bad", "living-room", false, true).await;
    assert_eq!(unknown.header.status, 404, "unknown entry_key must 404");

    let bogus_stream = service
        .resolve(&request("/stream/not-a-session/media".into()))
        .await;
    assert_eq!(bogus_stream.header.status, 404);

    let stop_unknown = service.resolve(&request("/stop/not-a-session".into())).await;
    assert_eq!(
        stop_unknown.header.status, 200,
        "POST /stop is idempotent even for an unknown session"
    );
}

#[tokio::test]
async fn crashed_next_episode_claim_does_not_block_retry_of_that_episode() {
    let fx = fixture("retry-same").await;
    let e4 = episode_entry(
        "0123456789abcdef00000004",
        "shows/Forensic Files/S04E04.mp4",
        "Season 4 Episode 4",
    );
    write_file(&fx.media_root, &e4.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&e4).await.unwrap();
    let service = service(&fx, wan_direct_config(&fx.base));
    let owner = "living-room";

    let first = plan_from(&negotiate(&service, &e4.entry_key, owner, false, true).await);
    assert_eq!(first.mode, PlaybackMode::Direct);
    claim_then_crash(&service, &first.path).await;

    // Issue: after the crash, opening S4E4 from the catalog "never loads".
    // Idle timeout is still five minutes; the retry must not wait for it.
    let retry = negotiate(&service, &e4.entry_key, owner, false, true).await;
    assert_eq!(
        retry.header.status, 200,
        "retrying the crashed episode from the same TV must not 429 on the orphaned claim"
    );
    let retry_plan = plan_from(&retry);
    assert_ne!(retry_plan.session_id, first.session_id);
    assert_eq!(
        service.resolve(&request(first.path.clone())).await.header.status,
        404,
        "the abandoned next-episode reservation must be gone so it cannot keep the slot"
    );
}

#[tokio::test]
async fn crashed_s4e4_claim_does_not_block_s4e5_or_a_fresh_s4e4() {
    let fx = fixture("sibling").await;
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
    let service = service(&fx, wan_direct_config(&fx.base));
    let owner = "living-room";

    let crashed = plan_from(&negotiate(&service, &e4.entry_key, owner, false, true).await);
    claim_then_crash(&service, &crashed.path).await;

    let e5_play = negotiate(&service, &e5.entry_key, owner, false, true).await;
    assert_eq!(
        e5_play.header.status, 200,
        "S4E5 must still play after a crash on the S4E3→S4E4 next-episode handoff"
    );

    let e4_again = negotiate(&service, &e4.entry_key, owner, false, true).await;
    assert_eq!(
        e4_again.header.status, 200,
        "navigating to S4E4 after the next-episode crash must load a stream"
    );
}

#[tokio::test]
async fn another_tvs_claimed_episode_is_not_reaped_to_recover_this_one() {
    let fx = fixture("other-owner").await;
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
    // Two 1 Mbps sessions fit; reaping the wrong owner would still "succeed"
    // the retry, so assert the other TV's path stays 200.
    let mut config = wan_direct_config(&fx.base);
    config.max_upload_bps = 2_000_000;
    let service = service(&fx, config);

    let bedroom = plan_from(
        &negotiate(&service, &e5.entry_key, "bedroom", false, true).await,
    );
    claim_then_crash(&service, &bedroom.path).await;

    let crashed = plan_from(
        &negotiate(&service, &e4.entry_key, "living-room", false, true).await,
    );
    claim_then_crash(&service, &crashed.path).await;

    let retry = negotiate(&service, &e4.entry_key, "living-room", false, true).await;
    assert_eq!(retry.header.status, 200);
    assert_eq!(
        service
            .resolve(&request(bedroom.path.clone()))
            .await
            .header
            .status,
        200,
        "recovering the crashed living-room next-episode must not stop the bedroom TV"
    );
}

#[tokio::test]
async fn claimed_track_sessions_survive_a_same_owner_track_play() {
    let fx = fixture("tracks").await;
    let t1 = track_entry(
        "0123456789abcdef00000aa1",
        "music/Artist/Album/01.m4a",
        "Track 1",
    );
    let t2 = track_entry(
        "0123456789abcdef00000aa2",
        "music/Artist/Album/02.m4a",
        "Track 2",
    );
    write_file(&fx.media_root, &t1.relative_path, &vec![7u8; 10_000]);
    write_file(&fx.media_root, &t2.relative_path, &vec![7u8; 10_000]);
    fx.library.upsert(&t1).await.unwrap();
    fx.library.upsert(&t2).await.unwrap();
    let mut config = wan_direct_config(&fx.base);
    config.max_upload_bps = 10_000_000;
    let service = service(&fx, config);
    let owner = "living-room";

    let current = plan_from(&negotiate(&service, &t1.entry_key, owner, false, true).await);
    claim_then_crash(&service, &current.path).await;

    // Gapless preload: the current track stays claimed while the next
    // track is negotiated. That pair is intentional, not an orphan.
    let next = negotiate(&service, &t2.entry_key, owner, false, true).await;
    assert_eq!(next.header.status, 200);
    assert_eq!(
        service
            .resolve(&request(current.path.clone()))
            .await
            .header
            .status,
        200,
        "preloadNextTrack must keep the playing track's claimed session"
    );
}

#[tokio::test]
async fn preview_does_not_reap_a_claimed_episode() {
    let fx = fixture("preview").await;
    let e4 = episode_entry(
        "0123456789abcdef00000004",
        "shows/Forensic Files/S04E04.mp4",
        "Season 4 Episode 4",
    );
    write_file(&fx.media_root, &e4.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&e4).await.unwrap();
    let mut config = wan_direct_config(&fx.base);
    config.max_upload_bps = 10_000_000;
    let service = service(&fx, config);
    let owner = "living-room";

    let playing = plan_from(&negotiate(&service, &e4.entry_key, owner, false, true).await);
    claim_then_crash(&service, &playing.path).await;

    let preview = service
        .resolve_for_peer(
            &play_request(&e4.entry_key, true, true),
            false,
            "Living room TV",
            owner,
        )
        .await;
    // Preview may 200 (new preview session) or 429; either way the claimed
    // foreground episode must still be readable. A preview that reaps it
    // would 404 the live path.
    let _ = preview.header.status;
    assert_eq!(
        service
            .resolve(&request(playing.path.clone()))
            .await
            .header
            .status,
        200,
        "a hover preview must not reap a claimed episode session"
    );
}

#[tokio::test]
async fn lan_hls_capacity_orphan_does_not_block_the_crashed_episode() {
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_err()
    {
        panic!("ffmpeg is required for the LAN HLS capacity path of issue #358");
    }

    let fx = fixture("hls-lan").await;
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

    // LAN Fire TV: bandwidth is not the limiter; max_sessions is. Force HLS
    // so this reservation occupies the single ffmpeg slot, matching a
    // next-episode crash on an episode that cannot direct-play.
    let first = plan_from(&negotiate(&service, &e4.entry_key, owner, true, false).await);
    assert_eq!(first.mode, PlaybackMode::Hls);
    claim_then_crash(&service, &first.path).await;

    let retry = negotiate(&service, &e4.entry_key, owner, true, false).await;
    assert_eq!(
        retry.header.status, 200,
        "LAN HLS retry after a claimed next-episode crash must not return 429 capacity full"
    );
}
