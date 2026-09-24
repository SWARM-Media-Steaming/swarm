//! Issue #384: "Same-owner /play 404s an already-claimed episode/movie
//! stream" — found while testing #358: `plan()` reaped *every* claimed
//! non-track session for an owner before admitting a new one, so a same-
//! device retry of the very stream a TV was still watching 404'd it. The
//! shipped fix scopes `cancel_stale_claimed_for_owner` to sessions whose
//! `entry_key` differs from the entry being negotiated, so a retry of the
//! entry already claimed can never reap that entry's own session.
//!
//! Expected behavior, derived from the issue and from the *other* domain
//! invariant already established by issue #389
//! (`dropped_claim_is_still_reaped_so_the_same_owner_is_not_stuck` in
//! `tests/adversarial/active_playback_retry`) — not from the current
//! implementation:
//!
//! 1. Re-negotiating `/play` for an entry the same owner already has
//!    claimed and is actively streaming must succeed at the live path
//!    (#384's own repro): opening `/stream/{id}/media` after negotiating
//!    again must still return 200, not 404.
//! 2. A *genuine* crash (the body is dropped, `/stop` never arrives) of
//!    that same entry must still be recoverable by an immediate same-owner
//!    retry, exactly as #389 requires for a crash of any other entry. The
//!    server cannot use `entry_key` to prove a session is "still being
//!    watched" — a crashed claim of the current entry and a live claim of
//!    the current entry are bit-for-bit identical state (claimed,
//!    non-track, `in_use == 0`, same owner, same `entry_key`). Scoping the
//!    reap by `entry_key` alone (as shipped) silently resolves that
//!    ambiguity in favor of "always assume it's live", which reintroduces
//!    the exact five-minute wait #389 was written to eliminate — but only
//!    for a crash of the entry being retried, not a crash of a different
//!    one.
//!
//! Both properties cannot hold at once under the shipped `entry_key`-only
//! guard: satisfying #384's repro (never touch a same-entry claimed session)
//! necessarily un-satisfies #389's repro (always reap a same-owner crashed
//! claim immediately) whenever the crash and the retry share an entry_key.
//! The tests below hold #384's positive case to account and then show where
//! the shipped guard gives back #389's guarantee.

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{AudioStreamInfo, MediaKind, PeerRequest, PlaybackPreferences, VideoStreamInfo};
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
        "swarm-adv-384-{tag}-{}-{n}",
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

async fn stream_status(service: &MediaService, path: &str) -> u16 {
    service
        .resolve(&request(path.to_string()))
        .await
        .header
        .status
}

/// First media request claims the reservation; dropping the body afterward
/// is the crash: `/stop` never arrives and the row stays claimed until idle
/// expiry or a same-owner recovery `/play`.
async fn claim_then_crash(service: &Arc<MediaService>, path: &str) {
    let opened = service.resolve(&request(path.to_string())).await;
    assert_eq!(opened.header.status, 200, "the stream must actually open");
    let body = stream_body(opened, service);
    let mut body = std::pin::pin!(body);
    assert!(
        body.next().await.is_some_and(|chunk| chunk.is_ok()),
        "claimed playback must deliver at least one chunk before the crash"
    );
}

/// #384's own repro, held to account: negotiating `/play` again for an
/// entry the same owner already claimed and is actively reading must leave
/// the live stream at 200, not 404 it.
#[tokio::test]
async fn same_owner_replay_of_a_live_stream_does_not_404_it() {
    let fx = fixture("live-replay").await;
    let movie = movie_entry("0123456789abcdef00000384", "movies/example.mp4", "Example");
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    // Budget for two direct sessions so the retry's own negotiation cannot
    // be blamed on bandwidth admission.
    let service = service(&fx, wan_direct_config(&fx.base, 2_000_000, Duration::from_secs(300)));
    let owner = "tv-fingerprint";

    let plan_a = negotiate(&service, &movie.entry_key, owner).await;
    let path = plan_path(&plan_a);
    let opened = service.resolve(&request(path.clone())).await;
    assert_eq!(opened.header.status, 200, "the stream must actually open");
    let body = stream_body(opened, &service);
    let mut body = std::pin::pin!(body);
    assert!(body.next().await.is_some_and(|chunk| chunk.is_ok()));

    let _retry = negotiate(&service, &movie.entry_key, owner).await;
    assert_eq!(
        stream_status(&service, &path).await,
        200,
        "issue #384: a same-owner /play for an entry already claimed and \
         being read must not 404 that live stream"
    );
    drop(body);
}

/// Issue #389's invariant, unchanged by #384: a genuinely crashed claim
/// (body dropped, no `/stop`) must be recoverable by an *immediate*
/// same-owner retry — it must not need to wait out `idle_timeout` just
/// because the crash happened on the same entry the TV is retrying.
///
/// The shipped #384 guard cannot tell this case apart from the live-replay
/// case above: both leave a claimed, non-track, `in_use == 0` session under
/// the same owner and the same `entry_key`. Because the guard now refuses
/// to reap *any* claimed session whose `entry_key` matches the entry being
/// negotiated, it treats every same-entry crash as if it were still being
/// watched, and the retry fails admission instead of reaping the orphan —
/// reintroducing the five-minute wait #389 explicitly eliminated, just
/// narrowed to same-entry retries.
#[tokio::test]
async fn same_entry_crash_retry_recovers_immediately_not_after_the_idle_timeout() {
    let fx = fixture("crash-retry").await;
    let movie = movie_entry("0123456789abcdef00000384", "movies/example.mp4", "Example");
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    // Only one 1 Mbps direct session fits: the retry can only succeed if
    // the crashed session's reservation is actually released.
    let idle_timeout = Duration::from_millis(150);
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000, idle_timeout));
    let owner = "tv-fingerprint";

    let first = negotiate(&service, &movie.entry_key, owner).await;
    let first_path = plan_path(&first);
    claim_then_crash(&service, &first_path).await;

    let immediate_retry = negotiate(&service, &movie.entry_key, owner).await;
    assert_eq!(
        immediate_retry.header.status, 200,
        "issue #389: a same-owner retry must reap a crashed claim \
         immediately, not wait out idle_timeout — even when the crash and \
         the retry are for the same entry"
    );
}

/// Companion to the test above: proves the crashed claim was only ever
/// gated on `idle_timeout` (not permanently stuck), which is what makes the
/// immediate-retry failure a *regression to the pre-#389 five-minute wait*
/// rather than a total lockout.
#[tokio::test]
async fn same_entry_crash_retry_does_eventually_succeed_once_idle_timeout_elapses() {
    let fx = fixture("crash-retry-after-idle").await;
    let movie = movie_entry("0123456789abcdef00000384", "movies/example.mp4", "Example");
    write_file(&fx.media_root, &movie.relative_path, &vec![7u8; 1_000_000]);
    fx.library.upsert(&movie).await.unwrap();
    let idle_timeout = Duration::from_millis(150);
    let service = service(&fx, wan_direct_config(&fx.base, 1_000_000, idle_timeout));
    let owner = "tv-fingerprint";

    let first = negotiate(&service, &movie.entry_key, owner).await;
    let first_path = plan_path(&first);
    claim_then_crash(&service, &first_path).await;

    tokio::time::sleep(idle_timeout * 3).await;
    let after_idle_timeout = negotiate(&service, &movie.entry_key, owner).await;
    assert_eq!(
        after_idle_timeout.header.status, 200,
        "the crashed claim is at least reachable once idle_timeout elapses \
         — confirming the immediate-retry failure above is a reap-timing \
         regression, not a permanent lockout"
    );
}
