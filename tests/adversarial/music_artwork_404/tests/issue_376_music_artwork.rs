//! Issue #376: music artwork still 404s when the catalogued cover
//! exists on disk under a different Unicode normalization.
//!
//! Expected behavior, derived from the issue, the #355 playback
//! invariant, and how TV clients actually load covers — not from the
//! current implementation:
//!
//! 1. `GET /art/{entry_key}/cover` is how Fire TV catalog cards and
//!    now-playing screens fetch a track's image. A 404 is a blank cover,
//!    not the playback-prep toast from #355, but it is still a miss of a
//!    file the library claims to have.
//! 2. Artwork relative paths are catalog values, same as track
//!    `relative_path`. SMB mounts on macOS can list an NFD directory
//!    (Cafe\u{301}/cover.jpg) for a row that stored NFC (Café/cover.jpg),
//!    and the reverse. Exact `root.join(relative).is_file()` is not
//!    evidence the cover is gone.
//! 3. Exact filesystem spelling still wins. Normalization matching is
//!    only a fallback, and only when a single directory entry NFC-equals
//!    the missing component. `Cafe` is not `Café`.
//! 4. An artwork 404 must not hide the music row. Issue #73's
//!    missing-file self-heal is a playback concern; a missing cover is
//!    just a missing image.
//! 5. The TV client always requests shelf covers with `?w=320` and
//!    detail/now-playing covers without `w`. Both must resolve the same
//!    catalog path. Artist-photo fallback (#277) reuses a cover path and
//!    must survive the same spelling mismatch.
//! 6. A genuine miss (unknown key, unknown kind, no artwork row,
//!    deleted file) is still 404.

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use swarm_core::peer::{AudioStreamInfo, ByteRange, MediaKind, PeerRequest};
use swarm_media::roots::{MediaRoot, MediaRootAssetType, RootResolver, SharedRootResolver};
use swarm_media::serve::{stream_body, Body, MediaService, Resolved};
use swarm_media::store::{ArtworkKind, EntryRecord, Library};
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
        "swarm-adv-376-{tag}-{}-{n}",
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

fn write_png(root: &Path, relative: &str, color: [u8; 3]) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(64, 64, image::Rgb(color))
        .save(&path)
        .unwrap();
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

fn track_entry(entry_key: &str, relative_path: &str) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Track,
        title: "Track".into(),
        size: 1_024,
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

fn movie_entry(entry_key: &str, relative_path: &str) -> EntryRecord {
    let mut entry = track_entry(entry_key, relative_path);
    entry.kind = MediaKind::Movie;
    entry.title = "Movie".into();
    entry.artist = None;
    entry.album = None;
    entry.track_number = None;
    entry.audio = None;
    entry
}

fn art_request(path: String, range: Option<ByteRange>, if_none_match: Option<String>) -> PeerRequest {
    PeerRequest {
        path,
        range,
        if_none_match,
        playback: None,
        error_report: None,
        like: None,
    }
}

async fn body_bytes(service: &Arc<MediaService>, resolved: Resolved) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stream = std::pin::pin!(stream_body(resolved, service));
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("artwork byte stream must stay readable"));
    }
    out
}

async fn upsert_cover(fx: &Fixture, entry_key: &str, catalog_cover: &str) {
    fx.library
        .set_artwork(entry_key, ArtworkKind::Cover, catalog_cover)
        .await
        .unwrap();
}

async fn assert_still_available(library: &Library, entry_key: &str) {
    assert!(
        library.get(entry_key).await.unwrap().is_some(),
        "a cover miss or hit must not hide the music row"
    );
}

async fn assert_art_bytes(
    service: &Arc<MediaService>,
    path: String,
    expected: &[u8],
    why: &str,
) {
    let resolved = service.resolve(&art_request(path, None, None)).await;
    assert_eq!(
        resolved.header.status, 200,
        "{why}; got {}",
        resolved.header.status
    );
    assert_eq!(body_bytes(service, resolved).await, expected, "{why}");
}

