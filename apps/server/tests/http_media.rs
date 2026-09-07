//! End-to-end proof that the plain-HTTP(S) pairing + media-playback surface
//! (`http_media.rs`) actually works over a real TCP connection, not just at
//! the unit-test level — Roku-groundwork prerequisite, see the pinned
//! implementation-plan comment on GitHub issue #54. Drives the exact same
//! sequence a real HTTP-only device would: pair over HTTP, get approved via
//! the same trusted in-process call the desktop Tauri command uses, poll
//! for the bearer token, then negotiate and fetch a byte range of real
//! media — asserting on wire-level details (status codes, `Content-Range`)
//! that only a real `reqwest` round trip can actually prove.

use serde_json::{json, Value};
use std::io::Read;
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{CatalogManifest, CatalogThumbprint, MediaKind, PlaybackMode, PlaybackPlan, VideoStreamInfo};
use swarm_media::roots::MediaRoot;
use swarm_media::store::{ArtworkKind, EntryRecord};
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};

/// Runs the same begin (device, HTTP) -> approve (owner, in-process, the
/// same trust boundary as the real Tauri command) -> poll (device, HTTP)
/// sequence every test in this file needs before it can call an
/// authenticated route, and returns the bearer token.
async fn pair_and_get_token(client: &reqwest::Client, base_url: &str, core: &ServerCore, name: &str) -> String {
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
    assert_eq!(code.len(), 8);

    let (approved_name, _token_returned_to_owner) =
        core.approve_http_media_pairing(code).await.unwrap();
    assert_eq!(approved_name, name);

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
    let token = poll["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    token
}

fn deterministic_bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| seed.wrapping_add((i * 31 + i / 251) as u8))
        .collect()
}

