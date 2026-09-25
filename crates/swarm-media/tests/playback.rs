use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use swarm_core::capability::CapabilityProfile;
use swarm_core::peer::{
    AudioStreamInfo, ByteRange, MediaKind, PeerRequest, PlaybackMode, PlaybackPlan,
    PlaybackPreferences, VideoStreamInfo,
};
use swarm_media::serve::{stream_body, Body, MediaService};
use swarm_media::store::{ArtworkKind, EntryRecord, Library, SubtitleRecord};
use swarm_media::transcode::TranscodeConfig;

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

#[tokio::test]
async fn music_playback_preparation_handles_unicode_normalized_smb_paths() {
    let root = std::env::temp_dir().join(format!("swarm-playback-unicode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let media_root = root.join("media");
    let actual_relative_path = "music/Cafe\u{301}/track.m4a";
    let catalog_relative_path = "music/Café/track.m4a";
    let media_path = media_root.join(actual_relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, vec![7u8; 10_000]).unwrap();
    let artwork_bytes = b"cover artwork";
    std::fs::write(
        media_root.join("music/Cafe\u{301}/cover.jpg"),
        artwork_bytes,
    )
    .unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let entry = EntryRecord {
        entry_key: "0123456789abcdef01234567".into(),
        relative_path: catalog_relative_path.into(),
        kind: MediaKind::Track,
        title: "Track".into(),
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
    };
    library.upsert(&entry).await.unwrap();
    library
        .set_artwork(&entry.entry_key, ArtworkKind::Cover, "music/Café/cover.jpg")
        .await
        .unwrap();
    let service = MediaService::with_transcoding(
        Arc::clone(&library),
        media_root,
        TranscodeConfig {
            enabled: false,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        },
    );
    let negotiation = PeerRequest {
        path: format!("/play/{}", entry.entry_key),
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
    };

    let resolved = service.resolve(&negotiation).await;
    assert_eq!(
        resolved.header.status, 200,
        "a music catalog path whose Unicode spelling differs from the SMB path must still negotiate playback"
    );

    let artwork = service
        .resolve(&request(format!("/art/{}/cover", entry.entry_key)))
        .await;
    assert_eq!(
        artwork.header.status, 200,
        "a music artwork path whose Unicode spelling differs from the SMB path must still be served"
    );
    let Body::File { path, .. } = artwork.body else {
        panic!("artwork must resolve to a file")
    };
    assert_eq!(std::fs::read(path).unwrap(), artwork_bytes);

    drop(service);
    let _ = std::fs::remove_dir_all(root);
}

/// #355 follow-up: a Unicode-normalization fix for `RootResolver` alone did
/// not stop reports of "server could not prepare playback (404)" after it
/// shipped. `scan::retry_transient_not_found`'s doc comment already
/// documents the same root cause for the background walk — a busy `smbfs`
/// mount returns a transient `NotFound` from a stat/`read_dir` call for a
/// file that is genuinely still there — but nothing gave the request-time
/// `/play` and `/media` path resolution an equivalent retry, so that exact
/// flake surfaced as a real, user-visible negotiation 404 instead. This
/// proves negotiation recovers when the file only becomes visible moments
/// after the first lookup, standing in for a stat that transiently errors
/// on a file that was there the whole time.
#[tokio::test]
async fn transient_missing_file_still_negotiates_playback_once_it_appears() {
    let root = std::env::temp_dir().join(format!("swarm-playback-transient-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let media_root = root.join("media");
    let relative_path = "music/Artist/Album/track.m4a";
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let entry = EntryRecord {
        entry_key: "0123456789abcdef01234567".into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Track,
        title: "Track".into(),
        size: 4_096,
        modified_time: 0,
        fingerprint: "fingerprint".into(),
        artist: None,
        album: None,
        track_number: None,
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
    };
    library.upsert(&entry).await.unwrap();

    let service = MediaService::with_transcoding(
        Arc::clone(&library),
        media_root,
        TranscodeConfig {
            enabled: false,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        },
    );
    let negotiation = PeerRequest {
        path: format!("/play/{}", entry.entry_key),
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
    };

    // The file does not exist at negotiation start and only appears ~70ms
    // in — inside the retry budget, but well past a single immediate stat.
    let payload = vec![9u8; 4_096];
    tokio::spawn({
        let media_path = media_path.clone();
        let payload = payload.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(70)).await;
            std::fs::write(&media_path, &payload).unwrap();
        }
    });

    let resolved = service.resolve(&negotiation).await;
    assert_eq!(
        resolved.header.status, 200,
        "a file that appears within the retry budget must still negotiate playback, not 404 (#355)"
    );
    assert!(
        library.get(&entry.entry_key).await.unwrap().is_some(),
        "a transient miss recovered by retry must not mark the row missing"
    );

    drop(service);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn playback_negotiation_returns_a_budgeted_direct_session_with_range_support() {
    let root = std::env::temp_dir().join(format!("swarm-playback-route-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let media_root = root.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let relative_path = "movies/example.mp4";
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, vec![7u8; 1_000_000]).unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let entry = EntryRecord {
        entry_key: "0123456789abcdef01234567".into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size: 1_000_000,
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
    };
    library.upsert(&entry).await.unwrap();
    let subtitle_path = root.join("subtitles").join("example.vtt");
    std::fs::create_dir_all(subtitle_path.parent().unwrap()).unwrap();
    std::fs::write(
        &subtitle_path,
        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n",
    )
    .unwrap();
    library
        .complete_transcription(&SubtitleRecord {
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

    let service = MediaService::with_transcoding(
        library,
        media_root,
        TranscodeConfig {
            enabled: false,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        },
    );
    let negotiation = PeerRequest {
        path: format!("/play/{}", entry.entry_key),
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
    };
    let resolved = service
        .resolve_for_peer(&negotiation, false, "Living room TV", "tv-fingerprint")
        .await;
    assert_eq!(resolved.header.status, 200);
    let Body::Bytes(body) = resolved.body else {
        panic!("playback plan must be JSON")
    };
    let abandoned_plan: PlaybackPlan = serde_json::from_slice(&body).unwrap();
    assert_eq!(abandoned_plan.mode, PlaybackMode::Direct);

    // The TV never opened the first plan, as happens when its `/play`
    // response stream is lost. A retry from the same authenticated peer must
    // replace that unclaimed reservation instead of returning 429 forever.
    let retry = service
        .resolve_for_peer(&negotiation, false, "Living room TV", "tv-fingerprint")
        .await;
    assert_eq!(retry.header.status, 200);
    let Body::Bytes(body) = retry.body else {
        panic!("retry playback plan must be JSON")
    };
    let plan: PlaybackPlan = serde_json::from_slice(&body).unwrap();
    assert_ne!(plan.session_id, abandoned_plan.session_id);
    assert_eq!(
        service
            .resolve(&request(abandoned_plan.path.clone()))
            .await
            .header
            .status,
        404,
        "the superseded plan must no longer consume capacity"
    );
    assert_eq!(plan.mode, PlaybackMode::Direct);
    assert_eq!(plan.max_bitrate, 1_000_000);
    assert_eq!(plan.subtitles.len(), 1);
    let subtitles = service
        .resolve(&request(plan.subtitles[0].path.clone()))
        .await;
    assert_eq!(subtitles.header.status, 200);
    assert_eq!(
        subtitles.header.content_type.as_deref(),
        Some("text/vtt; charset=utf-8")
    );
    let Body::Bytes(subtitle_body) = subtitles.body else {
        panic!("subtitle must be served as bytes")
    };
    assert!(String::from_utf8(subtitle_body).unwrap().contains("Hello"));

    let mut media_request = request(plan.path.clone());
    media_request.range = Some(ByteRange::FromTo {
        start: 500_000,
        end: Some(500_099),
    });
    let media = service.resolve(&media_request).await;
    assert_eq!((media.header.status, media.header.len), (206, 100));
    assert_eq!(service.transcode_manager().reserved_bps(), 1_000_000);

    // Once the TV has opened the plan, another negotiation must not cancel
    // active playback just because it comes from the same device.
    let active_retry = service
        .resolve_for_peer(&negotiation, false, "Living room TV", "tv-fingerprint")
        .await;
    assert_eq!(active_retry.header.status, 200);
    assert_eq!(
        service.resolve(&request(plan.path.clone())).await.header.status,
        200,
        "a retry must not supersede a plan after the TV has opened it"
    );

    // The first internet session already holds budget. A second internet
    // negotiation is therefore rejected, while the identical request from
    // the LAN bypasses admission control and succeeds.
    service.transcode_manager().set_max_upload_bps(1_000_000);
    let internet_retry = service.resolve(&negotiation).await;
    assert_ne!(internet_retry.header.status, 200);
    let lan_retry = service.resolve_for_network(&negotiation, true).await;
    assert_eq!(lan_retry.header.status, 200);

    // Raw file serving on LAN also omits both the global and per-session
    // byte pacers.
    let lan_media = service.resolve_for_network(&media_request, true).await;
    let Body::File { rate_limiters, .. } = lan_media.body else {
        panic!("direct media must resolve to a file")
    };
    assert!(rate_limiters.is_empty());

    let session_id = plan.path.split('/').nth(2).unwrap();
    service.transcode_manager().finish_use(session_id);
    drop(service);
    let _ = std::fs::remove_dir_all(root);
}

/// Confirmed live on real hardware: a video froze, the user pressed back,
/// and every subsequent play attempt (including of a different title) got
/// rejected with 429 "not enough upload bandwidth is available for the
/// lowest rendition" — the frozen session's reservation was never released
/// on back-press, only by the (much longer) idle timeout. `/stop/{id}`
/// exists so the client can release it immediately on its way out instead
/// of leaving the whole upload budget stuck for however long remains.
#[tokio::test]
async fn stop_releases_the_reservation_so_a_retry_no_longer_needs_the_idle_timeout() {
    let root = std::env::temp_dir().join(format!("swarm-playback-stop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let media_root = root.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let relative_path = "movies/example.mp4";
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, vec![7u8; 1_000_000]).unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let entry = EntryRecord {
        entry_key: "0123456789abcdef01234567".into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size: 1_000_000,
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
    };
    library.upsert(&entry).await.unwrap();

    // Budget sized to exactly fit one of this entry's 1,000,000bps direct
    // sessions and nothing more on top of it — a second concurrent
    // negotiation must fail until the first is released.
    let service = MediaService::with_transcoding(
        library,
        media_root,
        TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 1_000_000,
            reserve_percent: 0,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        },
    );
    let negotiate = || PeerRequest {
        path: format!("/play/{}", entry.entry_key),
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
    };

    let first = service.resolve(&negotiate()).await;
    assert_eq!(first.header.status, 200);
    let Body::Bytes(body) = first.body else {
        panic!("playback plan must be JSON")
    };
    let plan: PlaybackPlan = serde_json::from_slice(&body).unwrap();
    assert_eq!(plan.mode, PlaybackMode::Direct);

    // Simulates "froze, pressed back, tried to play again" without ever
    // calling /stop: the reservation is still held, so this must still fail.
    let stuck_retry = service.resolve(&negotiate()).await;
    assert_eq!(
        stuck_retry.header.status, 429,
        "budget must still be held by the first, unreleased session"
    );

    let stop = service
        .resolve(&request(format!("/stop/{}", plan.session_id)))
        .await;
    assert_eq!(stop.header.status, 200);
    assert_eq!(
        service.transcode_manager().reserved_bps(),
        0,
        "release must free the whole reservation immediately, not just mark it idle"
    );

    let retry_after_stop = service.resolve(&negotiate()).await;
    assert_eq!(
        retry_after_stop.header.status, 200,
        "retry must succeed right away now, not after waiting out idle_timeout"
    );

    drop(service);
    let _ = std::fs::remove_dir_all(root);
}

/// `stream_body` exists specifically so an HTTP transport (unlike QUIC's
/// `handle_stream`, which only ever exits by finishing or erroring) can
/// abandon a response stream mid-read — an HTTP client seeking within a
/// `<video>` element, or simply disconnecting, routinely drops the
/// connection before the body is fully sent. This proves the session that
/// stream was serving still gets released in that case, not just when the
/// stream runs to completion: an early-dropped stream that leaked its
/// session would leave `in_use` stuck above zero forever, which
/// `expire_idle` (run at the top of every `plan()` call) only reclaims once
/// `in_use == 0` — so a still-stuck reservation after `idle_timeout` has
/// elapsed is the observable symptom of exactly the bug this guards against.
#[tokio::test]
async fn dropping_a_stream_body_early_still_releases_the_session() {
    let root = std::env::temp_dir().join(format!("swarm-playback-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let media_root = root.join("media");
    std::fs::create_dir_all(&media_root).unwrap();
    let relative_path = "movies/example.mp4";
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, vec![7u8; 1_000_000]).unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let entry = EntryRecord {
        entry_key: "0123456789abcdef01234567".into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size: 1_000_000,
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
    };
    library.upsert(&entry).await.unwrap();

    // Same budget-sizing trick as the reservation-release test above: exactly
    // enough upload budget for one of this entry's 1,000,000bps sessions and
    // nothing more, plus a near-zero idle_timeout so `expire_idle` reclaims a
    // truly-released session almost immediately instead of a realistic wait.
    let service = Arc::new(MediaService::with_transcoding(
        library,
        media_root,
        TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 1_000_000,
            reserve_percent: 0,
            max_sessions: 1,
            idle_timeout: Duration::from_millis(50),
            segment_duration_secs: 4,
            ..Default::default()
        },
    ));
    let negotiate = || PeerRequest {
        path: format!("/play/{}", entry.entry_key),
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
    };

    let negotiated = service.resolve(&negotiate()).await;
    assert_eq!(negotiated.header.status, 200);
    let Body::Bytes(body) = negotiated.body else {
        panic!("playback plan must be JSON")
    };
    let plan: PlaybackPlan = serde_json::from_slice(&body).unwrap();

    // Negotiation only reserves the session; resolving the media path is
    // what actually opens it (`open_direct`, bumping `in_use`).
    let resolved = service.resolve(&request(plan.path.clone())).await;
    assert_eq!(resolved.header.status, 200);

    // Simulate an HTTP client that reads one chunk, then disconnects or
    // seeks away mid-range-request: poll exactly once, then drop the stream
    // without ever letting it reach its natural end.
    {
        let body = stream_body(resolved, &service);
        let mut body = std::pin::pin!(body);
        let first_chunk = body.next().await;
        assert!(
            first_chunk.is_some_and(|chunk| chunk.is_ok()),
            "expected a real first chunk before dropping the stream early"
        );
    } // `body`, and the session guard it owns, drops here — mid-stream.

    // A second concurrent negotiation must still fail immediately: the
    // reservation is real and hasn't idled out yet.
    let stuck_retry = service.resolve(&negotiate()).await;
    assert_eq!(
        stuck_retry.header.status, 429,
        "budget must still be held immediately after the early drop"
    );

    // Once idle_timeout elapses, `expire_idle` reclaims the session only if
    // `in_use` is back to 0. A cleanup that ran solely "after the read loop
    // finishes normally" (not on drop) would leave `in_use` stuck at 1
    // forever, and this retry would keep failing no matter how long we wait.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let retry_after_idle = service.resolve(&negotiate()).await;
    assert_eq!(
        retry_after_idle.header.status, 200,
        "an early-dropped stream must still release its session so idle reclamation can free the budget"
    );

    drop(service);
    let _ = std::fs::remove_dir_all(root);
}

/// Confirmed live on real hardware: with two lingering direct-play sessions
/// from earlier, unrelated plays (neither expired yet — they sit for up to
/// `idle_timeout` after their last byte, not released the moment playback
/// actually stops), negotiating a plain direct-play-eligible music file
/// failed with 429 "server transcode capacity is full" — max_sessions=1 was
/// being spent by sessions that never spawned an ffmpeg process at all.
/// Direct play is bandwidth-limited, not process-limited, so N of them must
/// be able to coexist under a max_sessions of 1 as long as the (separately
/// enforced, still-real) bandwidth budget allows it.
#[tokio::test]
async fn direct_play_sessions_are_not_limited_by_max_sessions() {
    let root = std::env::temp_dir().join(format!("swarm-playback-capacity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let media_root = root.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let mut entry_keys = Vec::new();
    for i in 0..2 {
        // .m4a (not .mp3): direct_compatible() only allows containers the
        // client's profile lists, and "mp3" isn't one of them (confirmed
        // live — real .mp3 files always go through HLS) — that's a real,
        // separate, correct behavior this test isn't about, so use a
        // container that direct-play actually accepts.
        let relative_path = format!("music/track{i}.m4a");
        let media_path = media_root.join(&relative_path);
        std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
        std::fs::write(&media_path, vec![7u8; 10_000]).unwrap();
        let entry_key = format!("0123456789abcdef0123456{i}");
        let entry = EntryRecord {
            entry_key: entry_key.clone(),
            relative_path,
            kind: MediaKind::Track,
            title: format!("Track {i}"),
            size: 10_000,
            modified_time: 0,
            fingerprint: format!("fingerprint{i}"),
            artist: None,
            album: None,
            track_number: None,
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
        };
        library.upsert(&entry).await.unwrap();
        entry_keys.push(entry_key);
    }

    let service = MediaService::with_transcoding(
        library,
        media_root,
        TranscodeConfig {
            enabled: true,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        },
    );

    for entry_key in &entry_keys {
        let negotiation = PeerRequest {
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
        };
        let resolved = service.resolve(&negotiation).await;
        assert_eq!(
            resolved.header.status, 200,
            "negotiation for {entry_key} must not be capacity-limited"
        );
        let Body::Bytes(body) = resolved.body else {
            panic!("playback plan must be JSON")
        };
        let plan: PlaybackPlan = serde_json::from_slice(&body).unwrap();
        assert_eq!(plan.mode, PlaybackMode::Direct);
    }

    drop(service);
    let _ = std::fs::remove_dir_all(root);
}

/// #143: a side-loaded `.srt` sidecar registered as an `external` subtitle
/// track is offered in the playback plan and served to the client as
/// WebVTT (converted on the way out), exactly like a Whisper track.
#[tokio::test]
async fn external_srt_sidecar_is_offered_and_served_as_webvtt() {
    let root = std::env::temp_dir().join(format!("swarm-playback-extsub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let media_root = root.join("media");
    let relative_path = "Movies/Heat (1995)/Heat.1995.1080p.mp4";
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, vec![9u8; 800_000]).unwrap();

    let srt_path = media_path
        .parent()
        .unwrap()
        .join("Subs")
        .join("2_English.srt");
    std::fs::create_dir_all(srt_path.parent().unwrap()).unwrap();
    std::fs::write(
        &srt_path,
        b"1\r\n00:00:01,500 --> 00:00:03,000\r\nDon't let yourself get attached\r\n",
    )
    .unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let entry = EntryRecord {
        entry_key: swarm_core::entry_key::entry_key(relative_path),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Heat".into(),
        size: 800_000,
        modified_time: 0,
        fingerprint: "heat-fp".into(),
        artist: None,
        album: None,
        track_number: None,
        show_title: None,
        season: None,
        episode: None,
        year: Some(1995),
        duration_secs: Some(10.0),
        video: Some(VideoStreamInfo {
            codec: "h264".into(),
            width: 640,
            height: 360,
            level: Some("4.1".into()),
            bitrate: Some(600_000),
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
    };
    library.upsert(&entry).await.unwrap();
    library
        .replace_external_subtitles(
            None,
            &[SubtitleRecord {
                id: format!(
                    "external-{}",
                    swarm_core::entry_key::entry_key("Movies/Heat (1995)/Subs/2_English.srt")
                ),
                entry_key: entry.entry_key.clone(),
                language: "en".into(),
                label: "English".into(),
                source: "external".into(),
                format: "srt".into(),
                file_path: srt_path.to_string_lossy().to_string(),
                fingerprint: entry.fingerprint.clone(),
            }],
        )
        .await
        .unwrap();

    let service = MediaService::with_transcoding(
        library,
        media_root,
        TranscodeConfig {
            enabled: false,
            ffmpeg_path: "ffmpeg".into(),
            session_dir: root.join("sessions"),
            max_upload_bps: 10_000_000,
            reserve_percent: 30,
            max_sessions: 1,
            idle_timeout: Duration::from_secs(300),
            segment_duration_secs: 4,
            ..Default::default()
        },
    );
    let negotiation = PeerRequest {
        path: format!("/play/{}", entry.entry_key),
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
    };
    let resolved = service.resolve(&negotiation).await;
    let Body::Bytes(body) = resolved.body else {
        panic!("playback plan must be JSON")
    };
    let plan: PlaybackPlan = serde_json::from_slice(&body).unwrap();
    assert_eq!(plan.subtitles.len(), 1);
    assert_eq!(plan.subtitles[0].source, "external");
    assert_eq!(plan.subtitles[0].language, "en");

    let served = service
        .resolve(&request(plan.subtitles[0].path.clone()))
        .await;
    assert_eq!(served.header.status, 200);
    assert_eq!(
        served.header.content_type.as_deref(),
        Some("text/vtt; charset=utf-8")
    );
    let Body::Bytes(vtt) = served.body else {
        panic!("subtitle must be served as bytes")
    };
    let vtt = String::from_utf8(vtt).unwrap();
    assert!(
        vtt.starts_with("WEBVTT"),
        "srt is converted to webvtt: {vtt}"
    );
    assert!(vtt.contains("00:00:01.500 --> 00:00:03.000"));
    assert!(vtt.contains("Don't let yourself get attached"));

    drop(service);
    let _ = std::fs::remove_dir_all(root);
}