#[tokio::test]
async fn nfc_catalog_nfd_directory_serves_music_cover() {
    nfc_nfd_are_distinct_strings();
    let fx = fixture("nfc-catalog").await;
    let fs_cover = format!("Music/{NFD_CAFE}/cover.jpg");
    let catalog_cover = format!("Music/{NFC_CAFE}/cover.jpg");
    let payload = b"nfc-catalog-nfd-cover".repeat(20);
    write_file(&fx.media_root, &format!("Music/{NFD_CAFE}/track.m4a"), &[7u8; 1_024]);
    write_file(&fx.media_root, &fs_cover, &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa01";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &catalog_cover).await;

    let resolver = RootResolver::single(fx.media_root.clone());
    assert!(
        resolver.resolve_existing(&catalog_cover).is_file(),
        "NFC catalog artwork path must find the NFD directory name an SMB listing can return"
    );
    if !host_treats_nfc_nfd_as_same(&fx.base) {
        assert!(
            !resolver.resolve(&catalog_cover).is_file(),
            "on a normalization-sensitive filesystem the raw join of an NFC cover path must miss the NFD file — that miss is the 404 this issue describes"
        );
    }

    let service = service(&fx);
    let resolved = service
        .resolve(&art_request(format!("/art/{entry_key}/cover"), None, None))
        .await;
    assert_eq!(
        resolved.header.status, 200,
        "a music cover whose Unicode spelling differs from the SMB path must still be served; got {}",
        resolved.header.status
    );
    let served_path = match &resolved.body {
        Body::File { path, .. } => path.clone(),
        _ => panic!("artwork must resolve to a file"),
    };
    if !host_treats_nfc_nfd_as_same(&fx.base) {
        assert_eq!(
            served_path,
            resolver.resolve_existing(&catalog_cover),
            "served cover path must be the existing-file resolution, not the raw NFC join"
        );
    }
    assert_eq!(body_bytes(&service, resolved).await, payload);
    assert_still_available(&fx.library, entry_key).await;
}

#[tokio::test]
async fn nfd_catalog_nfc_directory_serves_music_cover() {
    nfc_nfd_are_distinct_strings();
    let fx = fixture("nfd-catalog").await;
    let payload = b"nfd-catalog-nfc-cover".repeat(20);
    write_file(
        &fx.media_root,
        &format!("Music/{NFC_CAFE}/cover.jpg"),
        &payload,
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa02";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFD_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFD_CAFE}/cover.jpg")).await;

    let service = service(&fx);
    assert_art_bytes(
        &service,
        format!("/art/{entry_key}/cover"),
        &payload,
        "NFD catalog cover path must find the NFC directory an APFS/HFS listing can return",
    )
    .await;
}

#[tokio::test]
async fn unicode_mismatch_in_the_cover_filename_is_served() {
    let fx = fixture("nfd-filename").await;
    let payload = b"filename-mismatch-cover".repeat(10);
    write_file(
        &fx.media_root,
        &format!("Music/Artist/Album/{NFD_CAFE}.jpg"),
        &payload,
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa03";
    fx.library
        .upsert(&track_entry(entry_key, "Music/Artist/Album/01 - Track.m4a"))
        .await
        .unwrap();
    upsert_cover(
        &fx,
        entry_key,
        &format!("Music/Artist/Album/{NFC_CAFE}.jpg"),
    )
    .await;

    let service = service(&fx);
    assert_art_bytes(
        &service,
        format!("/art/{entry_key}/cover"),
        &payload,
        "cover filename NFC/NFD mismatch must still serve the image",
    )
    .await;
}

#[tokio::test]
async fn hangul_syllable_normalization_mismatch_serves_cover() {
    let fx = fixture("hangul").await;
    let payload = b"hangul-cover".repeat(8);
    write_file(
        &fx.media_root,
        &format!("Music/{NFD_HANGUL}/cover.jpg"),
        &payload,
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa04";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_HANGUL}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_HANGUL}/cover.jpg")).await;

    let service = service(&fx);
    assert_art_bytes(
        &service,
        format!("/art/{entry_key}/cover"),
        &payload,
        "Hangul NFC catalog cover must find the NFD directory spelling",
    )
    .await;
}

