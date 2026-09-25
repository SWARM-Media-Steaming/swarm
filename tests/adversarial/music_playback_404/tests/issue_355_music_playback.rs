//! Issue #355: navigating to Music reports
//! `server could not prepare playback (404)`.
//!
//! Expected behavior, derived from the issue and playback domain rules
//! (not from the current implementation):
//!
//! 1. A catalogued music track whose file still exists must negotiate
//!    `/play` with HTTP 200. The TV client maps any other `/play` status
//!    onto the user-visible "server could not prepare playback (N)" error.
//! 2. 404 on `/play`/`/media` means "this library row has no readable file
//!    right now" (unknown key, deleted file, path is a directory). It is
//!    not a valid outcome for a row whose file is present under a
//!    different Unicode normalization of the same characters — SMB mounts
//!    on macOS are known to report NFD names for NFC catalog rows (and
//!    the reverse).
//! 3. Exact filesystem spelling wins. Normalization matching is only a
//!    fallback, and only when a single directory entry NFC-equals the
//!    missing component. Distinct files that merely look similar (Café vs
//!    Cafe, or two NFC-equivalent spellings that both exist) must not be
//!    substituted.
//! 4. A 404 caused by a file that is genuinely gone must still flip the
//!    row unavailable immediately (issue #73). A 404 must not be recorded
//!    for a file the resolver actually found.
//! 5. `/play` and the subsequent byte-serving routes (`/media`,
//!    `/stream/{session}/media`) share that resolution. Negotiation
//!    succeeding then 404ing on the first byte read is still a failed play.

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{
    AudioStreamInfo, ByteRange, MediaKind, PeerRequest, PlaybackMode, PlaybackPlan,
    PlaybackPreferences,
};
use swarm_media::roots::{MediaRoot, MediaRootAssetType, RootResolver, SharedRootResolver};
use swarm_media::scan::scan_roots;
use swarm_media::serve::{stream_body, Body, MediaService, Resolved};
use swarm_media::store::{EntryRecord, Library};
use swarm_media::transcode::TranscodeConfig;

const NFC_CAFE: &str = "Caf\u{e9}";
const NFD_CAFE: &str = "Cafe\u{301}";
const NFC_HANGUL: &str = "\u{ac01}";
const NFD_HANGUL: &str = "\u{1100}\u{1161}\u{11a8}";

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
        "swarm-adv-355-{tag}-{}-{n}",
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

fn host_treats_nfc_nfd_as_same(dir: &Path) -> bool {
    let probe = dir.join("_nfc_nfd_probe");
    let _ = std::fs::remove_dir_all(&probe);
    std::fs::create_dir_all(&probe).unwrap();
    let nfd_dir = probe.join(NFD_CAFE);
    std::fs::create_dir(&nfd_dir).unwrap();
    let same = probe.join(NFC_CAFE).is_dir();
    let _ = std::fs::remove_dir_all(&probe);
    same
}

fn nfc_nfd_are_distinct_strings() {
    assert_ne!(
        NFC_CAFE, NFD_CAFE,
        "fixture invariant: NFC and NFD Café must be different UTF-8"
    );
    assert_ne!(
        NFC_HANGUL, NFD_HANGUL,
        "fixture invariant: NFC and NFD Hangul syllable must be different UTF-8"
    );
}

fn transcode_config(base: &Path) -> TranscodeConfig {
    TranscodeConfig {
        enabled: false,
        ffmpeg_path: "ffmpeg".into(),
        session_dir: base.join("sessions"),
        max_upload_bps: 10_000_000,
        reserve_percent: 30,
        max_sessions: 4,
        idle_timeout: Duration::from_secs(300),
        segment_duration_secs: 4,
        ..Default::default()
    }
}

fn service(fx: &Fixture) -> Arc<MediaService> {
    Arc::new(MediaService::with_transcoding(
        Arc::clone(&fx.library),
        fx.media_root.clone(),
        transcode_config(&fx.base),
    ))
}

fn service_with_roots(fx: &Fixture, roots: Vec<MediaRoot>) -> Arc<MediaService> {
    Arc::new(MediaService::with_roots(
        Arc::clone(&fx.library),
        SharedRootResolver::new(RootResolver::new(roots)),
        transcode_config(&fx.base),
    ))
}

