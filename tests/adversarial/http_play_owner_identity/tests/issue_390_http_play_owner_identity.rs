//! Issue #390: HTTP `/play` never passed a `playback_owner` into
//! `MediaService::resolve_for_peer`/`resolve_for_transport`, so
//! `TranscodeManager::plan`'s `cancel_unclaimed_for_owner`/
//! `cancel_stale_claimed_for_owner` never ran on the plain-HTTP media
//! surface (`apps/server/src/http_media.rs`). The fix threads the paired
//! device's `token_hash` through as `playback_owner`.
//!
//! Expected behavior, derived from the fix's own stated rationale (see
//! `.claude/skills/swarm-http-media-server/SKILL.md`'s "playback_owner"
//! section and issue #389's playback-owner invariants) and from the domain
//! fact that `AuthenticatedDevice`'s *display name* is user-editable in the
//! dashboard and has no uniqueness constraint
//! (`state_db::save_http_media_device` upserts keyed only by `token_hash`,
//! see `apps/server/src/state_db.rs`) while `token_hash` is derived from the
//! device's bearer token and is therefore both stable and unique per paired
//! device — NOT from the current implementation:
//!
//! Two independently paired devices that happen to share a display name
//! (e.g. two Rokus a household never bothered to rename) are different
//! playback owners. A `/play` negotiation from device B must never cancel
//! device A's own outstanding reservation just because the two devices'
//! `AuthenticatedDevice` display names collide. If `playback_owner` were
//! (re)implemented as the display name instead of `token_hash` — the
//! precise regression the fix's own inline comments warn against — this
//! test would catch it directly over the real HTTP wire: B's negotiation
//! would wrongly reap A's still-valid, not-yet-opened plan, and A's plan
//! path would 404 instead of continuing to resolve.

use serde_json::{json, Value};
use swarm_core::peer::{MediaKind, PlaybackMode, PlaybackPlan, VideoStreamInfo};
use swarm_media::store::EntryRecord;
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};