#[tokio::test]
async fn every_cover_path_component_can_differ_in_normalization() {
    let fx = fixture("all-components").await;
    let payload = b"all-components-cover".repeat(4);
    write_file(
        &fx.media_root,
        &format!("Music/{NFD_CAFE}/{NFD_CAFE}/{NFD_CAFE}.jpg"),
        &payload,
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa05";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(
        &fx,
        entry_key,
        &format!("Music/{NFC_CAFE}/{NFC_CAFE}/{NFC_CAFE}.jpg"),
    )
    .await;

    let service = service(&fx);
    assert_art_bytes(
        &service,
        format!("/art/{entry_key}/cover"),
        &payload,
        "every path component of a cover can differ in Unicode normalization",
    )
    .await;
}

#[tokio::test]
async fn tv_card_thumbnail_query_serves_cover_after_unicode_resolve() {
    let fx = fixture("thumb").await;
    write_png(
        &fx.media_root,
        &format!("Music/{NFD_CAFE}/cover.png"),
        [40, 80, 120],
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa06";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_CAFE}/cover.png")).await;

    let service = service(&fx);
    // Fire TV catalog cards request `?v=$etag&w=320` (SwarmViewModel.artworkUrl).
    let card = service
        .resolve(&art_request(
            format!("/art/{entry_key}/cover?v=v1&w=320"),
            None,
            None,
        ))
        .await;
    assert_eq!(
        card.header.status, 200,
        "TV shelf covers are requested with w=320; that thumbnail must not 404 on an NFC/NFD cover path"
    );
    assert_eq!(card.header.content_type.as_deref(), Some("image/jpeg"));
    let Body::File { path, .. } = card.body else {
        panic!("thumbnail must be file-backed");
    };
    let thumb = image::open(&path).unwrap();
    assert_eq!(thumb.width(), 320);

    let full = service
        .resolve(&art_request(format!("/art/{entry_key}/cover"), None, None))
        .await;
    assert_eq!(
        full.header.status, 200,
        "detail/now-playing full cover must still be served after a thumbnail request"
    );
}

#[tokio::test]
async fn artist_photo_fallback_uses_unicode_resolved_cover() {
    let fx = fixture("artist-fallback").await;
    let payload = b"fallback-cover-bytes".repeat(6);
    write_file(
        &fx.media_root,
        &format!("Music/{NFD_CAFE}/cover.jpg"),
        &payload,
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa07";
    let mut entry = track_entry(entry_key, &format!("Music/{NFC_CAFE}/track.m4a"));
    entry.artist = Some("Cafe Tacvba".into());
    fx.library.upsert(&entry).await.unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_CAFE}/cover.jpg")).await;

    let service = service(&fx);
    assert_art_bytes(
        &service,
        format!("/art/{entry_key}/artist"),
        &payload,
        "/art/.../artist falls back to album cover (#277) and that cover path has the same NFC/NFD problem",
    )
    .await;
}

#[tokio::test]
async fn range_request_after_unicode_resolve_returns_the_right_cover_bytes() {
    let fx = fixture("range").await;
    let payload: Vec<u8> = (0..200).collect();
    write_file(
        &fx.media_root,
        &format!("Music/{NFD_CAFE}/cover.jpg"),
        &payload,
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa08";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_CAFE}/cover.jpg")).await;

    let service = service(&fx);
    let partial = service
        .resolve(&art_request(
            format!("/art/{entry_key}/cover"),
            Some(ByteRange::FromTo {
                start: 10,
                end: Some(19),
            }),
            None,
        ))
        .await;
    assert_eq!(partial.header.status, 206);
    assert_eq!(body_bytes(&service, partial).await, payload[10..20]);
}

#[tokio::test]
async fn matching_etag_is_304_and_does_not_depend_on_path_spelling() {
    let fx = fixture("etag").await;
    write_file(
        &fx.media_root,
        &format!("Music/{NFD_CAFE}/cover.jpg"),
        b"cover",
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa09";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_CAFE}/cover.jpg")).await;

    let service = service(&fx);
    let first = service
        .resolve(&art_request(format!("/art/{entry_key}/cover"), None, None))
        .await;
    assert_eq!(first.header.status, 200);
    let etag = first.header.etag.clone().expect("cover responses carry an etag");

    let cached = service
        .resolve(&art_request(
            format!("/art/{entry_key}/cover"),
            None,
            Some(etag),
        ))
        .await;
    assert_eq!(cached.header.status, 304);
}