fn track_entry(entry_key: &str, relative_path: &str, size: u64) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Track,
        title: "Track".into(),
        size,
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

fn media_request(entry_key: &str, range: Option<ByteRange>) -> PeerRequest {
    PeerRequest {
        path: format!("/media/{entry_key}"),
        range,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

async fn body_bytes(service: &Arc<MediaService>, resolved: Resolved) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stream = std::pin::pin!(stream_body(resolved, service));
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("byte stream must stay readable"));
    }
    out
}

async fn assert_play_ok(service: &Arc<MediaService>, entry_key: &str) -> PlaybackPlan {
    let resolved = service.resolve(&play_request(entry_key)).await;
    assert_eq!(
        resolved.header.status, 200,
        "catalogued music whose file exists must negotiate playback; got {} (this is the issue #355 client error)",
        resolved.header.status
    );
    let Body::Bytes(body) = resolved.body else {
        panic!("/play must return a JSON playback plan");
    };
    let plan: PlaybackPlan = serde_json::from_slice(&body).unwrap();
    assert_eq!(plan.mode, PlaybackMode::Direct);
    assert!(
        plan.path.starts_with("/stream/") && plan.path.ends_with("/media"),
        "direct music play must hand the client a session media path, got {}",
        plan.path
    );
    plan
}

async fn assert_still_available(library: &Library, entry_key: &str) {
    assert!(
        library.get(entry_key).await.unwrap().is_some(),
        "finding the file must not mark the music row missing"
    );
}

#[tokio::test]
async fn ascii_plex_music_layout_prepares_playback_after_scan() {
    let fx = fixture("ascii-scan").await;
    let relative = "Music/Boards of Canada/Music Has the Right to Children/01 - Wildlife Analysis.m4a";
    let payload = vec![9u8; 8_192];
    write_file(&fx.media_root, relative, &payload);
    scan_roots(
        fx.library.as_ref(),
        &[MediaRoot {
            label: "local".into(),
            path: fx.media_root.clone(),
            asset_type: MediaRootAssetType::Music,
        }],
        None,
    )
    .await
    .unwrap();

    let tracks: Vec<_> = fx
        .library
        .list()
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == MediaKind::Track)
        .collect();
    assert_eq!(
        tracks.len(),
        1,
        "a Music root with one audio file must catalog one track"
    );
    let entry_key = tracks[0].entry_key.clone();

    let service = service(&fx);
    let plan = assert_play_ok(&service, &entry_key).await;
    let streamed = service
        .resolve(&PeerRequest {
            path: plan.path.clone(),
            range: None,
            if_none_match: None,
            playback: None,
            error_report: None,
            like: None,
        })
        .await;
    assert_eq!(streamed.header.status, 200);
    assert_eq!(body_bytes(&service, streamed).await, payload);
}

