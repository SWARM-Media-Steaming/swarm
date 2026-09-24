//! Issue #384: "Same-owner /play 404s an already-claimed episode/movie
//! stream" — a third angle, distinct from the same-entry-retry scenarios
//! already covered in `tests/adversarial/same_entry_crash_retry` and
//! `tests/adversarial/same_owner_range_gap_retry`.
//!
//! `TranscodeManager::cancel_stale_claimed_for_owner` exists in the first
//! place because of #358/#389: a same-owner negotiation must not get stuck
//! behind a claimed session the owner has abandoned (crashed, navigated
//! away, picked something else) without ever sending `/stop` — see
//! `dropped_claim_is_still_reaped_so_the_same_owner_is_not_stuck` in
//! `tests/adversarial/active_playback_retry`. That is a domain invariant
//! established *before* #384, not something #384 gets to quietly narrow.
//!
//! #384's own fix (crates/swarm-media/src/transcode.rs,
//! `Session::last_release_clean`) makes the reaper skip any claimed session
//! whose *most recent* body finished at a natural end-of-stream rather than
//! being dropped early, so a same-entry retry landing between two range
//! requests of a still-watched stream no longer 404s it. That is correct for
//! same-entry retries. But the reaper is not scoped to same-entry retries —
//! it runs for *every* `/play` negotiation from that owner, including one
//! for a completely different title. And a range request finishing at a
//! natural end-of-stream is the overwhelmingly ordinary outcome of *any*
//! successfully-buffered read, not evidence the owner is still watching that
//! particular entry: an owner who reads one clean chunk of movie A and then,
//! without ever calling `/stop`, picks a different movie B has abandoned A
//! exactly as much as one whose read was dropped mid-stream — #358/#389 draw
//! no distinction between those two abandonment shapes, and nothing in #384
//! gives grounds to invent one.
//!
//! Expected behavior, derived from #358/#389's own invariant plus #384's
//! text ("Same-owner /play 404s an already-claimed episode/movie stream" —
//! the defect is the 404 of a still-*current* stream, not a license to
//! starve every *other* stream the owner has moved on from): negotiating
//! `/play` for entry B must be able to reap owner's stale claimed session
//! for a *different* entry A regardless of whether A's last range read
//! happened to finish cleanly, because "last read finished cleanly" says
//! nothing about whether the owner is still watching A once B has been
//! requested instead.

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
        "swarm-adv-384-switch-title-{tag}-{}-{n}",
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
        "playback negotiation must succeed, got {}",
        resolved.header.status
    );
    let Body::Bytes(body) = &resolved.body else {
        panic!("playback plan must be JSON");
    };
    let plan: swarm_core::peer::PlaybackPlan = serde_json::from_slice(body).unwrap();
    plan.path
}

/// Issue one ranged GET and fully drain it — the ordinary, successful
/// completion of a single buffered read, not an abandonment.
async fn complete_one_range_request(
    service: &Arc<MediaService>,
    path: &str,
    start: u64,
    end: u64,
) -> u16 {
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

fn two_movies() -> (EntryRecord, EntryRecord) {
    (
        movie_entry("0123456789abcdef00000aaa", "movies/a.mp4", "A"),
        movie_entry("0123456789abcdef00000bbb", "movies/b.mp4", "B"),
    )
}

/// Core regression: an owner who finishes one clean range read of A and then
/// (without `/stop`) negotiates `/play` for a *different* title B must still
/// get B — A's cleanly-finished-but-abandoned-for-B claim must not survive
/// forever just because its last read happened to hit natural EOF.
#[tokio::test]
async fn switching_titles_after_a_clean_range_read_still_frees_the_old_reservation() {
    let fx = fixture("switch").await;
    let (movie_a, movie_b) = two_movies();
    write_file(&fx.media_root, &movie_a.relative_path, &vec![7u8; 1_000_000]);
    write_file(&fx.media_root, &movie_b.relative_path, &vec![9u8; 1_000_000]);
    fx.library.upsert(&movie_a).await.unwrap();
    fx.library.upsert(&movie_b).await.unwrap();
    // Only one 1 Mbps direct session fits: B's negotiation can only succeed
    // if A's reservation is actually released.
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000, Duration::from_secs(300)));
    let owner = "tv-fingerprint";

    let plan_a = negotiate(&service, &movie_a.entry_key, owner).await;
    let path_a = plan_path(&plan_a);

    // Ordinary clean completion of one range read of A — exactly the
    // "routine, not exceptional" outcome #384's own fix protects when A is
    // renegotiated. Here the owner instead abandons A for B, with no
    // `/stop` in between, same as the crash-during-Next shape #358/#389
    // recover from.
    let first_status = complete_one_range_request(&service, &path_a, 0, 99_999).await;
    assert!(
        first_status == 200 || first_status == 206,
        "first ranged read of A must succeed, got {first_status}"
    );

    let plan_b = negotiate(&service, &movie_b.entry_key, owner).await;
    assert_eq!(
        plan_b.header.status, 200,
        "issue #384 / #358 / #389: negotiating a *different* title B must \
         free the same owner's stale reservation for A even though A's last \
         range read completed cleanly — a clean completion is the ordinary \
         outcome of any successful read, not evidence A is still being \
         watched once B has been requested instead. Got {} (expected 200)",
        plan_b.header.status
    );
}

/// Companion to the regression above: confirms A's reservation is only
/// gated on `idle_timeout` (not permanently stuck), which is what makes the
/// immediate-negotiation failure a *regression to the pre-#358 stuck-slot
/// behavior* rather than a total lockout.
#[tokio::test]
async fn switching_titles_does_eventually_succeed_once_idle_timeout_elapses() {
    let fx = fixture("switch-after-idle").await;
    let (movie_a, movie_b) = two_movies();
    write_file(&fx.media_root, &movie_a.relative_path, &vec![7u8; 1_000_000]);
    write_file(&fx.media_root, &movie_b.relative_path, &vec![9u8; 1_000_000]);
    fx.library.upsert(&movie_a).await.unwrap();
    fx.library.upsert(&movie_b).await.unwrap();
    let idle_timeout = Duration::from_millis(150);
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000, idle_timeout));
    let owner = "tv-fingerprint";

    let plan_a = negotiate(&service, &movie_a.entry_key, owner).await;
    let path_a = plan_path(&plan_a);
    let first_status = complete_one_range_request(&service, &path_a, 0, 99_999).await;
    assert!(first_status == 200 || first_status == 206);

    tokio::time::sleep(idle_timeout * 3).await;
    let plan_b = negotiate(&service, &movie_b.entry_key, owner).await;
    assert_eq!(
        plan_b.header.status, 200,
        "A's reservation is at least reachable once idle_timeout elapses — \
         confirming the immediate-negotiation failure above is a \
         reap-timing regression, not a permanent lockout"
    );
}