#[tokio::test]
async fn cafe_without_accent_is_not_substituted_for_cover_cafe_with_accent() {
    let fx = fixture("lookalike").await;
    write_file(
        &fx.media_root,
        "Music/Cafe/cover.jpg",
        b"wrong-cover-without-accent",
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0a";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_CAFE}/cover.jpg")).await;

    let service = service(&fx);
    let resolved = service
        .resolve(&art_request(format!("/art/{entry_key}/cover"), None, None))
        .await;
    assert_eq!(
        resolved.header.status, 404,
        "NFC matching is not fuzzy filename matching: Cafe/cover.jpg is not Café/cover.jpg"
    );
    assert_still_available(&fx.library, entry_key).await;
}

#[tokio::test]
async fn deleted_cover_still_404s_and_leaves_the_track_available() {
    let fx = fixture("deleted").await;
    let path = write_file(&fx.media_root, "Music/Artist/Album/cover.jpg", &[9u8; 64]);
    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0b";
    fx.library
        .upsert(&track_entry(entry_key, "Music/Artist/Album/track.m4a"))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, "Music/Artist/Album/cover.jpg").await;
    std::fs::remove_file(&path).unwrap();

    let service = service(&fx);
    let art = service
        .resolve(&art_request(format!("/art/{entry_key}/cover"), None, None))
        .await;
    assert_eq!(art.header.status, 404);
    assert_still_available(&fx.library, entry_key).await;
}

#[tokio::test]
async fn unknown_kind_unknown_key_and_missing_row_are_404() {
    let fx = fixture("keys").await;
    write_file(&fx.media_root, "Music/Artist/Album/cover.jpg", b"cover");
    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0c";
    fx.library
        .upsert(&track_entry(entry_key, "Music/Artist/Album/track.m4a"))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, "Music/Artist/Album/cover.jpg").await;

    let service = service(&fx);
    assert_eq!(
        service
            .resolve(&art_request(format!("/art/{entry_key}/not-a-kind"), None, None))
            .await
            .header
            .status,
        404
    );
    assert_eq!(
        service
            .resolve(&art_request(
                "/art/bbbbbbbbbbbbbbbbbbbbbbbb/cover".into(),
                None,
                None,
            ))
            .await
            .header
            .status,
        404
    );
    assert_eq!(
        service
            .resolve(&art_request("/art/not-a-valid-entry-key/cover".into(), None, None))
            .await
            .header
            .status,
        404
    );

    let no_art_key = "aaaaaaaaaaaaaaaaaaaaaa0d";
    fx.library
        .upsert(&track_entry(no_art_key, "Music/Artist/Album/other.m4a"))
        .await
        .unwrap();
    assert_eq!(
        service
            .resolve(&art_request(format!("/art/{no_art_key}/cover"), None, None))
            .await
            .header
            .status,
        404,
        "a track with no cover row is 404, not a unicode fallback into a sibling album"
    );
}

#[tokio::test]
async fn empty_cover_file_is_still_served() {
    let fx = fixture("empty").await;
    write_file(&fx.media_root, &format!("Music/{NFD_CAFE}/cover.jpg"), b"");
    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0e";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_CAFE}/cover.jpg")).await;

    let service = service(&fx);
    assert_art_bytes(
        &service,
        format!("/art/{entry_key}/cover"),
        b"",
        "an empty but present cover file is a 200, not a missing-file 404",
    )
    .await;
}

#[tokio::test]
async fn multi_root_labeled_cover_path_survives_unicode_mismatch() {
    let fx = fixture("multi-root").await;
    let nas = fx.base.join("nas");
    std::fs::create_dir_all(&nas).unwrap();
    let payload = b"nas-cover".repeat(12);
    write_file(&nas, &format!("Music/{NFD_CAFE}/cover.jpg"), &payload);

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa0f";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("nas/Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(
        &fx,
        entry_key,
        &format!("nas/Music/{NFC_CAFE}/cover.jpg"),
    )
    .await;

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
    assert_art_bytes(
        &service,
        format!("/art/{entry_key}/cover"),
        &payload,
        "multi-root {label}/ prefix plus NFC/NFD mismatch must still serve the cover",
    )
    .await;
}