async fn pair_and_get_token(
    client: &reqwest::Client,
    base_url: &str,
    core: &ServerCore,
    name: &str,
) -> String {
    let begin: Value = client
        .post(format!("{base_url}/pair/begin"))
        .json(&json!({ "name": name }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let code = begin["code"].as_str().unwrap();
    core.approve_http_media_pairing(code).await.unwrap();

    let poll: Value = client
        .post(format!("{base_url}/pair/poll"))
        .json(&json!({
            "activation_id": begin["activation_id"],
            "poll_token": begin["poll_token"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(poll["status"], "approved");
    poll["token"].as_str().unwrap().to_string()
}

fn direct_play_entry(entry_key: &str, relative_path: &str, size: u64) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size,
        modified_time: 0,
        fingerprint: format!("fingerprint-{entry_key}"),
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
        audio: None,
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

fn test_config(media_root: &std::path::Path, data_dir: std::path::PathBuf) -> ServerConfig {
    ServerConfig {
        media_roots: vec![swarm_media::roots::MediaRoot {
            label: "local".into(),
            path: media_root.to_path_buf(),
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir,
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    }
}

async fn negotiate(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    entry_key: &str,
) -> PlaybackPlan {
    client
        .post(format!("{base_url}/play/{entry_key}"))
        .bearer_auth(token)
        .json(&json!({
            "capabilities": swarm_core::capability::CapabilityProfile::fire_tv_baseline(),
            "start_position_secs": 0,
            "prefer_direct": true,
            "preview": false,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// #390's core contract: `playback_owner` must be a per-device identity
/// (`token_hash`), not the collidable, user-editable display name.
#[tokio::test]
async fn devices_sharing_a_display_name_do_not_share_playback_owner_identity_over_http() {
    let base = std::env::temp_dir().join(format!(
        "swarm-adv-390-owner-identity-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let core = ServerCore::start(test_config(&media_root, base.join("server-data")))
        .await
        .unwrap();
    core.wait_for_scan().await.unwrap();

    let relative_path = "movies/example.mp4";
    let media_bytes = vec![7u8; 1_000_000];
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, &media_bytes).unwrap();
    let entry = direct_play_entry(
        "0123456789abcdef00000390",
        relative_path,
        media_bytes.len() as u64,
    );
    core.library.upsert(&entry).await.unwrap();

    let base_url = format!("http://{}", core.http_media_addr);
    let client = reqwest::Client::new();

    // Two independently paired devices, deliberately given the identical
    // display name — a household with two unrenamed Rokus is not an edge
    // case, it's the default state of most homes.
    let token_a = pair_and_get_token(&client, &base_url, &core, "Roku").await;
    let token_b = pair_and_get_token(&client, &base_url, &core, "Roku").await;

    // Device A negotiates but never opens the plan (e.g. the response was
    // lost, or the viewer backed out before the player started streaming).
    // This reservation is only "stale" from A's own perspective; nothing
    // has any right to reap it except a later negotiation from A itself.
    let plan_a = negotiate(&client, &base_url, &token_a, &entry.entry_key).await;
    assert_eq!(plan_a.mode, PlaybackMode::Direct);

    // Device B — a distinct physical device with a distinct token, whose
    // owner only *looks* identical because of the shared display name —
    // now negotiates its own, independent playback.
    let plan_b = negotiate(&client, &base_url, &token_b, &entry.entry_key).await;
    assert_eq!(plan_b.mode, PlaybackMode::Direct);
    assert_ne!(
        plan_a.session_id, plan_b.session_id,
        "each device must get its own session"
    );

    // The decisive check: device A's still-unclaimed plan must still be
    // resolvable. If playback_owner were keyed on the shared display name,
    // B's negotiation would have called cancel_unclaimed_for_owner("Roku")
    // and reaped A's reservation, turning this into a 404.
    let a_media = client
        .get(format!("{base_url}{}", plan_a.path))
        .bearer_auth(&token_a)
        .send()
        .await
        .unwrap();
    assert_eq!(
        a_media.status(),
        200,
        "device B's negotiation must not cancel device A's reservation just because they share a display name"
    );

    // And B's own plan must independently be playable too.
    let b_media = client
        .get(format!("{base_url}{}", plan_b.path))
        .bearer_auth(&token_b)
        .send()
        .await
        .unwrap();
    assert_eq!(b_media.status(), 200);

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}

/// Complementary boundary: a same-name device's *own* retry must still
/// supersede its own abandoned reservation (the ordinary #390/#389
/// contract) — this is the negative-control half of the test above,
/// proving the isolation seen there is about owner identity and not, say,
/// an accidental global "never cancel anything" regression.
#[tokio::test]
async fn a_devices_own_retry_still_supersedes_its_own_abandoned_reservation_despite_a_same_named_sibling() {
    let base = std::env::temp_dir().join(format!(
        "swarm-adv-390-owner-retry-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let core = ServerCore::start(test_config(&media_root, base.join("server-data")))
        .await
        .unwrap();
    core.wait_for_scan().await.unwrap();

    let relative_path = "movies/example.mp4";
    let media_bytes = vec![7u8; 1_000_000];
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, &media_bytes).unwrap();
    let entry = direct_play_entry(
        "0123456789abcdef00000391",
        relative_path,
        media_bytes.len() as u64,
    );
    core.library.upsert(&entry).await.unwrap();

    let base_url = format!("http://{}", core.http_media_addr);
    let client = reqwest::Client::new();

    let token_a = pair_and_get_token(&client, &base_url, &core, "Roku").await;
    // A same-named sibling exists in the picture but never touches this
    // entry — it must have zero effect on A's own supersede behavior.
    let _token_sibling = pair_and_get_token(&client, &base_url, &core, "Roku").await;

    let first = negotiate(&client, &base_url, &token_a, &entry.entry_key).await;
    let retry = negotiate(&client, &base_url, &token_a, &entry.entry_key).await;
    assert_ne!(first.session_id, retry.session_id);

    let first_media = client
        .get(format!("{base_url}{}", first.path))
        .bearer_auth(&token_a)
        .send()
        .await
        .unwrap();
    assert_eq!(
        first_media.status(),
        404,
        "the same device's own retry must still supersede its own abandoned reservation"
    );

    let retry_media = client
        .get(format!("{base_url}{}", retry.path))
        .bearer_auth(&token_a)
        .send()
        .await
        .unwrap();
    assert_eq!(retry_media.status(), 200);

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}