#[tokio::test]
async fn nfc_catalog_nfd_directory_prepares_music_playback() {
    nfc_nfd_are_distinct_strings();
    let fx = fixture("nfc-catalog").await;
    let fs_relative = format!("Music/{NFD_CAFE} Tacvba/Re/01 - El Aparato.m4a");
    let catalog_relative = format!("Music/{NFC_CAFE} Tacvba/Re/01 - El Aparato.m4a");
    let payload = b"nfc-catalog-nfd-dir".repeat(400);
    write_file(&fx.media_root, &fs_relative, &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa01";
    fx.library
        .upsert(&track_entry(entry_key, &catalog_relative, payload.len() as u64))
        .await
        .unwrap();

    let resolver = RootResolver::single(fx.media_root.clone());
    let resolved_path = resolver.resolve_existing(&catalog_relative);
    assert!(
        resolved_path.is_file(),
        "NFC catalog path must find the NFD directory name an SMB listing can return"
    );

    let service = service(&fx);
    let plan = assert_play_ok(&service, entry_key).await;
    assert_still_available(&fx.library, entry_key).await;

    let streamed = service
        .resolve(&PeerRequest {
            path: plan.path,
            range: None,
            if_none_match: None,
            playback: None,
            error_report: None,
            like: None,
        })
        .await;
    assert_eq!(
        streamed.header.status, 200,
        "session byte serving must use the same existing-file resolution as /play"
    );
    assert_eq!(body_bytes(&service, streamed).await, payload);
}

#[tokio::test]
async fn nfd_catalog_nfc_directory_prepares_music_playback() {
    nfc_nfd_are_distinct_strings();
    let fx = fixture("nfd-catalog").await;
    let fs_relative = format!("Music/{NFC_CAFE} Tacvba/Re/01 - El Aparato.m4a");
    let catalog_relative = format!("Music/{NFD_CAFE} Tacvba/Re/01 - El Aparato.m4a");
    let payload = b"nfd-catalog-nfc-dir".repeat(400);
    write_file(&fx.media_root, &fs_relative, &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa02";
    fx.library
        .upsert(&track_entry(entry_key, &catalog_relative, payload.len() as u64))
        .await
        .unwrap();

    let service = service(&fx);
    assert_play_ok(&service, entry_key).await;
    assert_still_available(&fx.library, entry_key).await;
}

#[tokio::test]
async fn unicode_mismatch_in_the_filename_prepares_playback() {
    let fx = fixture("nfd-filename").await;
    let fs_relative = format!("Music/Artist/Album/01 - {NFD_CAFE}.m4a");
    let catalog_relative = format!("Music/Artist/Album/01 - {NFC_CAFE}.m4a");
    let payload = vec![3u8; 4_096];
    write_file(&fx.media_root, &fs_relative, &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa03";
    fx.library
        .upsert(&track_entry(entry_key, &catalog_relative, payload.len() as u64))
        .await
        .unwrap();

    let service = service(&fx);
    assert_play_ok(&service, entry_key).await;

    let media = service.resolve(&media_request(entry_key, None)).await;
    assert_eq!(media.header.status, 200);
    assert_eq!(body_bytes(&service, media).await, payload);
}

#[tokio::test]
async fn hangul_syllable_normalization_mismatch_prepares_playback() {
    let fx = fixture("hangul").await;
    let fs_relative = format!("Music/{NFD_HANGUL} Artist/Album/01 - Track.m4a");
    let catalog_relative = format!("Music/{NFC_HANGUL} Artist/Album/01 - Track.m4a");
    let payload = vec![11u8; 2_048];
    write_file(&fx.media_root, &fs_relative, &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa04";
    fx.library
        .upsert(&track_entry(entry_key, &catalog_relative, payload.len() as u64))
        .await
        .unwrap();

    let service = service(&fx);
    assert_play_ok(&service, entry_key).await;
}

#[tokio::test]
async fn every_path_component_can_differ_in_normalization() {
    let fx = fixture("all-components").await;
    let fs_relative = format!("Music/{NFD_CAFE}/{NFD_CAFE}/{NFD_CAFE}.m4a");
    let catalog_relative = format!("Music/{NFC_CAFE}/{NFC_CAFE}/{NFC_CAFE}.m4a");
    let payload = vec![5u8; 1_024];
    write_file(&fx.media_root, &fs_relative, &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa05";
    fx.library
        .upsert(&track_entry(entry_key, &catalog_relative, payload.len() as u64))
        .await
        .unwrap();

    let service = service(&fx);
    assert_play_ok(&service, entry_key).await;
    let media = service.resolve(&media_request(entry_key, None)).await;
    assert_eq!(body_bytes(&service, media).await, payload);
}

#[tokio::test]
async fn range_request_after_unicode_resolve_returns_the_right_bytes() {
    let fx = fixture("range").await;
    let fs_relative = format!("Music/{NFD_CAFE}/Album/track.m4a");
    let catalog_relative = format!("Music/{NFC_CAFE}/Album/track.m4a");
    let payload: Vec<u8> = (0..200).collect();
    write_file(&fx.media_root, &fs_relative, &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa06";
    fx.library
        .upsert(&track_entry(entry_key, &catalog_relative, payload.len() as u64))
        .await
        .unwrap();

    let service = service(&fx);
    let media = service
        .resolve(&media_request(
            entry_key,
            Some(ByteRange::FromTo {
                start: 10,
                end: Some(19),
            }),
        ))
        .await;
    assert_eq!(media.header.status, 206);
    assert_eq!(body_bytes(&service, media).await, payload[10..20]);
}

#[tokio::test]
async fn cafe_without_accent_is_not_substituted_for_cafe_with_accent() {
    let fx = fixture("lookalike").await;
    write_file(
        &fx.media_root,
        "Music/Cafe/Album/track.m4a",
        b"wrong-file-without-accent",
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa07";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/Album/track.m4a"),
            8,
        ))
        .await
        .unwrap();

    let service = service(&fx);
    let resolved = service.resolve(&play_request(entry_key)).await;
    assert_eq!(
        resolved.header.status, 404,
        "NFC matching is not fuzzy filename matching: Cafe is not Café"
    );
}

#[tokio::test]
async fn deleted_music_file_still_404s_and_is_marked_missing() {
    let fx = fixture("deleted").await;
    let relative = "Music/Artist/Album/01 - Gone.m4a";
    let path = write_file(&fx.media_root, relative, &[7u8; 1_024]);
    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa08";
    fx.library
        .upsert(&track_entry(entry_key, relative, 1_024))
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();

    let service = service(&fx);
    let play = service.resolve(&play_request(entry_key)).await;
    assert_eq!(play.header.status, 404);
    assert!(
        fx.library.get(entry_key).await.unwrap().is_none(),
        "issue #73: a genuine miss must still hide the stale music row"
    );

    let media = service.resolve(&media_request(entry_key, None)).await;
    assert_eq!(media.header.status, 404);
}

#[tokio::test]
async fn unicode_hit_must_not_mark_the_row_missing() {
    let fx = fixture("not-missing").await;
    let fs_relative = format!("Music/{NFD_CAFE}/Album/track.m4a");
    let catalog_relative = format!("Music/{NFC_CAFE}/Album/track.m4a");
    write_file(&fx.media_root, &fs_relative, &[1u8; 512]);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa09";
    fx.library
        .upsert(&track_entry(entry_key, &catalog_relative, 512))
        .await
        .unwrap();

    let service = service(&fx);
    assert_eq!(
        service.resolve(&play_request(entry_key)).await.header.status,
        200
    );
    assert_eq!(
        service
            .resolve(&media_request(entry_key, None))
            .await
            .header
            .status,
        200
    );
    assert_still_available(&fx.library, entry_key).await;
}

#[tokio::test]
async fn directory_at_the_catalog_path_is_404() {
    let fx = fixture("isdir").await;
    let relative = "Music/Artist/Album/track.m4a";
    std::fs::create_dir_all(fx.media_root.join(relative)).unwrap();

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0a";
    fx.library
        .upsert(&track_entry(entry_key, relative, 1))
        .await
        .unwrap();

    let service = service(&fx);
    assert_eq!(
        service.resolve(&play_request(entry_key)).await.header.status,
        404,
        "a directory is not a playable music file"
    );
}

#[tokio::test]
async fn unknown_and_malformed_entry_keys_are_404() {
    let fx = fixture("keys").await;
    let service = service(&fx);

    let unknown = service
        .resolve(&play_request("bbbbbbbbbbbbbbbbbbbbbbbb"))
        .await;
    assert_eq!(unknown.header.status, 404);

    let malformed = service.resolve(&play_request("not-a-valid-entry-key")).await;
    assert_eq!(malformed.header.status, 404);

    let empty = service
        .resolve(&PeerRequest {
            path: "/play/".into(),
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
        })
        .await;
    assert_eq!(empty.header.status, 404);
}

#[tokio::test]
async fn missing_playback_preferences_is_not_a_file_missing_404() {
    let fx = fixture("prefs").await;
    let relative = "Music/Artist/Album/01 - Track.m4a";
    write_file(&fx.media_root, relative, &[2u8; 256]);
    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0b";
    fx.library
        .upsert(&track_entry(entry_key, relative, 256))
        .await
        .unwrap();

    let service = service(&fx);
    let resolved = service
        .resolve(&PeerRequest {
            path: format!("/play/{entry_key}"),
            range: None,
            if_none_match: None,
            playback: None,
            error_report: None,
            like: None,
        })
        .await;
    assert_ne!(
        resolved.header.status, 404,
        "a well-known track with a missing PlaybackPreferences object is a client error, not a missing file"
    );
    assert_eq!(resolved.header.status, 400);
    assert_still_available(&fx.library, entry_key).await;
}

#[tokio::test]
async fn multi_root_labeled_music_path_survives_unicode_mismatch() {
    let fx = fixture("multi-root").await;
    let nas = fx.base.join("nas");
    std::fs::create_dir_all(&nas).unwrap();
    let fs_relative = format!("Music/{NFD_CAFE}/Album/track.m4a");
    let catalog_relative = format!("nas/Music/{NFC_CAFE}/Album/track.m4a");
    let payload = vec![8u8; 3_000];
    write_file(&nas, &fs_relative, &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0c";
    fx.library
        .upsert(&track_entry(entry_key, &catalog_relative, payload.len() as u64))
        .await
        .unwrap();

    let service = service_with_roots(
        &fx,
        vec![
            MediaRoot {
                label: "local".into(),
                path: fx.media_root.clone(),
                asset_type: MediaRootAssetType::Music,
            },
            MediaRoot {
                label: "nas".into(),
                path: nas,
                asset_type: MediaRootAssetType::Music,
            },
        ],
    );
    assert_play_ok(&service, entry_key).await;
    let media = service.resolve(&media_request(entry_key, None)).await;
    assert_eq!(media.header.status, 200);
    assert_eq!(body_bytes(&service, media).await, payload);
}

#[tokio::test]
async fn empty_music_file_still_prepares_playback() {
    let fx = fixture("empty").await;
    let relative = "Music/Artist/Album/01 - Silence.m4a";
    write_file(&fx.media_root, relative, b"");
    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0d";
    fx.library
        .upsert(&track_entry(entry_key, relative, 0))
        .await
        .unwrap();

    let service = service(&fx);
    assert_play_ok(&service, entry_key).await;
    let media = service.resolve(&media_request(entry_key, None)).await;
    assert_eq!(media.header.status, 200);
    assert!(body_bytes(&service, media).await.is_empty());
}

#[tokio::test]
async fn ambiguous_nfc_equivalent_names_are_not_silently_chosen() {
    let fx = fixture("ambiguous").await;
    if host_treats_nfc_nfd_as_same(&fx.base) {
        // APFS (and similar) cannot store both spellings. The refuse-to-pick
        // branch is only observable on a normalization-sensitive filesystem
        // such as the SMB mount that motivated the issue.
        return;
    }

    let parent = fx.media_root.join("Music/Artist/Album");
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::write(parent.join(format!("{NFC_CAFE}.m4a")), b"nfc-bytes").unwrap();
    std::fs::write(parent.join(format!("{NFD_CAFE}.m4a")), b"nfd-bytes").unwrap();
    assert_eq!(
        std::fs::read_dir(&parent).unwrap().count(),
        2,
        "this host must actually store both NFC and NFD names for the ambiguity case"
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0e";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/Artist/Album/{NFC_CAFE}.m4a"),
            9,
        ))
        .await
        .unwrap();

    // Exact NFC spelling exists, so exact-path wins and playback must succeed.
    let service = service(&fx);
    assert_play_ok(&service, entry_key).await;
    let media = service.resolve(&media_request(entry_key, None)).await;
    assert_eq!(body_bytes(&service, media).await, b"nfc-bytes");

    // If the exact catalog spelling is missing but two NFC-equivalent
    // siblings remain, the resolver must not pick either of them.
    std::fs::remove_file(parent.join(format!("{NFC_CAFE}.m4a"))).unwrap();
    std::fs::write(parent.join(format!("Cafe\u{301}\u{301}.m4a")), b"other").ok();
    let nfd_only_key = "aaaaaaaaaaaaaaaaaaaaaa0f";
    // Recreate two distinct NFC-equal names without an exact catalog match:
    // remaining NFD file plus a second NFD-equivalent sibling if the FS allows.
    let listing: Vec<_> = std::fs::read_dir(&parent)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    if listing.len() >= 2 {
        fx.library
            .upsert(&track_entry(
                nfd_only_key,
                "Music/Artist/Album/missing-exact.m4a",
                1,
            ))
            .await
            .unwrap();
        let resolved = service.resolve(&play_request(nfd_only_key)).await;
        assert_eq!(
            resolved.header.status, 404,
            "an unambiguous file is required; two NFC-equal names must not be auto-picked"
        );
    }
}

#[tokio::test]
async fn resolve_existing_falls_back_only_when_exact_join_misses() {
    let fx = fixture("resolver-api").await;
    let fs_relative = format!("Music/{NFD_CAFE}/Album/track.m4a");
    let catalog_relative = format!("Music/{NFC_CAFE}/Album/track.m4a");
    write_file(&fx.media_root, &fs_relative, b"audio");

    let resolver = RootResolver::single(fx.media_root.clone());
    let exact = resolver.resolve(&catalog_relative);
    let existing = resolver.resolve_existing(&catalog_relative);
    assert!(
        existing.is_file(),
        "resolve_existing must locate the music file"
    );
    if !host_treats_nfc_nfd_as_same(&fx.base) {
        assert!(
            !exact.is_file(),
            "on a normalization-sensitive filesystem the raw join of an NFC catalog path must miss an NFD file — that miss is the 404 the client saw"
        );
        assert_ne!(exact, existing);
    }

    assert!(!resolver
        .resolve_existing("Music/DoesNotExist/track.m4a")
        .is_file());
}

// --- Trusted #355 amendment follow-up ---------------------------------
//
// The reporter's own post-fix log (a track named "Solar Movement - Pure
// Soul (Dark Mix)", entry_key `ebe89f5bd288b591f9f6ddcd`) shows the SAME
// entry failing `/play` negotiation three separate times — 19:23:32,
// 19:23:38, 19:23:44 (client-driven retries roughly 6s apart) — plus two
// other, unrelated entries failing the same way earlier in the same
// session (19:22:56, 19:23:10). A per-request retry bounded at ~1.9s
// worst case (`RESOLVE_EXISTING_RETRIES`/`RESOLVE_EXISTING_RETRY_DELAY` in
// `serve.rs`) cannot explain three independent 404s spread across 12+
// seconds for the *same* file unless something outlives that budget and
// then persists. `serve.rs::mark_entry_missing` -> `store.rs::
// mark_missing_by_path` explains it: the very first miss (available != 0)
// flips `available = 0` unconditionally, with no confirmation window —
// the "grace" parameter only gates repeat *scan-time* misses after the
// row is already unavailable. `MediaService::play`/`media` both call
// `self.library.get(entry_key)` — which filters `available = 1` — *before*
// ever reaching `resolve_existing_media_file`'s retry. So one outage
// longer than ~1.9s latches the row 404 for every later request, no
// matter how quickly the file actually comes back, until a full rescan
// restores it (`restore_available_by_path` is only ever called from
// `scan_roots`; the automatic background rescan is
// `AUTO_LIBRARY_WATCH_INTERVAL` = 15 minutes in `apps/server/src/gui.rs`,
// otherwise a manual rescan). That is a far better match for the
// amendment's evidence than a sub-two-second blip, and it is not covered
// by the retry-timing test already added for this issue
// (`crates/swarm-media/tests/playback.rs::
// transient_missing_file_still_negotiates_playback_once_it_appears`),
// which only ever calls `resolve` once and never revisits the same entry
// after recovery.

#[tokio::test]
async fn one_outage_past_the_retry_budget_latches_the_row_404_after_the_file_recovers() {
    let fx = fixture("latch").await;
    // `.m4a`, not `.mp3`: this test's `service(&fx)` fixture disables
    // transcoding and negotiates with `fire_tv_baseline()`, whose
    // `containers` are only `["mp4", "hls"]` — an `.mp3` extension maps to
    // container `"mp3"` in `direct_compatible()` (transcode.rs), which that
    // profile never lists, so it would 503 on codec/container mismatch
    // regardless of the availability-latch behavior under test here. `.m4a`
    // maps to `"mp4"` and stays direct-play-compatible, matching the
    // sibling `playback.rs` fixtures that already avoid `.mp3` for the same
    // reason.
    let relative = "Music/Solarstone/Pure Trance Vol 8/06 - Solar Movement.m4a";
    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa10";
    fx.library
        .upsert(&track_entry(entry_key, relative, 9_201_596))
        .await
        .unwrap();

    // The file is not present yet and stays absent past the request-time
    // retry budget (~1.9s worst case), so this first negotiation must
    // exhaust its retries and self-heal the catalog exactly as issue #73
    // requires for a genuine miss.
    let service = service(&fx);
    let first = service.resolve(&play_request(entry_key)).await;
    assert_eq!(
        first.header.status, 404,
        "sanity check: the file must still be absent when the first negotiation gives up"
    );
    assert!(
        fx.library.get(entry_key).await.unwrap().is_none(),
        "an outage that outlasts the retry budget must still flip the row unavailable"
    );

    // The underlying condition now fully recovers — not a further glitch,
    // an ordinary, indefinitely-present file, matching what the reporting
    // server's own SMB mount looks like once it reconnects.
    write_file(&fx.media_root, &relative, &vec![9u8; 4_096]);
    assert!(fx.media_root.join(&relative).is_file());

    let second = service.resolve(&play_request(entry_key)).await;
    assert_eq!(
        second.header.status, 200,
        "a catalogued track whose file exists right now must negotiate playback — a stale \
         `available = 0` latch left over from one past outage must not keep 404ing it forever. \
         This is the pattern in the #355 trusted amendment: the same track kept 404ing across \
         three separate client negotiation attempts 6-22s apart even though the share was not \
         permanently gone."
    );
    assert!(
        fx.library.get(entry_key).await.unwrap().is_some(),
        "a recovered file must be usable again without waiting for a full library rescan"
    );
}

#[tokio::test]
async fn permission_denied_from_a_reconnecting_mount_is_not_retried_like_not_found_is() {
    // `resolve_existing_media_file` (serve.rs) only widens the retry
    // condition to `ErrorKind::NotFound`. A busy/reconnecting network
    // mount does not reliably surface as ENOENT: on macOS/Linux, a
    // directory that is transiently unreadable mid-reconnect makes a stat
    // of anything inside it fail with EACCES (`PermissionDenied`)
    // instead, because the traversal itself is denied, not because the
    // entry is absent. That failure mode gets none of the retry the
    // NotFound case gets, and 404s (and latches, per the test above)
    // immediately.
    let fx = fixture("perm-denied").await;
    let parent = fx.media_root.join("Music/Artist/Album");
    std::fs::create_dir_all(&parent).unwrap();
    let relative = "Music/Artist/Album/01 - Track.m4a";
    write_file(&fx.media_root, relative, &[4u8; 2_048]);

    #[cfg(not(unix))]
    {
        eprintln!(
            "SKIP: permission_denied_from_a_reconnecting_mount_is_not_retried_like_not_found_is \
             requires a unix host to simulate EACCES via directory permissions"
        );
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Feature-detect that this process actually has directory
        // permissions enforced against it (root, and some sandboxes,
        // bypass DAC entirely) before relying on it to simulate a busy
        // mount, the same portability guard `host_treats_nfc_nfd_as_same`
        // already uses for the Unicode fallback tests above.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
        let blocked = std::fs::metadata(fx.media_root.join(relative)).is_err();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !blocked {
            eprintln!(
                "SKIP: directory permissions are not enforced for this process (root or a \
                 permission-bypassing sandbox); cannot simulate EACCES"
            );
            return;
        }

        let entry_key = "aaaaaaaaaaaaaaaaaaaaaa11";
        fx.library
            .upsert(&track_entry(entry_key, relative, 2_048))
            .await
            .unwrap();

        let service = service(&fx);

        // Block the directory, then let a background task restore it
        // partway through the retry budget — mirroring a mount that
        // reconnects mid-request. If PermissionDenied were retried the
        // same way NotFound is, this would still resolve to 200.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
        let restore_parent = parent.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = std::fs::set_permissions(&restore_parent, std::fs::Permissions::from_mode(0o755));
        });

        let resolved = service.resolve(&play_request(entry_key)).await;
        let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755));

        assert_eq!(
            resolved.header.status, 200,
            "a transient EACCES from a reconnecting mount (not just ENOENT) must still recover \
             within the retry budget — resolve_existing_media_file only retries `ErrorKind::NotFound` today"
        );
    }
}