#[tokio::test]
async fn disk_cache_fill_reads_the_unicode_resolved_cover() {
    let fx = fixture("disk-cache").await;
    let payload = b"cached-cover-bytes".repeat(16);
    write_file(
        &fx.media_root,
        &format!("Music/{NFD_CAFE}/cover.jpg"),
        &payload,
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa10";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_CAFE}/cover.jpg")).await;

    let cache_root = fx.base.join("artwork-cache");
    let service = Arc::new(MediaService::with_roots_and_artwork_cache(
        Arc::clone(&fx.library),
        SharedRootResolver::new(RootResolver::single(fx.media_root.clone())),
        transcode_config(&fx.base),
        cache_root.clone(),
    ));
    service.set_artwork_disk_cache_enabled(true);

    let first = service
        .resolve_for_client(
            &art_request(format!("/art/{entry_key}/cover"), None, None),
            true,
            "Living Room TV",
        )
        .await;
    assert_eq!(
        first.header.status, 200,
        "artwork disk-cache fill must read the cover through the existing-file resolver"
    );
    let Body::File {
        path: first_path, ..
    } = &first.body
    else {
        panic!("cached artwork should be file-backed");
    };
    assert!(first_path.starts_with(&cache_root));
    assert_eq!(std::fs::read(first_path).unwrap(), payload);

    // Source can disappear after a successful fill; the cache still answers.
    let _ = std::fs::remove_dir_all(fx.media_root.join("Music"));
    let hit = service
        .resolve_for_client(
            &art_request(format!("/art/{entry_key}/cover"), None, None),
            true,
            "Bedroom TV",
        )
        .await;
    assert_eq!(hit.header.status, 200);
    assert_eq!(body_bytes(&service, hit).await, payload);
}

#[tokio::test]
async fn movie_poster_through_the_same_art_handler_survives_unicode_mismatch() {
    let fx = fixture("poster").await;
    let payload = b"poster-bytes".repeat(8);
    write_file(
        &fx.media_root,
        &format!("Movies/{NFD_CAFE}/poster.jpg"),
        &payload,
    );

    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa11";
    fx.library
        .upsert(&movie_entry(
            entry_key,
            &format!("Movies/{NFC_CAFE}/movie.mkv"),
        ))
        .await
        .unwrap();
    fx.library
        .set_artwork(
            entry_key,
            ArtworkKind::Poster,
            &format!("Movies/{NFC_CAFE}/poster.jpg"),
        )
        .await
        .unwrap();

    let service = service(&fx);
    assert_art_bytes(
        &service,
        format!("/art/{entry_key}/poster"),
        &payload,
        "art() serves every kind; a poster NFC/NFD mismatch must not 404 either",
    )
    .await;
}

#[tokio::test]
async fn unsatisfiable_cover_range_is_416_not_a_missing_file_404() {
    let fx = fixture("range-416").await;
    write_file(
        &fx.media_root,
        &format!("Music/{NFD_CAFE}/cover.jpg"),
        &[1u8; 50],
    );
    let entry_key = "aaaaaaaaaaaaaaaaaaaaaa12";
    fx.library
        .upsert(&track_entry(
            entry_key,
            &format!("Music/{NFC_CAFE}/track.m4a"),
        ))
        .await
        .unwrap();
    upsert_cover(&fx, entry_key, &format!("Music/{NFC_CAFE}/cover.jpg")).await;

    let service = service(&fx);
    let resolved = service
        .resolve(&art_request(
            format!("/art/{entry_key}/cover"),
            Some(ByteRange::FromTo {
                start: 100,
                end: Some(200),
            }),
            None,
        ))
        .await;
    assert_eq!(
        resolved.header.status, 416,
        "the cover was found; an unsatisfiable Range is 416, not a unicode 404"
    );
    assert_still_available(&fx.library, entry_key).await;
}