fn test_config(media_root: &std::path::Path, data_dir: std::path::PathBuf) -> ServerConfig {
    ServerConfig {
        media_roots: vec![MediaRoot {
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

/// A direct-play-eligible (h264/aac) movie entry, matching the codec fields
/// crates/swarm-media/tests/playback.rs already established as the
/// convention for tests that are about the serving layer, not real ffprobe
/// scanning — only `entry_key`/`relative_path`/`size` vary per call site.
fn direct_play_entry(entry_key: &str, relative_path: &str, size: u64) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size,
        modified_time: 0,
        fingerprint: "fingerprint".into(),
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
    }
}

#[tokio::test]
async fn pair_negotiate_and_range_fetch_media_over_real_http() {
    let base = std::env::temp_dir().join(format!("swarm-http-media-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let config = ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".into(),
            path: media_root.clone(),
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir: base.join("server-data"),
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    };
    let core = ServerCore::start(config).await.unwrap();
    // Settles the initial (here, empty-root) background scan before this
    // test's own direct upsert below, so the two can never race — same
    // reasoning `live_media_roots.rs` uses `wait_for_scan` for.
    core.wait_for_scan().await.unwrap();

    // Hardcoded codec fields, matching crates/swarm-media/tests/playback.rs's
    // established convention: this test is about the HTTP surface, not real
    // ffprobe scanning, so a direct upsert (not a real scan of a real
    // container file) is the deliberate, established way to avoid depending
    // on ffmpeg/ffprobe being installed.
    let relative_path = "movies/example.mp4";
    let media_bytes = deterministic_bytes(1_000_000, 9);
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, &media_bytes).unwrap();
    let entry = EntryRecord {
        entry_key: "0123456789abcdef01234567".into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size: media_bytes.len() as u64,
        modified_time: 0,
        fingerprint: "fingerprint".into(),
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
    };
    core.library.upsert(&entry).await.unwrap();

    let base_url = format!("http://{}", core.http_media_addr);
    let client = reqwest::Client::new();

    // A browser page's fetch() carries Sec-Fetch-* metadata that a real
    // device client never sends — reject_cross_site must block it even
    // though the request's source IP genuinely is on the LAN (this test
    // runs from 127.0.0.1).
    let cross_site_attempt = client
        .post(format!("{base_url}/pair/begin"))
        .header("sec-fetch-site", "cross-site")
        .json(&json!({ "name": "Malicious Page" }))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_site_attempt.status(), 403);

    let token = pair_and_get_token(&client, &base_url, &core, "Living Room Roku").await;

    // --- Auth is load-bearing: no token, no access. ---
    let unauthenticated = client
        .get(format!("{base_url}/media/{}", entry.entry_key))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), 401);

    // --- Negotiate playback with the token. ---
    let plan: PlaybackPlan = client
        .post(format!("{base_url}/play/{}", entry.entry_key))
        .bearer_auth(&token)
        .json(&json!({
            "capabilities": CapabilityProfile::fire_tv_baseline(),
            "start_position_secs": 0,
            "prefer_direct": true,
            "preview": false,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan.mode, PlaybackMode::Direct);
    let media_path_from_plan = plan.path.clone();

    // --- Fetch a real byte range through the negotiated path, with the
    // same token. ---
    let ranged = client
        .get(format!("{base_url}{media_path_from_plan}"))
        .bearer_auth(&token)
        .header("range", "bytes=500000-500099")
        .send()
        .await
        .unwrap();
    assert_eq!(ranged.status(), 206);
    assert_eq!(
        ranged.headers().get("content-range").unwrap(),
        "bytes 500000-500099/1000000"
    );
    let body = ranged.bytes().await.unwrap();
    assert_eq!(body.len(), 100);
    assert_eq!(&body[..], &media_bytes[500_000..500_100]);

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}

/// Proves a paired HTTP-only device can actually *browse* the library, not
/// just play an already-known entry_key — catalog/artwork share the exact
/// same `media_get` handler as the byte-serving routes (both are just
/// opaque paths to `MediaService::resolve_for_network`), so this also
/// exercises the two things that handler needs beyond plain byte-range
/// serving: forwarding the full path+query (artwork width requests ride in
/// the query string) and the If-None-Match/ETag round trip artwork caching
/// depends on.
#[tokio::test]
async fn browse_catalog_and_fetch_artwork_over_real_http() {
    let base = std::env::temp_dir().join(format!("swarm-http-catalog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let config = ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".into(),
            path: media_root.clone(),
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir: base.join("server-data"),
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    };
    let core = ServerCore::start(config).await.unwrap();
    core.wait_for_scan().await.unwrap();

    let relative_path = "movies/example.mp4";
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, deterministic_bytes(10_000, 3)).unwrap();
    let entry = EntryRecord {
        entry_key: "fedcba9876543210fedcba9".into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size: 10_000,
        modified_time: 0,
        fingerprint: "fingerprint".into(),
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
    };
    core.library.upsert(&entry).await.unwrap();

    let poster_relative_path = "movies/poster.jpg";
    let poster_bytes = deterministic_bytes(2_000, 7);
    std::fs::write(media_root.join(poster_relative_path), &poster_bytes).unwrap();
    core.library
        .set_artwork(&entry.entry_key, ArtworkKind::Poster, poster_relative_path)
        .await
        .unwrap();

    let base_url = format!("http://{}", core.http_media_addr);
    let client = reqwest::Client::new();
    let token = pair_and_get_token(&client, &base_url, &core, "Living Room Roku").await;
    core.set_artwork_disk_cache_enabled(true);

    // --- /catalog/thumbprint ---
    let thumbprint: CatalogThumbprint = client
        .get(format!("{base_url}/catalog/thumbprint"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(thumbprint.entry_count, 1);

    // --- /catalog/manifest (plain) ---
    let manifest: CatalogManifest = client
        .get(format!("{base_url}/catalog/manifest"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(manifest.entries.len(), 1);
    assert_eq!(manifest.entries[0].entry_key, entry.entry_key);
    assert_eq!(manifest.thumbprint, thumbprint.thumbprint);

    // --- /catalog/manifest.gz — same content, compressed ---
    let gz_response = client
        .get(format!("{base_url}/catalog/manifest.gz"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(gz_response.status(), 200);
    let gz_bytes = gz_response.bytes().await.unwrap();
    let mut decompressed = String::new();
    flate2::read::GzDecoder::new(&gz_bytes[..])
        .read_to_string(&mut decompressed)
        .unwrap();
    let gz_manifest: CatalogManifest = serde_json::from_str(&decompressed).unwrap();
    assert_eq!(gz_manifest.entries[0].entry_key, entry.entry_key);

    // --- /art/{entry_key}/poster — real bytes, plus the ETag/304 round trip
    // artwork caching depends on. ---
    let art_response = client
        .get(format!("{base_url}/art/{}/poster", entry.entry_key))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(art_response.status(), 200);
    let etag = art_response
        .headers()
        .get("etag")
        .expect("art() always sets an ETag")
        .to_str()
        .unwrap()
        .to_string();
    let art_body = art_response.bytes().await.unwrap();
    assert_eq!(&art_body[..], &poster_bytes[..]);

    let cache_hit = client
        .get(format!("{base_url}/art/{}/poster", entry.entry_key))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(cache_hit.status(), 200);
    assert_eq!(&cache_hit.bytes().await.unwrap()[..], &poster_bytes[..]);

    let activity = core.artwork_cache_snapshot().await;
    assert_eq!(activity.events.len(), 2);
    assert!(activity
        .events
        .iter()
        .all(|event| event.client == "Living Room Roku"));
    assert_eq!(
        activity.events[0].kind,
        swarm_media::artwork_cache::ArtworkCacheEventKind::Cached
    );
    assert_eq!(
        activity.events[1].kind,
        swarm_media::artwork_cache::ArtworkCacheEventKind::ServedFromCache
    );

    let cached = client
        .get(format!("{base_url}/art/{}/poster", entry.entry_key))
        .bearer_auth(&token)
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(
        cached.status(),
        304,
        "a matching If-None-Match must short-circuit to 304, proving the ETag round trip actually works over HTTP"
    );

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}

/// `/stop` is not a nice-to-have: without it, an HTTP client that stops
/// playback early (user presses back, or just closes the app) has no way to
/// release its session's upload-bandwidth reservation — the exact bug
/// crates/swarm-media/tests/playback.rs's
/// `stop_releases_the_reservation_so_a_retry_no_longer_needs_the_idle_timeout`
/// documents being found live and fixed for QUIC clients. That test forces
/// a visible 429 by negotiating with `is_lan: false` directly against
/// `MediaService`; a real HTTP round trip can't do that — a loopback
/// `reqwest` connection genuinely *is* LAN (`is_lan_ip(127.0.0.1)` is
/// `true`), and LAN playback deliberately bypasses budget admission control
/// entirely, by design (see `settings.rs`'s "LAN playback always bypasses
/// it" doc comment) — so this proves the same release via
/// `TranscodeManager::reserved_bps()` instead, which still tracks every
/// session's reservation regardless of whether admission control enforced
/// anything for it.
#[tokio::test]
async fn stop_releases_the_reservation_over_http() {
    let base = std::env::temp_dir().join(format!("swarm-http-stop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let core = ServerCore::start(test_config(&media_root, base.join("server-data")))
        .await
        .unwrap();
    core.wait_for_scan().await.unwrap();

    let relative_path = "movies/example.mp4";
    let media_bytes = deterministic_bytes(1_000_000, 5);
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, &media_bytes).unwrap();
    let entry = direct_play_entry("aaaa11112222333344445555", relative_path, media_bytes.len() as u64);
    core.library.upsert(&entry).await.unwrap();

    let base_url = format!("http://{}", core.http_media_addr);
    let client = reqwest::Client::new();
    let token = pair_and_get_token(&client, &base_url, &core, "Living Room Roku").await;

    let plan: PlaybackPlan = client
        .post(format!("{base_url}/play/{}", entry.entry_key))
        .bearer_auth(&token)
        .json(&json!({
            "capabilities": CapabilityProfile::fire_tv_baseline(),
            "start_position_secs": 0,
            "prefer_direct": true,
            "preview": false,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan.mode, PlaybackMode::Direct);
    assert!(
        core.media_service().transcode_manager().reserved_bps() > 0,
        "negotiating a direct-play session must reserve real bandwidth"
    );

    let stop = client
        .post(format!("{base_url}/stop/{}", plan.session_id))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(stop.status(), 200);
    assert_eq!(
        core.media_service().transcode_manager().reserved_bps(),
        0,
        "/stop must release the whole reservation immediately, not just mark it idle"
    );

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}

/// A device can't caption anything without this route: `PlaybackPlan.subtitles`
/// points at server-generated paths (WebVTT from local transcription, in
/// this case), not directly-fetchable file paths — without `/subtitles/*`
/// mirrored to HTTP, a paired device has no way to ever resolve them.
#[tokio::test]
async fn subtitles_are_served_over_http() {
    let base = std::env::temp_dir().join(format!("swarm-http-subtitles-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let core = ServerCore::start(test_config(&media_root, base.join("server-data")))
        .await
        .unwrap();
    core.wait_for_scan().await.unwrap();

    let relative_path = "movies/example.mp4";
    let media_bytes = deterministic_bytes(1_000_000, 11);
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, &media_bytes).unwrap();
    let entry = direct_play_entry("bbbb11112222333344445555", relative_path, media_bytes.len() as u64);
    core.library.upsert(&entry).await.unwrap();

    let subtitle_path = base.join("subtitles").join("example.vtt");
    std::fs::create_dir_all(subtitle_path.parent().unwrap()).unwrap();
    std::fs::write(
        &subtitle_path,
        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n",
    )
    .unwrap();
    core.library
        .complete_transcription(&swarm_media::store::SubtitleRecord {
            id: "whisper-en".into(),
            entry_key: entry.entry_key.clone(),
            language: "en".into(),
            label: "English — AI generated".into(),
            source: "whisper".into(),
            format: "vtt".into(),
            file_path: subtitle_path.to_string_lossy().to_string(),
            fingerprint: entry.fingerprint.clone(),
        })
        .await
        .unwrap();

    let base_url = format!("http://{}", core.http_media_addr);
    let client = reqwest::Client::new();
    let token = pair_and_get_token(&client, &base_url, &core, "Living Room Roku").await;

    let plan: PlaybackPlan = client
        .post(format!("{base_url}/play/{}", entry.entry_key))
        .bearer_auth(&token)
        .json(&json!({
            "capabilities": CapabilityProfile::fire_tv_baseline(),
            "start_position_secs": 0,
            "prefer_direct": true,
            "preview": false,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan.subtitles.len(), 1);

    let subtitle_response = client
        .get(format!("{base_url}{}", plan.subtitles[0].path))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(subtitle_response.status(), 200);
    assert_eq!(
        subtitle_response.headers().get("content-type").unwrap(),
        "text/vtt; charset=utf-8"
    );
    let body = subtitle_response.text().await.unwrap();
    assert!(body.contains("Hello"));

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}

/// The `/hls/{session_id}/{*rest}` route uses an axum catch-all
/// specifically because `swarm-media`'s `safe_hls_path` permits any number
/// of `/`-separated segments (a fixed `{rendition}/{file}` two-segment
/// pattern would 404 a real request, since a real HLS session's *master*
/// playlist sits at a single-segment path and its rendition playlists sit
/// one level deeper). Real ffmpeg-generated media, real transcoding, real
/// nested-path fetch — the only way to actually prove the wildcard route
/// works, since a bogus/nonexistent session_id would 404 identically
/// whether the route matched-then-failed or never matched at all. Skips
/// gracefully if ffmpeg isn't available, matching
/// crates/swarm-media/src/transcode.rs's own
/// `ffmpeg_hls_pipeline_smoke_test_when_ffmpeg_is_available` convention.
#[tokio::test]
async fn hls_master_and_nested_rendition_playlist_serve_over_real_http() {
    let base = std::env::temp_dir().join(format!("swarm-http-hls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let relative_path = "movies/source.mp4";
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    let generated = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-f", "lavfi", "-i", "testsrc=duration=2:size=1280x720:rate=30",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", "-y",
        ])
        .arg(&media_path)
        .status()
        .await;
    match generated {
        // ffmpeg absent entirely (a bare CI runner) — skip, same as a
        // fixture-generation failure. The doc comment above promises this.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping: ffmpeg is not installed");
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        Err(error) => panic!("could not launch ffmpeg: {error}"),
        Ok(status) if !status.success() => {
            eprintln!("skipping: ffmpeg could not generate the test fixture");
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        Ok(_) => {}
    }

    let config = test_config(&media_root, base.join("server-data"));
    let core = ServerCore::start(config).await.unwrap();
    // Unlike every other test in this file, the real media file already
    // exists on disk *before* ServerCore::start (ffmpeg wrote it above, so
    // this test's own negotiation has something real to transcode) — the
    // initial background scan discovers and upserts it under its own
    // real-ffprobe-derived entry_key before this line returns. A second,
    // synthetic upsert at the same relative_path would collide with that
    // row (relative_path is UNIQUE) — use the real scanned entry instead of
    // constructing one, which is also more honest for an HLS test: the
    // video/audio codec info driving transcode eligibility comes from real
    // ffprobe output, not hand-picked fields.
    let scan_report = core.wait_for_scan().await.unwrap();
    assert_eq!(scan_report.added, 1);
    let entry = core.library.list().await.unwrap().into_iter().next().unwrap();

    let base_url = format!("http://{}", core.http_media_addr);
    let client = reqwest::Client::new();
    let token = pair_and_get_token(&client, &base_url, &core, "Living Room Roku").await;

    let plan: PlaybackPlan = client
        .post(format!("{base_url}/play/{}", entry.entry_key))
        .bearer_auth(&token)
        .json(&json!({
            "capabilities": CapabilityProfile::fire_tv_baseline(),
            "start_position_secs": 0,
            "prefer_direct": false,
            "preview": false,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan.mode, PlaybackMode::Hls);

    let master = client
        .get(format!("{base_url}{}", plan.path))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(master.status(), 200);
    let master_body = master.text().await.unwrap();
    assert!(master_body.contains("#EXTM3U"));

    // The master playlist references each rendition's own playlist at a
    // *nested* relative path (e.g. "0/index.m3u8") — fetching one proves
    // the catch-all route actually forwards a multi-segment path, not just
    // the single-segment master playlist request above.
    let rendition_relative_path = master_body
        .lines()
        .find(|line| !line.starts_with('#') && line.ends_with("index.m3u8"))
        .expect("a real HLS master playlist references at least one rendition playlist")
        .to_string();
    assert!(
        rendition_relative_path.contains('/'),
        "expected a nested rendition path, got {rendition_relative_path:?}"
    );

    let rendition = client
        .get(format!(
            "{base_url}/hls/{}/{rendition_relative_path}",
            plan.session_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        rendition.status(),
        200,
        "the catch-all /hls route must forward a nested rendition-playlist path, not 404 it"
    );

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}

/// The two lowest-priority routes from the QUIC dispatch (client-error
/// triage and like/unlike) — no playback/browsing depends on either, but a
/// paired device still needs them mirrored for real feature parity.
#[tokio::test]
async fn error_reporting_and_likes_work_over_real_http() {
    let base = std::env::temp_dir().join(format!("swarm-http-errors-likes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let core = ServerCore::start(test_config(&media_root, base.join("server-data")))
        .await
        .unwrap();
    core.wait_for_scan().await.unwrap();

    let relative_path = "movies/example.mp4";
    let media_bytes = deterministic_bytes(1_000, 13);
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, &media_bytes).unwrap();
    let entry = direct_play_entry("dddd11112222333344445555", relative_path, media_bytes.len() as u64);
    core.library.upsert(&entry).await.unwrap();

    let base_url = format!("http://{}", core.http_media_addr);
    let client = reqwest::Client::new();
    let token = pair_and_get_token(&client, &base_url, &core, "Living Room Roku").await;

    let report_response = client
        .post(format!("{base_url}/errors/report"))
        .bearer_auth(&token)
        .json(&json!({
            "device_id": "roku-test-device",
            "device_name": "Living Room Roku",
            "entry_key": entry.entry_key,
            "message": "playback failed: decoder error",
            "occurred_at_ms": 1_700_000_000_000i64,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(report_response.status(), 204);
    let errors = core.library.list_client_errors().await.unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].message, "playback failed: decoder error");

    let like_response = client
        .post(format!("{base_url}/likes/toggle"))
        .bearer_auth(&token)
        .json(&json!({
            "device_id": "roku-test-device",
            "device_name": "Living Room Roku",
            "entry_key": entry.entry_key,
            "liked": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(like_response.status(), 204);
    let counts = core.library.like_counts().await.unwrap();
    assert_eq!(counts.get(entry.entry_key.as_str()), Some(&1));

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}

/// Proves the TLS listener end-to-end: a device pairs over the plain
/// listener (as always — `/pair/*` is `is_lan_ip`-gated and has no reason to
/// require TLS), learns the server's HTTP CA from `/pair/poll`'s `Approved`
/// response, and can then reach every authenticated route over the TLS
/// listener using *only* that CA as its trust root — exactly what a Roku
/// `HTTPCertificatesFile` would do. Also proves a client that does *not*
/// trust the CA is refused at the TLS handshake itself (never reaches the
/// app), and that the plain listener keeps working unaffected.
#[tokio::test]
async fn pair_over_http_learns_ca_then_media_is_reachable_over_pinned_tls() {
    let base = std::env::temp_dir().join(format!("swarm-http-media-tls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let media_root = base.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let mut config = test_config(&media_root, base.join("server-data"));
    config.http_media_tls_bind = Some("127.0.0.1:0".parse().unwrap());
    let core = ServerCore::start(config).await.unwrap();
    core.wait_for_scan().await.unwrap();

    let relative_path = "movies/example.mp4";
    let media_bytes = deterministic_bytes(500_000, 3);
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, &media_bytes).unwrap();
    let entry = direct_play_entry(
        "abc123abc123abc123abc123",
        relative_path,
        media_bytes.len() as u64,
    );
    core.library.upsert(&entry).await.unwrap();

    let tls_addr = core
        .http_media_tls_addr
        .expect("TLS listener should have started when http_media_tls_bind is Some");
    let http_base_url = format!("http://{}", core.http_media_addr);
    let https_base_url = format!("https://127.0.0.1:{}", tls_addr.port());

    let plain_client = reqwest::Client::new();
    let token = pair_and_get_token(&plain_client, &http_base_url, &core, "Living Room Roku").await;

    // pair_and_get_token only asserts on `token`; re-poll independently to
    // inspect `http_ca_pem`, the field this test actually cares about.
    // (A second /pair/poll after approval is a legitimate no-op per the
    // pairing state machine — the token is re-returned, not reissued.)
    let begin: Value = plain_client
        .post(format!("{http_base_url}/pair/begin"))
        .json(&json!({ "name": "Living Room Roku (second device)" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    core.approve_http_media_pairing(begin["code"].as_str().unwrap())
        .await
        .unwrap();
    let poll: Value = plain_client
        .post(format!("{http_base_url}/pair/poll"))
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
    let ca_pem = poll["http_ca_pem"]
        .as_str()
        .expect("Approved response must carry the HTTP CA PEM when the TLS listener is running")
        .to_string();
    assert!(ca_pem.contains("BEGIN CERTIFICATE"));
    assert!(core.http_ca_fingerprint().is_some());

    // --- A client trusting only this CA reaches authenticated routes over TLS. ---
    let pinned_client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(ca_pem.as_bytes()).unwrap())
        .build()
        .unwrap();

    let plan: PlaybackPlan = pinned_client
        .post(format!("{https_base_url}/play/{}", entry.entry_key))
        .bearer_auth(&token)
        .json(&json!({
            "capabilities": CapabilityProfile::fire_tv_baseline(),
            "start_position_secs": 0,
            "prefer_direct": true,
            "preview": false,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan.mode, PlaybackMode::Direct);

    let range_response = pinned_client
        .get(format!("{https_base_url}{}", plan.path))
        .bearer_auth(&token)
        .header("range", "bytes=0-99")
        .send()
        .await
        .unwrap();
    assert_eq!(range_response.status(), 206);
    let body = range_response.bytes().await.unwrap();
    assert_eq!(body.len(), 100);
    assert_eq!(&body[..], &media_bytes[0..100]);

    // --- A client that does NOT trust this CA fails at the handshake, not merely with an HTTP error. ---
    let untrusting_client = reqwest::Client::builder().build().unwrap();
    let untrusted_attempt = untrusting_client
        .get(format!("{https_base_url}/media/{}", entry.entry_key))
        .bearer_auth(&token)
        .send()
        .await;
    assert!(
        untrusted_attempt.is_err(),
        "a client that doesn't trust the server's HTTP CA must fail the TLS handshake outright"
    );

    // --- The plain listener keeps working unaffected. ---
    let plain_ok = plain_client
        .get(format!("{http_base_url}/catalog/thumbprint"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(plain_ok.status(), 200);

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}
