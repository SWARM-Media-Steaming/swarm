//! Scan/store delta correctness — the Phase 2 exit criteria for the library
//! engine: add, modify, rename, delete are all reflected in entries, the
//! pending-changes queue, the deleted-archive, and the thumbprint.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use swarm_core::entry_key::entry_key;
use swarm_core::peer::{AudioStreamInfo, MediaKind, SkipSegment, SkipSegmentKind, TrackLyrics};
use swarm_media::roots::{MediaRoot, MediaRootAssetType, RootResolver, SharedRootResolver};
use swarm_media::scan::{
    scan_root, scan_roots, scan_roots_cancellable, scan_roots_scoped,
    scan_roots_with_options, ScanError, ScanOptions,
};
use swarm_media::store::{ArtworkKind, EntryRecord, Library, MissingDisposition, SubtitleRecord};

struct Fixture {
    root: PathBuf,
    library: Library,
    _db_path: PathBuf,
}

async fn fixture(tag: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("swarm-lib-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("media");
    std::fs::create_dir_all(&root).unwrap();
    let db_path = base.join("library.sqlite");
    let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
    Fixture {
        root,
        library,
        _db_path: db_path,
    }
}

fn write(root: &Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[tokio::test]
async fn cancelled_scan_stops_before_catalog_reconciliation() {
    let fx = fixture("cancelled-scan").await;
    write(&fx.root, "movie.mp4", b"media");
    let cancel = Arc::new(AtomicBool::new(true));
    let roots = vec![MediaRoot {
        label: "local".into(),
        path: fx.root.clone(),
        asset_type: Default::default(),
    }];

    let error = scan_roots_cancellable(&fx.library, &roots, None, cancel)
        .await
        .unwrap_err();

    assert!(matches!(error, ScanError::Cancelled));
    assert!(fx.library.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn transcription_queue_resumes_segments_and_cascades_with_media() {
    let fx = fixture("transcription-queue").await;
    let entry = EntryRecord {
        entry_key: "0123456789abcdef01234567".into(),
        relative_path: "movies/example.mp4".into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size: 100,
        modified_time: 1,
        fingerprint: "media-fingerprint".into(),
        artist: None,
        album: None,
        track_number: None,
        show_title: None,
        season: None,
        episode: None,
        year: None,
        duration_secs: Some(1_201.0),
        video: None,
        audio: Some(AudioStreamInfo {
            codec: "aac".into(),
            channels: 2,
            bitrate: Some(128_000),
        }),
        scraped_title: None,
        episode_title: None,
        genres: Vec::new(),
        artwork_version: 0,
        cast: Vec::new(),
        overview: None,
        rating: None,
        community_rating: None,
        community_rating_votes: None,
    };
    fx.library.upsert(&entry).await.unwrap();
    assert_eq!(
        fx.library
            .enqueue_missing_transcriptions("small.en", "en", 600, false)
            .await
            .unwrap(),
        1
    );
    let first = fx
        .library
        .claim_next_transcription()
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.total_segments, first.completed_segments), (3, 0));
    fx.library
        .store_transcription_segment(&entry.entry_key, 0, "[]")
        .await
        .unwrap();

    // Simulate a real process exit after one durable segment was committed.
    fx.library
        .recover_interrupted_transcriptions()
        .await
        .unwrap();
    let resumed = fx
        .library
        .claim_next_transcription()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.completed_segments, 1);
    fx.library
        .store_transcription_segment(&entry.entry_key, 1, "[]")
        .await
        .unwrap();
    fx.library
        .store_transcription_segment(&entry.entry_key, 2, "[]")
        .await
        .unwrap();
    fx.library
        .complete_transcription(&SubtitleRecord {
            id: "whisper-en".into(),
            entry_key: entry.entry_key.clone(),
            language: "en".into(),
            label: "English — AI generated".into(),
            source: "whisper".into(),
            format: "vtt".into(),
            file_path: "/tmp/example.vtt".into(),
            fingerprint: entry.fingerprint.clone(),
        })
        .await
        .unwrap();
    let status = fx.library.transcription_queue_status().await.unwrap();
    assert_eq!(
        (
            status.completed,
            status.completed_segments,
            status.total_segments
        ),
        (1, 3, 3)
    );

    fx.library
        .remove_by_path(&entry.relative_path)
        .await
        .unwrap();
    assert!(fx
        .library
        .subtitle_tracks(&entry.entry_key)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        fx.library
            .transcription_queue_status()
            .await
            .unwrap()
            .total_segments,
        0
    );
}

fn movie_entry(entry_key: &str, relative_path: &str, fingerprint: &str) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size: 100,
        modified_time: 1,
        fingerprint: fingerprint.into(),
        artist: None,
        album: None,
        track_number: None,
        show_title: None,
        season: None,
        episode: None,
        year: None,
        duration_secs: Some(1_201.0),
        video: None,
        audio: Some(AudioStreamInfo {
            codec: "aac".into(),
            channels: 2,
            bitrate: Some(128_000),
        }),
        scraped_title: None,
        episode_title: None,
        genres: Vec::new(),
        artwork_version: 0,
        cast: Vec::new(),
        overview: None,
        rating: None,
        community_rating: None,
        community_rating_votes: None,
    }
}

#[tokio::test]
async fn introdb_segments_are_cached_and_version_the_catalog() {
    let fx = fixture("introdb-segments").await;
    let entry = movie_entry(
        "0123456789abcdef01234567",
        "movies/example.mp4",
        "introdb-fingerprint",
    );
    fx.library.upsert(&entry).await.unwrap();
    let before = fx.library.thumbprint().await.unwrap();
    let segments = vec![
        SkipSegment {
            kind: SkipSegmentKind::Intro,
            start_ms: Some(30_000),
            end_ms: Some(90_000),
        },
        SkipSegment {
            kind: SkipSegmentKind::Credits,
            start_ms: Some(3_000_000),
            end_ms: None,
        },
    ];

    fx.library
        .set_introdb_segments(&entry.entry_key, 27205, &segments)
        .await
        .unwrap();
    let (after, catalog) = fx.library.catalog_snapshot().await.unwrap();

    assert_ne!(before, after);
    assert_eq!(catalog[0].skip_segments, segments);
}

#[tokio::test]
async fn introdb_segments_for_reports_scrape_state() {
    let fx = fixture("introdb-segments-for").await;
    let entry = movie_entry(
        "0123456789abcdef01234567",
        "movies/example.mp4",
        "introdb-fingerprint",
    );
    fx.library.upsert(&entry).await.unwrap();

    // Never scraped: no cached lookup at all.
    assert_eq!(
        fx.library
            .introdb_segments_for(&entry.entry_key)
            .await
            .unwrap(),
        None
    );

    // Scraped, but TheIntroDB published nothing: an empty (but present) result.
    fx.library
        .set_introdb_segments(&entry.entry_key, 27205, &[])
        .await
        .unwrap();
    assert_eq!(
        fx.library
            .introdb_segments_for(&entry.entry_key)
            .await
            .unwrap(),
        Some(Vec::new())
    );

    // Scraped with markers.
    let segments = vec![SkipSegment {
        kind: SkipSegmentKind::Intro,
        start_ms: Some(30_000),
        end_ms: Some(90_000),
    }];
    fx.library
        .set_introdb_segments(&entry.entry_key, 27205, &segments)
        .await
        .unwrap();
    assert_eq!(
        fx.library
            .introdb_segments_for(&entry.entry_key)
            .await
            .unwrap(),
        Some(segments)
    );
}

#[tokio::test]
async fn bulk_enqueue_can_skip_entries_with_any_existing_subtitle() {
    let fx = fixture("skip-existing-subtitles").await;
    let with_subtitle = movie_entry("a1", "movies/a.mp4", "fp-a");
    let without_subtitle = movie_entry("b2", "movies/b.mp4", "fp-b");
    fx.library.upsert(&with_subtitle).await.unwrap();
    fx.library.upsert(&without_subtitle).await.unwrap();
    // A downloaded (non-Whisper) subtitle counts too — "any existing
    // subtitle track", not just a prior Whisper job.
    fx.library
        .upsert_subtitle(&SubtitleRecord {
            id: "opensubtitles-en".into(),
            entry_key: with_subtitle.entry_key.clone(),
            language: "en".into(),
            label: "English".into(),
            source: "opensubtitles".into(),
            format: "vtt".into(),
            file_path: "/tmp/downloaded.vtt".into(),
            fingerprint: with_subtitle.fingerprint.clone(),
        })
        .await
        .unwrap();

    let queued = fx
        .library
        .enqueue_missing_transcriptions("small.en", "en", 600, true)
        .await
        .unwrap();
    assert_eq!(queued, 1);
    let job = fx
        .library
        .claim_next_transcription()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.entry_key, without_subtitle.entry_key);
    assert!(fx
        .library
        .claim_next_transcription()
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn targeted_enqueue_forces_a_fresh_job_and_rejects_ineligible_entries() {
    let fx = fixture("targeted-enqueue").await;
    let entry = movie_entry("c3", "movies/c.mp4", "fp-c");
    fx.library.upsert(&entry).await.unwrap();
    fx.library
        .enqueue_missing_transcriptions("small.en", "en", 600, false)
        .await
        .unwrap();
    let job = fx
        .library
        .claim_next_transcription()
        .await
        .unwrap()
        .unwrap();
    fx.library
        .store_transcription_segment(&entry.entry_key, 0, "[]")
        .await
        .unwrap();
    fx.library
        .store_transcription_segment(&entry.entry_key, 1, "[]")
        .await
        .unwrap();
    fx.library
        .store_transcription_segment(&entry.entry_key, 2, "[]")
        .await
        .unwrap();
    fx.library
        .complete_transcription(&SubtitleRecord {
            id: "whisper-en".into(),
            entry_key: entry.entry_key.clone(),
            language: "en".into(),
            label: "English — AI generated".into(),
            source: "whisper".into(),
            format: "vtt".into(),
            file_path: "/tmp/c.vtt".into(),
            fingerprint: job.fingerprint.clone(),
        })
        .await
        .unwrap();

    // A completed job for the current fingerprint is normally never
    // re-queued by bulk enqueue...
    let bulk_requeued = fx
        .library
        .enqueue_missing_transcriptions("small.en", "en", 600, false)
        .await
        .unwrap();
    assert_eq!(bulk_requeued, 0);

    // ...but a targeted request always forces a fresh job.
    let targeted = fx
        .library
        .enqueue_transcription_for_entry(&entry.entry_key, "small.en", "en", 600)
        .await
        .unwrap();
    assert!(targeted);
    let requeued = fx
        .library
        .claim_next_transcription()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (requeued.total_segments, requeued.completed_segments),
        (3, 0)
    );
    assert!(fx
        .library
        .subtitle_tracks(&entry.entry_key)
        .await
        .unwrap()
        .is_empty());

    let ineligible = fx
        .library
        .enqueue_transcription_for_entry("does-not-exist", "small.en", "en", 600)
        .await
        .unwrap();
    assert!(!ineligible);
}

#[tokio::test]
async fn scan_add_modify_rename_delete() {
    let fx = fixture("delta").await;

    // --- initial add ---
    write(
        &fx.root,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![1u8; 4096],
    );
    write(
        &fx.root,
        "music/Artist/Album/01 - Song.flac",
        b"fake flac bytes",
    );
    write(&fx.root, "movies/Heat (1995)/poster.jpg", b"not media"); // must be ignored
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, 2);
    assert_eq!(report.removed, 0);

    let entries = fx.library.list().await.unwrap();
    assert_eq!(entries.len(), 2);
    let movie = entries.iter().find(|e| e.kind == MediaKind::Movie).unwrap();
    let track = entries.iter().find(|e| e.kind == MediaKind::Track).unwrap();
    // "1995" is a standalone dot-delimited token, captured into `year` and
    // stripped from the title the same way a bracketed year already is —
    // see classify.rs's extract_bare_year_token.
    assert_eq!(movie.title, "Heat");
    assert_eq!(movie.year, Some(1995));
    assert_eq!(track.artist.as_deref(), Some("Artist"));
    assert_eq!(track.track_number, Some(1));
    let thumbprint_v1 = fx.library.thumbprint().await.unwrap();

    // Pending changes queue saw both adds; clearing marks the clean point.
    let changes = fx.library.pending_changes().await.unwrap();
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|c| c.operation == "upsert"));
    fx.library.clear_pending_changes().await.unwrap();

    // --- unchanged rescan is a no-op ---
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.unchanged, 2);
    assert_eq!(report.added + report.updated + report.removed, 0);
    assert!(fx.library.pending_changes().await.unwrap().is_empty());
    assert_eq!(fx.library.thumbprint().await.unwrap(), thumbprint_v1);

    // --- modify (size change forces re-fingerprint) ---
    write(
        &fx.root,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![2u8; 8192],
    );
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.updated, 1);
    let updated = fx.library.get(&movie.entry_key).await.unwrap().unwrap();
    assert_eq!(updated.size, 8192);
    assert_ne!(updated.fingerprint, movie.fingerprint);
    let thumbprint_v2 = fx.library.thumbprint().await.unwrap();
    assert_ne!(thumbprint_v2, thumbprint_v1);
    let changes = fx.library.pending_changes().await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].entry_key, movie.entry_key);
    fx.library.clear_pending_changes().await.unwrap();

    // --- rename = delete old path + add new path ---
    std::fs::rename(
        fx.root.join("movies/Heat (1995)/Heat.1995.mkv"),
        fx.root.join("movies/Heat (1995)/Heat.Remastered.mkv"),
    )
    .unwrap();
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!((report.added, report.removed), (1, 1));
    assert!(fx.library.get(&movie.entry_key).await.unwrap().is_none());
    let entries = fx.library.list().await.unwrap();
    let renamed = entries.iter().find(|e| e.kind == MediaKind::Movie).unwrap();
    assert_eq!(
        renamed.relative_path,
        "movies/Heat (1995)/Heat.Remastered.mkv"
    );
    // Same bytes, new path: content identity survives the rename.
    assert_eq!(renamed.fingerprint, updated.fingerprint);
    let changes = fx.library.pending_changes().await.unwrap();
    assert_eq!(changes.len(), 2);
    assert!(changes
        .iter()
        .any(|c| c.operation == "delete" && c.entry_key == movie.entry_key));
    assert!(changes
        .iter()
        .any(|c| c.operation == "upsert" && c.entry_key == renamed.entry_key));
    fx.library.clear_pending_changes().await.unwrap();

    // --- delete ---
    std::fs::remove_file(fx.root.join("music/Artist/Album/01 - Song.flac")).unwrap();
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(fx.library.list().await.unwrap().len(), 1);
    let changes = fx.library.pending_changes().await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].operation, "delete");
    assert_eq!(changes[0].entry_key, track.entry_key);
}

#[tokio::test]
async fn modified_time_change_with_same_size_triggers_update() {
    let fx = fixture("mtime-delta").await;
    write(&fx.root, "movies/example.mkv", b"same-size");
    scan_root(&fx.library, &fx.root).await.unwrap();
    let path = fx.root.join("movies/example.mkv");
    let old = fx.library.list().await.unwrap().remove(0);
    filetime::set_file_mtime(
        &path,
        filetime::FileTime::from_unix_time(old.modified_time + 5, 0),
    )
    .unwrap();

    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.updated, 1);
    assert_eq!(report.unchanged, 0);
}

#[tokio::test]
async fn comprehensive_check_detects_content_hidden_by_size_and_timestamp() {
    let fx = fixture("comprehensive-content-delta").await;
    let relative = "movies/example.mkv";
    write(&fx.root, relative, b"contents-a");
    scan_root(&fx.library, &fx.root).await.unwrap();
    let original = fx.library.list().await.unwrap().remove(0);
    let path = fx.root.join(relative);

    write(&fx.root, relative, b"contents-b");
    filetime::set_file_mtime(
        &path,
        filetime::FileTime::from_unix_time(original.modified_time, 0),
    )
    .unwrap();

    let roots = [MediaRoot {
        label: "local".into(),
        path: fx.root.clone(),
        asset_type: MediaRootAssetType::Movies,
    }];
    let fast = scan_roots_with_options(
        &fx.library,
        &roots,
        None,
        ScanOptions {
            comprehensive_check: false,
            scan_music_tracks: false,
        },
    )
    .await
    .unwrap();
    assert_eq!((fast.updated, fast.unchanged), (0, 1));
    assert_eq!(fx.library.list().await.unwrap()[0].fingerprint, original.fingerprint);

    let comprehensive = scan_roots_with_options(
        &fx.library,
        &roots,
        None,
        ScanOptions {
            comprehensive_check: true,
            scan_music_tracks: false,
        },
    )
    .await
    .unwrap();
    assert_eq!((comprehensive.updated, comprehensive.unchanged), (1, 0));
    assert_ne!(fx.library.list().await.unwrap()[0].fingerprint, original.fingerprint);
}

#[tokio::test]
async fn typed_roots_filter_media_and_music_tracks_are_opt_in() {
    let fx = fixture("typed-root-filter").await;
    write(&fx.root, "movie.mkv", b"video");
    write(&fx.root, "Artist/Album/01 Song.flac", b"audio");
    let roots = [MediaRoot {
        label: "local".into(),
        path: fx.root.clone(),
        asset_type: MediaRootAssetType::Music,
    }];

    let skipped = scan_roots_with_options(
        &fx.library,
        &roots,
        None,
        ScanOptions {
            comprehensive_check: false,
            scan_music_tracks: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(skipped.added, 0);

    let scanned = scan_roots_with_options(
        &fx.library,
        &roots,
        None,
        ScanOptions {
            comprehensive_check: false,
            scan_music_tracks: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(scanned.added, 1);
    assert_eq!(fx.library.list().await.unwrap()[0].kind, MediaKind::Track);

    let disabled = scan_roots_with_options(
        &fx.library,
        &roots,
        None,
        ScanOptions {
            comprehensive_check: false,
            scan_music_tracks: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(disabled.removed, 1);
    assert!(fx.library.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn scan_indexes_scene_release_movies_with_scrapeable_titles() {
    let fx = fixture("scene-release-movie-titles").await;
    let cases = [
        (
            "Social Network.2010.BD.Rip.1080p.h264.Rus.Eng.mkv",
            "Social Network",
            2010,
        ),
        (
            "The.Prestige.2006.CUSTOM.MULTi.VF2.1080p.HDLight.AC3.5.1.H264-LiHDL.mkv",
            "The Prestige",
            2006,
        ),
        (
            "Top.Gun.Maverick.2022.IMAX.1080p.Bluray.Atmos.TrueHD.7.1.x264-EVO.mkv",
            "Top Gun Maverick",
            2022,
        ),
        (
            "Waterworld.1995.The.Ulysses.Cut.1080p.BluRay.HEVC.x265-RiPRG.mkv",
            "Waterworld",
            1995,
        ),
    ];
    for (filename, _, _) in cases {
        write(&fx.root, &format!("movies/{filename}"), b"fake video bytes");
    }

    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, cases.len() as u64);

    let entries = fx.library.list().await.unwrap();
    for (filename, expected_title, expected_year) in cases {
        let entry = entries
            .iter()
            .find(|entry| entry.relative_path.ends_with(filename))
            .unwrap_or_else(|| panic!("missing scanned entry for {filename}"));
        assert_eq!(entry.kind, MediaKind::Movie, "{filename}");
        assert_eq!(entry.title, expected_title, "{filename}");
        assert_eq!(entry.year, Some(expected_year), "{filename}");
    }
}

// --- real bug, found live: a network mount dropping mid-session made an
// unreachable root look identical to "the user deleted every file", and a
// rescan wiped the entire local library even though the real files were
// untouched on the still-alive remote share. Two independent guards, tested
// separately: the root directory itself failing to even list (a dropped
// mount is usually a hard I/O error), and a root that's still readable but
// suspiciously empty (belt-and-suspenders for any other root-level glitch
// that isn't a hard error).

#[tokio::test]
async fn rescan_of_a_root_that_stops_existing_refuses_to_wipe_the_known_library() {
    let fx = fixture("root-disappears").await;
    write(
        &fx.root,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![1u8; 4096],
    );
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, 1);
    assert_eq!(fx.library.list().await.unwrap().len(), 1);

    // Simulate a dropped network mount: the root path itself stops
    // resolving to anything readable (unlike a real deletion, where the
    // parent directory stays readable and just returns fewer entries).
    std::fs::remove_dir_all(&fx.root).unwrap();

    let result = scan_root(&fx.library, &fx.root).await;
    assert!(
        result.is_err(),
        "a root that vanished entirely must be a hard error, not an empty scan"
    );
    let error = result.unwrap_err().to_string();
    assert!(
        error.contains("local"),
        "error must identify the root label"
    );
    assert!(
        error.contains(&fx.root.to_string_lossy().to_string()),
        "error must identify the unavailable root path"
    );
    assert_eq!(
        fx.library.list().await.unwrap().len(),
        1,
        "the known library must survive an unreachable root untouched"
    );
}

#[tokio::test]
async fn rescan_that_finds_zero_files_anywhere_refuses_to_wipe_a_nonempty_known_library() {
    let fx = fixture("suspicious-empty").await;
    write(
        &fx.root,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![1u8; 4096],
    );
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, 1);

    // Root directory still exists and is readable, but every file under it
    // is gone — the "readable but empty" case `SuspiciousEmptyScan` guards,
    // distinct from the hard-I/O-error case above.
    std::fs::remove_file(fx.root.join("movies/Heat (1995)/Heat.1995.mkv")).unwrap();

    let result = scan_root(&fx.library, &fx.root).await;
    assert!(
        result.is_err(),
        "finding 0 files against a nonempty known library must refuse, not wipe everything"
    );
    assert_eq!(
        fx.library.list().await.unwrap().len(),
        1,
        "the known library must survive untouched"
    );
}

#[tokio::test]
async fn a_genuinely_empty_root_on_first_scan_is_not_treated_as_suspicious() {
    // No known entries yet — an empty root the very first time is completely
    // normal (a fresh install, or a root that's legitimately empty so far),
    // not a signal anything's wrong.
    let fx = fixture("first-scan-empty").await;
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!((report.added, report.removed), (0, 0));
}

#[tokio::test]
async fn rescan_relinks_artwork_files_still_on_disk_after_the_catalog_row_is_gone() {
    // Real gap, found live: scraped artwork lives as a plain file beside the
    // source media, entirely independent of the SQLite catalog — it
    // survives a catalog wipe even though the DB row referencing it
    // doesn't. A brand-new row (this test simulates the DB-row-lost, file-
    // still-there case directly, without needing a real wipe) must relink
    // whatever's already sitting in `images/` rather than leaving artwork
    // unset until a full re-scrape.
    let fx = fixture("artwork-recovery").await;
    write(
        &fx.root,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![1u8; 4096],
    );
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, 1);
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();

    // Simulate a completed prior scrape: a real poster file on disk, in the
    // exact convention `save_video_artwork` uses, plus the DB pointer a
    // real scrape would have written.
    let images_dir = fx.root.join("movies/Heat (1995)/images");
    std::fs::create_dir_all(&images_dir).unwrap();
    let poster_path = images_dir.join("Heat_1995-tmdb-poster.jpg");
    std::fs::write(&poster_path, b"fake poster bytes").unwrap();
    fx.library
        .set_artwork(
            &entry.entry_key,
            ArtworkKind::Poster,
            "movies/Heat (1995)/images/Heat_1995-tmdb-poster.jpg",
        )
        .await
        .unwrap();
    assert!(fx
        .library
        .artwork(&entry.entry_key, ArtworkKind::Poster)
        .await
        .unwrap()
        .is_some());

    // Simulate the catalog row being lost (a wipe) without touching the
    // real file on disk — same entry_key will be produced by the rescan
    // below, since it's purely path-derived.
    fx.library
        .remove_by_path(&entry.relative_path)
        .await
        .unwrap();
    assert!(fx
        .library
        .artwork(&entry.entry_key, ArtworkKind::Poster)
        .await
        .unwrap()
        .is_none());

    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(
        report.added, 1,
        "the row is gone, so this is a fresh add again, not an update"
    );
    let recovered = fx
        .library
        .artwork(&entry.entry_key, ArtworkKind::Poster)
        .await
        .unwrap();
    assert_eq!(
        recovered.map(|(path, _)| path),
        Some("movies/Heat (1995)/images/Heat_1995-tmdb-poster.jpg".to_string())
    );
}

#[tokio::test]
async fn rescan_relinks_artwork_for_an_already_known_unchanged_entry_too() {
    // Real bug, found live: entries added by a scan that ran *before*
    // artwork recovery existed stay "unchanged" (same size/mtime) forever
    // on every later rescan — the fast-path `continue` never even reaches
    // the artwork check, so they'd otherwise never recover their real,
    // still-on-disk artwork short of a full library wipe.
    let fx = fixture("artwork-recovery-unchanged").await;
    write(
        &fx.root,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![1u8; 4096],
    );
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, 1);
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();

    // A poster file appears on disk (e.g. from a scrape this test doesn't
    // otherwise simulate) but nothing ever links it in the DB.
    let images_dir = fx.root.join("movies/Heat (1995)/images");
    std::fs::create_dir_all(&images_dir).unwrap();
    std::fs::write(
        images_dir.join("Heat_1995-tmdb-poster.jpg"),
        b"fake poster bytes",
    )
    .unwrap();
    assert!(fx
        .library
        .artwork(&entry.entry_key, ArtworkKind::Poster)
        .await
        .unwrap()
        .is_none());

    // Same file, unchanged — a normal rescan must still pick up the
    // artwork sitting right next to it.
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.unchanged, 1);
    let recovered = fx
        .library
        .artwork(&entry.entry_key, ArtworkKind::Poster)
        .await
        .unwrap();
    assert_eq!(
        recovered.map(|(path, _)| path),
        Some("movies/Heat (1995)/images/Heat_1995-tmdb-poster.jpg".to_string())
    );
}

#[tokio::test]
async fn artwork_recovery_never_cross_links_a_sibling_movies_poster() {
    // Real bug, found live and visually confirmed (screenshot): movies that
    // share one `images/` folder (a flat movie root with no per-movie
    // subfolder — the overwhelmingly common real-world layout) all got the
    // *same* poster — whichever one the directory listing happened to
    // return, regardless of which movie it actually belonged to. Every
    // movie in a shared folder must only ever pick up its own,
    // stem-matched poster/backdrop.
    let fx = fixture("artwork-no-cross-link").await;
    write(&fx.root, "10 Cloverfield Lane (2016).mkv", b"a");
    write(&fx.root, "Jaws 2 (1978).mkv", b"b");
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, 2);

    // Both movies' posters land in the SAME shared images/ folder, since
    // both files are direct siblings at the media root. Filenames match
    // `sanitize_stem`'s real output exactly (parens preserved).
    let images_dir = fx.root.join("images");
    std::fs::create_dir_all(&images_dir).unwrap();
    std::fs::write(
        images_dir.join("10 Cloverfield Lane (2016)-tmdb-poster.jpg"),
        b"cloverfield poster",
    )
    .unwrap();
    std::fs::write(
        images_dir.join("Jaws 2 (1978)-tmdb-poster.jpg"),
        b"jaws poster",
    )
    .unwrap();

    // Force both entries through the recovery path (as if the catalog had
    // just been rebuilt) by clearing their DB rows and rescanning — the
    // same real-world trigger the live bug was hit through.
    let entries = fx.library.list().await.unwrap();
    for e in &entries {
        fx.library.remove_by_path(&e.relative_path).await.unwrap();
    }
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, 2);

    let jaws = fx
        .library
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.relative_path.starts_with("Jaws"))
        .unwrap();
    let cloverfield = fx
        .library
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.relative_path.starts_with("10 Cloverfield"))
        .unwrap();

    let jaws_poster = fx
        .library
        .artwork(&jaws.entry_key, ArtworkKind::Poster)
        .await
        .unwrap();
    let cloverfield_poster = fx
        .library
        .artwork(&cloverfield.entry_key, ArtworkKind::Poster)
        .await
        .unwrap();

    assert_eq!(
        jaws_poster.map(|(p, _)| p),
        Some("images/Jaws 2 (1978)-tmdb-poster.jpg".to_string())
    );
    assert_eq!(
        cloverfield_poster.map(|(p, _)| p),
        Some("images/10 Cloverfield Lane (2016)-tmdb-poster.jpg".to_string())
    );
}

#[tokio::test]
async fn single_root_scan_roots_matches_scan_root_byte_for_byte() {
    // Backward-compat guarantee: scan_root (single PathBuf) must still
    // produce the exact relative_path/entry_key values it always has, since
    // it's a thin wrapper over scan_roots with one implicit "local" root
    // that scan_roots never prefixes when there's only one root configured.
    let fx = fixture("single-root-parity").await;
    write(
        &fx.root,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![1u8; 4096],
    );
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entries = fx.library.list().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].relative_path, "movies/Heat (1995)/Heat.1995.mkv");
    assert_eq!(
        entries[0].entry_key,
        entry_key("movies/Heat (1995)/Heat.1995.mkv")
    );
}

#[tokio::test]
async fn two_roots_with_the_same_relative_path_get_distinct_entry_keys() {
    let base = std::env::temp_dir().join(format!("swarm-lib-two-roots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root_a = base.join("root-a");
    let root_b = base.join("root-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let library = Library::open(base.join("library.sqlite").to_str().unwrap())
        .await
        .unwrap();

    // Same sub-path under both roots — must not collide.
    write(
        &root_a,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![1u8; 4096],
    );
    write(
        &root_b,
        "movies/Heat (1995)/Heat.1995.mkv",
        &vec![2u8; 4096],
    );

    let roots = [
        MediaRoot {
            label: "local".to_string(),
            path: root_a.clone(),
            asset_type: Default::default(),
        },
        MediaRoot {
            label: "nas".to_string(),
            path: root_b.clone(),
            asset_type: Default::default(),
        },
    ];
    let report = scan_roots(&library, &roots, None).await.unwrap();
    assert_eq!(report.added, 2);

    let entries = library.list().await.unwrap();
    assert_eq!(entries.len(), 2);
    let paths: Vec<&str> = entries.iter().map(|e| e.relative_path.as_str()).collect();
    assert!(paths.contains(&"local/movies/Heat (1995)/Heat.1995.mkv"));
    assert!(paths.contains(&"nas/movies/Heat (1995)/Heat.1995.mkv"));
    assert_ne!(entries[0].entry_key, entries[1].entry_key);
    assert_ne!(entries[0].fingerprint, entries[1].fingerprint); // distinct content too
}

#[tokio::test]
async fn scoped_rescan_reconciles_only_the_selected_multi_root() {
    let base = std::env::temp_dir().join(format!("swarm-lib-scoped-roots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let local = base.join("local");
    let nas = base.join("nas");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&nas).unwrap();
    let library = Library::open(base.join("library.sqlite").to_str().unwrap())
        .await
        .unwrap();
    write(&local, "movies/Local One.mkv", &[1u8; 16]);
    write(&local, "movies/Local Two.mkv", &[2u8; 16]);
    write(&nas, "movies/NAS One.mkv", &[3u8; 16]);
    write(&nas, "movies/NAS Two.mkv", &[4u8; 16]);
    let roots = vec![
        MediaRoot {
            label: "local".into(),
            path: local.clone(),
            asset_type: Default::default(),
        },
        MediaRoot {
            label: "nas".into(),
            path: nas.clone(),
            asset_type: Default::default(),
        },
    ];
    scan_roots(&library, &roots, None).await.unwrap();

    std::fs::remove_file(local.join("movies/Local Two.mkv")).unwrap();
    std::fs::remove_file(nas.join("movies/NAS Two.mkv")).unwrap();
    let report = scan_roots_scoped(&library, &roots[..1], true, None)
        .await
        .unwrap();

    assert_eq!((report.removed, report.unchanged), (1, 1));
    let paths = library
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.relative_path)
        .collect::<Vec<_>>();
    assert!(!paths.contains(&"local/movies/Local Two.mkv".to_string()));
    assert!(
        paths.contains(&"nas/movies/NAS Two.mkv".to_string()),
        "a scoped local rescan must not reconcile an unscanned NAS root"
    );
}

/// Real bug (#72): a root nested inside (or identical to) another already-
/// configured root — e.g. a dedicated mount added for one show's folder on
/// top of an existing umbrella "TV Shows" root — got walked a second time
/// under its own `{label}/` prefix, silently duplicating every entry under
/// the overlap even though there was exactly one copy of each file on disk.
/// The outer root's own recursive walk already covers everything under the
/// nested one, so the nested root must always be the one skipped —
/// regardless of which order the two were configured in.
#[tokio::test]
async fn overlapping_roots_are_scanned_once_regardless_of_configuration_order() {
    for outer_first in [true, false] {
        let base = std::env::temp_dir().join(format!(
            "swarm-lib-overlap-{outer_first}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let tv = base.join("tv");
        let office = tv.join("The Office");
        std::fs::create_dir_all(&office).unwrap();
        let library = Library::open(base.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap();
        write(&tv, "The Office/S01E01.mkv", &[1u8; 16]);
        write(&tv, "Other Show/S01E01.mkv", &[2u8; 16]);

        let outer = MediaRoot {
            label: "tv".into(),
            path: tv.clone(),
            asset_type: Default::default(),
        };
        let inner = MediaRoot {
            label: "office".into(),
            path: office.clone(),
            asset_type: Default::default(),
        };
        let roots = if outer_first {
            vec![outer, inner]
        } else {
            vec![inner, outer]
        };

        let report = scan_roots(&library, &roots, None).await.unwrap();
        assert_eq!(
            report.added, 2,
            "outer_first={outer_first}: the nested root's own file must not be counted twice"
        );
        let paths = library
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.relative_path)
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 2, "outer_first={outer_first}: {paths:?}");
        assert!(paths.iter().any(|p| p.ends_with("The Office/S01E01.mkv")));
        assert!(paths.iter().any(|p| p.ends_with("Other Show/S01E01.mkv")));
    }
}

/// Same real bug as above, from the angle of an *already* affected install:
/// before this fix, two overlapping roots ("tv" and a nested "office" mount)
/// were both walked, cataloging the exact same on-disk file twice — once
/// under each root's `{label}/` prefix. Simulate that pre-existing
/// duplicate row directly (rather than by reverting the fix), then confirm
/// a normal rescan with the fixed code reconciles it away on its own: the
/// nested root's stale duplicate must end up unavailable, not just stop
/// growing further, leaving exactly one available entry for the file.
#[tokio::test]
async fn overlapping_root_self_heals_a_pre_existing_duplicate_on_rescan() {
    let base = std::env::temp_dir().join(format!("swarm-lib-overlap-heal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let tv = base.join("tv");
    let office = tv.join("The Office");
    std::fs::create_dir_all(&office).unwrap();
    let library = Library::open(base.join("library.sqlite").to_str().unwrap())
        .await
        .unwrap();
    write(&tv, "The Office/S01E01.mkv", &[1u8; 16]);

    // Pre-existing duplicate row from before this fix existed: the nested
    // root's own walk had already catalogued the file under its `office/`
    // prefix.
    let stale_key = entry_key("office/S01E01.mkv");
    library
        .upsert(&movie_entry(&stale_key, "office/S01E01.mkv", "stale-fp"))
        .await
        .unwrap();

    let roots = vec![
        MediaRoot {
            label: "office".into(),
            path: office.clone(),
            asset_type: Default::default(),
        },
        MediaRoot {
            label: "tv".into(),
            path: tv.clone(),
            asset_type: Default::default(),
        },
    ];
    scan_roots(&library, &roots, None).await.unwrap();

    let after = library.list().await.unwrap();
    assert_eq!(
        after.len(),
        1,
        "the pre-existing duplicate under the redundant root must be reconciled away: {after:?}"
    );
    assert!(after[0].relative_path.ends_with("The Office/S01E01.mkv"));
    assert_ne!(
        after[0].entry_key, stale_key,
        "the surviving entry must be the one under the kept \"tv\" root, not the stale duplicate"
    );
}

#[tokio::test]
async fn manual_metadata_overwrites_display_fields_and_leaves_grouping_fields_untouched() {
    let fx = fixture("manual-metadata").await;
    write(
        &fx.root,
        "music/Artist/Album/01 - Song.flac",
        b"fake flac bytes",
    );
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    assert_eq!(entry.artist.as_deref(), Some("Artist"));
    assert_eq!(entry.album.as_deref(), Some("Album"));

    // Only title provided — genres must stay untouched (None means "leave
    // unchanged", not "clear").
    fx.library
        .set_manual_metadata(&entry.entry_key, Some("Manual Title"), None)
        .await
        .unwrap();
    let after_title = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(after_title.scraped_title.as_deref(), Some("Manual Title"));
    assert!(after_title.genres.is_empty());
    // Grouping fields are never touched by a manual edit.
    assert_eq!(after_title.artist.as_deref(), Some("Artist"));
    assert_eq!(after_title.album.as_deref(), Some("Album"));

    // A title having been set marks the entry as "processed" for the bulk
    // scraper's purposes, same as a real scrape result would.
    assert!(fx.library.missing_scrape().await.unwrap().is_empty());

    // Now set genres too, and clear the title explicitly via Some("").
    fx.library
        .set_manual_metadata(
            &entry.entry_key,
            Some(""),
            Some(&["Electronic".to_string()]),
        )
        .await
        .unwrap();
    let after_genres = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(after_genres.scraped_title, None); // empty string is the "no title override" sentinel
    assert_eq!(after_genres.genres, vec!["Electronic"]);
    assert_eq!(after_genres.artist.as_deref(), Some("Artist"));
}

#[tokio::test]
async fn clear_scrape_result_reverts_to_unscraped_and_leaves_grouping_fields_untouched() {
    use swarm_media::store::ArtworkKind;

    let fx = fixture("clear-scrape").await;
    write(&fx.root, "movies/Heat (1995)/Heat.1995.mkv", &[1u8; 10]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();

    fx.library
        .set_scrape_result(
            &entry.entry_key,
            Some("Heat"),
            &["Crime".to_string()],
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    fx.library
        .set_artwork(
            &entry.entry_key,
            ArtworkKind::Poster,
            "movies/Heat (1995)/images/poster.jpg",
        )
        .await
        .unwrap();
    let scraped = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(scraped.scraped_title.as_deref(), Some("Heat"));
    assert!(fx.library.missing_scrape().await.unwrap().is_empty());

    let cleared_paths = fx
        .library
        .clear_scrape_result(&entry.entry_key)
        .await
        .unwrap();
    assert_eq!(
        cleared_paths,
        vec!["movies/Heat (1995)/images/poster.jpg".to_string()]
    );

    let reverted = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(reverted.scraped_title, None);
    assert!(reverted.genres.is_empty());
    assert!(fx
        .library
        .artwork(&entry.entry_key, ArtworkKind::Poster)
        .await
        .unwrap()
        .is_none());
    // Path-derived fields are never touched by a scrape (or its reversal).
    assert_eq!(reverted.relative_path, scraped.relative_path);
    // Reverting must put it back into the bulk-scraper's "needs work" set.
    let missing = fx.library.missing_scrape().await.unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].entry_key, entry.entry_key);
}

#[tokio::test]
async fn track_lyrics_are_relational_cached_and_cleared_with_scrape_data() {
    let fx = fixture("track-lyrics").await;
    let relative_path = "music/Artist/Album/01 - Song.flac";
    write(&fx.root, relative_path, b"fake flac bytes");
    scan_root(&fx.library, &fx.root).await.unwrap();
    let mut entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    // Fake bytes cannot be probed; give this store-focused test the same
    // duration a real scanned track would have.
    entry.duration_secs = Some(213.7);
    fx.library.upsert(&entry).await.unwrap();
    assert_eq!(fx.library.missing_track_lyrics().await.unwrap().len(), 1);

    let lyrics = TrackLyrics {
        provider: "lrclib".into(),
        provider_id: Some(17),
        language: Some("en".into()),
        plain_lyrics: Some("First line\nSecond line".into()),
        synced_lyrics: Some("[00:01.00]First line\n[00:03.50]Second line".into()),
        instrumental: false,
    };
    fx.library
        .set_track_lyrics(&entry.entry_key, &lyrics)
        .await
        .unwrap();
    assert_eq!(
        fx.library.track_lyrics(&entry.entry_key).await.unwrap(),
        Some(lyrics.clone())
    );
    assert!(fx.library.missing_track_lyrics().await.unwrap().is_empty());

    fx.library
        .clear_scrape_result(&entry.entry_key)
        .await
        .unwrap();
    assert_eq!(
        fx.library.track_lyrics(&entry.entry_key).await.unwrap(),
        None
    );
    assert_eq!(fx.library.missing_track_lyrics().await.unwrap().len(), 1);

    fx.library
        .mark_track_lyrics_not_found(&entry.entry_key)
        .await
        .unwrap();
    assert_eq!(
        fx.library.track_lyrics(&entry.entry_key).await.unwrap(),
        None
    );
    assert!(
        fx.library.missing_track_lyrics().await.unwrap().is_empty(),
        "a fresh 404 must not be retried every run"
    );

    fx.library
        .set_track_lyrics(&entry.entry_key, &lyrics)
        .await
        .unwrap();
    fx.library.remove_by_path(relative_path).await.unwrap();
    assert_eq!(
        fx.library.track_lyrics(&entry.entry_key).await.unwrap(),
        None,
        "foreign-key cascade must remove orphaned lyrics"
    );
}

#[tokio::test]
async fn set_overview_round_trips_and_clear_scrape_result_wipes_it() {
    let fx = fixture("overview").await;
    write(&fx.root, "movies/Heat (1995)/Heat.1995.mkv", &[1u8; 10]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    assert_eq!(entry.overview, None);

    fx.library
        .set_overview(&entry.entry_key, "A group of professional bank robbers...")
        .await
        .unwrap();
    let with_overview = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(
        with_overview.overview.as_deref(),
        Some("A group of professional bank robbers...")
    );

    fx.library
        .clear_scrape_result(&entry.entry_key)
        .await
        .unwrap();
    let reverted = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(reverted.overview, None);
}

#[tokio::test]
async fn set_rating_round_trips_and_clear_scrape_result_wipes_it() {
    let fx = fixture("rating").await;
    write(&fx.root, "movies/Heat (1995)/Heat.1995.mkv", &[1u8; 10]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    assert_eq!(entry.rating, None);

    fx.library.set_rating(&entry.entry_key, "R").await.unwrap();
    let with_rating = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(with_rating.rating.as_deref(), Some("R"));
    assert_eq!(with_rating.to_catalog_entry().rating.as_deref(), Some("R"));

    fx.library
        .clear_scrape_result(&entry.entry_key)
        .await
        .unwrap();
    let reverted = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(reverted.rating, None);
}

#[tokio::test]
async fn scraped_content_and_community_ratings_round_trip_to_catalog() {
    let fx = fixture("scraped-community-rating").await;
    write(&fx.root, "movies/Heat (1995)/Heat.1995.mkv", &[1u8; 10]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();

    fx.library
        .set_scrape_result(
            &entry.entry_key,
            Some("Heat"),
            &["Crime".into()],
            &[],
            Some("R"),
            Some(8.3),
            Some(7_251),
        )
        .await
        .unwrap();

    let stored = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(stored.rating.as_deref(), Some("R"));
    assert_eq!(stored.community_rating, Some(8.3));
    assert_eq!(stored.community_rating_votes, Some(7_251));
    let catalog = stored.to_catalog_entry();
    assert_eq!(catalog.community_rating, Some(8.3));
    assert_eq!(catalog.community_rating_votes, Some(7_251));

    fx.library
        .clear_scrape_result(&entry.entry_key)
        .await
        .unwrap();
    let cleared = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(cleared.community_rating, None);
    assert_eq!(cleared.community_rating_votes, None);
}

#[tokio::test]
async fn distinct_genres_unions_across_entries_and_sorts_case_insensitively() {
    let fx = fixture("distinct-genres").await;
    write(&fx.root, "movies/Heat (1995)/Heat.1995.mkv", &[1u8; 10]);
    write(&fx.root, "movies/Alien (1979)/Alien.1979.mkv", &[1u8; 10]);
    write(&fx.root, "movies/Amelie (2001)/Amelie.2001.mkv", &[1u8; 10]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entries = fx.library.list().await.unwrap();

    // No genres assigned to any entry yet.
    assert_eq!(
        fx.library.distinct_genres().await.unwrap(),
        Vec::<String>::new()
    );

    for entry in &entries {
        let genres: &[String] = if entry.title.starts_with("Heat") {
            &["Crime".to_string(), "action".to_string()]
        } else if entry.title.starts_with("Alien") {
            &["Sci-Fi".to_string(), "action".to_string()] // "action" exactly repeated -> collapses to one
        } else {
            &[] // Amelie contributes nothing
        };
        fx.library
            .set_manual_metadata(&entry.entry_key, None, Some(genres))
            .await
            .unwrap();
    }

    // Exact-string dedup ("action" from both entries collapses to one), sorted
    // case-insensitively (lowercase "action" sorts with the Cs/Ss on its letter,
    // not after every uppercase-initial entry the way a plain byte sort would).
    assert_eq!(
        fx.library.distinct_genres().await.unwrap(),
        vec![
            "action".to_string(),
            "Crime".to_string(),
            "Sci-Fi".to_string()
        ]
    );
}

#[tokio::test]
async fn set_manual_kind_moves_a_movie_to_a_track_and_clears_movie_only_fields() {
    // A music video sitting under movies/ as an .mkv — exactly the real
    // scenario this escape hatch exists for.
    let fx = fixture("manual-kind-to-track").await;
    write(&fx.root, "movies/Some Music Video.mkv", &[1u8; 10]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    assert_eq!(entry.kind, MediaKind::Movie);

    fx.library
        .set_manual_kind(
            &entry.entry_key,
            MediaKind::Track,
            Some("The Artist"),
            Some("The Album"),
            None,
        )
        .await
        .unwrap();
    let moved = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(moved.kind, MediaKind::Track);
    assert_eq!(moved.artist.as_deref(), Some("The Artist"));
    assert_eq!(moved.album.as_deref(), Some("The Album"));
    assert_eq!(moved.show_title, None);

    // Moving it again, to Episode, must clear the artist/album a Track move just set.
    fx.library
        .set_manual_kind(
            &entry.entry_key,
            MediaKind::Episode,
            None,
            None,
            Some("Some Show"),
        )
        .await
        .unwrap();
    let moved_again = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(moved_again.kind, MediaKind::Episode);
    assert_eq!(moved_again.show_title.as_deref(), Some("Some Show"));
    assert_eq!(moved_again.artist, None);
    assert_eq!(moved_again.album, None);
}

#[tokio::test]
async fn set_manual_kind_survives_fix_classifications() {
    let fx = fixture("manual-kind-survives-reclassify").await;
    write(&fx.root, "movies/Some Music Video.mkv", &[1u8; 10]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    fx.library
        .set_manual_kind(
            &entry.entry_key,
            MediaKind::Track,
            Some("The Artist"),
            Some("The Album"),
            None,
        )
        .await
        .unwrap();

    let roots = SharedRootResolver::new(RootResolver::single(fx.root.clone()));
    let report = fx.library.reclassify_all(&roots).await.unwrap();
    assert_eq!(report.changed, 0);
    assert_eq!(report.unchanged, 1);

    let after = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    assert_eq!(after.kind, MediaKind::Track);
    assert_eq!(after.artist.as_deref(), Some("The Artist"));
}

#[tokio::test]
async fn set_manual_kind_survives_a_rescan_after_the_file_changes_on_disk() {
    let fx = fixture("manual-kind-survives-rescan").await;
    write(&fx.root, "movies/Some Music Video.mkv", &[1u8; 10]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    fx.library
        .set_manual_kind(
            &entry.entry_key,
            MediaKind::Track,
            Some("The Artist"),
            Some("The Album"),
            None,
        )
        .await
        .unwrap();

    // Change the file's content (different size, so scan_roots takes the
    // "changed" path, not the "unchanged, skip entirely" fast path).
    write(&fx.root, "movies/Some Music Video.mkv", &[1u8; 999]);
    let roots = vec![MediaRoot {
        label: "local".into(),
        path: fx.root.clone(),
        asset_type: Default::default(),
    }];
    let report = scan_roots(&fx.library, &roots, None).await.unwrap();
    assert_eq!(report.updated, 1);

    let after = fx.library.get(&entry.entry_key).await.unwrap().unwrap();
    // Kind/grouping preserved from the override, not reverted to Movie...
    assert_eq!(after.kind, MediaKind::Track);
    assert_eq!(after.artist.as_deref(), Some("The Artist"));
    assert_eq!(after.album.as_deref(), Some("The Album"));
    // ...but the technical fields did pick up the real on-disk change.
    assert_eq!(after.size, 999);
}

#[tokio::test]
async fn thumbprint_is_order_independent_and_content_sensitive() {
    let fx_a = fixture("tp-a").await;
    let fx_b = fixture("tp-b").await;
    // Same content written in different order → same thumbprint.
    write(&fx_a.root, "movies/A.mkv", b"aaaa");
    write(&fx_a.root, "movies/B.mkv", b"bbbb");
    write(&fx_b.root, "movies/B.mkv", b"bbbb");
    write(&fx_b.root, "movies/A.mkv", b"aaaa");
    scan_root(&fx_a.library, &fx_a.root).await.unwrap();
    scan_root(&fx_b.library, &fx_b.root).await.unwrap();
    assert_eq!(
        fx_a.library.thumbprint().await.unwrap(),
        fx_b.library.thumbprint().await.unwrap()
    );
}

#[tokio::test]
async fn thumbprint_changes_with_client_visible_catalog_metadata() {
    use swarm_media::store::ArtworkKind;

    let fx = fixture("tp-metadata").await;
    write(&fx.root, "movies/A.mkv", b"aaaa");
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().remove(0);
    let initial = fx.library.thumbprint().await.unwrap();

    fx.library
        .set_scrape_result(
            &entry.entry_key,
            Some("Scraped A"),
            &["Drama".into()],
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let scraped = fx.library.thumbprint().await.unwrap();
    assert_ne!(scraped, initial);

    fx.library
        .set_artwork(&entry.entry_key, ArtworkKind::Poster, "movies/A-poster.jpg")
        .await
        .unwrap();
    let artwork = fx.library.thumbprint().await.unwrap();
    assert_ne!(artwork, scraped);

    fx.library
        .set_like(&entry.entry_key, "tv-1", "Living Room", true)
        .await
        .unwrap();
    assert_ne!(fx.library.thumbprint().await.unwrap(), artwork);
}

#[tokio::test]
async fn reclassify_all_repairs_stale_bonus_content_and_leaves_correct_entries_untouched() {
    // scan_root always runs the CURRENT classify(), so it can't be used to
    // manufacture a stale pre-fix state — write the DB row directly instead,
    // exactly as the old buggy classify() would have (filed as a Movie, with
    // real-looking-but-wrong scraped data), matching the real bug this
    // guards: bonus content under a show's season folder scraped as if it
    // were a completely unrelated standalone movie.
    use swarm_media::classify::classify;
    use swarm_media::store::{ArtworkKind, EntryRecord};

    let fx = fixture("reclassify").await;

    let wrong_relative =
        "Shows/Dexter/Dexter (2006) S03/Featurettes/Interviews/Michael C. Hall.mkv";
    let wrong_entry = EntryRecord {
        entry_key: entry_key(wrong_relative),
        relative_path: wrong_relative.to_string(),
        kind: MediaKind::Movie,
        title: "Michael C Hall".to_string(),
        size: 10,
        modified_time: 0,
        fingerprint: "fp1".to_string(),
        artist: None,
        album: None,
        track_number: None,
        show_title: None,
        season: None,
        episode: None,
        year: None,
        duration_secs: None,
        video: None,
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
    fx.library.upsert(&wrong_entry).await.unwrap();
    // upsert() deliberately never writes scrape/artwork columns (a rescan
    // must never clobber existing scrape results) — set the "already
    // (wrongly) scraped" state the way a real scrape actually would.
    fx.library
        .set_scrape_result(
            &wrong_entry.entry_key,
            Some("The Interview"),
            &["Comedy".to_string()],
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    fx.library
        .set_artwork(
            &wrong_entry.entry_key,
            ArtworkKind::Poster,
            "Shows/Dexter/Dexter (2006) S03/images/wrong-poster.jpg",
        )
        .await
        .unwrap();

    // A genuinely correct entry — a real numbered episode — that reclassify
    // must leave completely untouched, including its existing scrape data.
    let correct_relative = "Shows/Dexter/Dexter (2006) S03/Dexter.S03E01.Our Father.mkv";
    let correct_classified = classify(correct_relative).unwrap();
    let correct_entry = EntryRecord {
        entry_key: entry_key(correct_relative),
        relative_path: correct_relative.to_string(),
        kind: correct_classified.kind,
        title: correct_classified.title.clone(),
        size: 20,
        modified_time: 0,
        fingerprint: "fp2".to_string(),
        artist: None,
        album: None,
        track_number: None,
        show_title: correct_classified.show_title.clone(),
        season: correct_classified.season,
        episode: correct_classified.episode,
        year: correct_classified.year,
        duration_secs: None,
        video: None,
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
    fx.library.upsert(&correct_entry).await.unwrap();
    fx.library
        .set_scrape_result(
            &correct_entry.entry_key,
            Some("Dexter"),
            &["Drama".to_string()],
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let roots = SharedRootResolver::new(RootResolver::single(fx.root.clone()));
    let report = fx.library.reclassify_all(&roots).await.unwrap();
    assert_eq!(report.changed, 1);
    assert_eq!(report.unchanged, 1);

    let fixed = fx
        .library
        .get(&wrong_entry.entry_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixed.kind, MediaKind::Episode);
    assert_eq!(fixed.show_title.as_deref(), Some("Dexter"));
    assert_eq!(fixed.season, Some(0));
    assert_eq!(fixed.episode, None);
    assert_eq!(
        fixed.scraped_title, None,
        "stale wrong scrape data must be cleared, not just relabeled"
    );
    assert!(fixed.genres.is_empty());
    assert!(
        fx.library
            .artwork(&fixed.entry_key, ArtworkKind::Poster)
            .await
            .unwrap()
            .is_none(),
        "the wrong movie's artwork must be cleared too"
    );

    let untouched = fx
        .library
        .get(&correct_entry.entry_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        untouched.scraped_title.as_deref(),
        Some("Dexter"),
        "already-correct entries must be left completely untouched"
    );
    assert_eq!(untouched.genres, vec!["Drama".to_string()]);
}

/// Companion to the bonus-content case above, guarding the other real bug
/// this same fix landed for: `reclassify_all`'s "unchanged" fast path used
/// to compare only kind/show_title/season/episode — never artist/album/
/// track_number — so it silently skipped every audio track whose *only*
/// wrong field was its (bottom-anchored, pre-fix) artist/album, even though
/// `classify()` itself had already been corrected. A track's kind never
/// changes (always `Track`) and show_title/season/episode are always None
/// for audio, so without comparing artist/album too this reclassify would
/// have reported the whole library "unchanged" and repaired nothing.
#[tokio::test]
async fn reclassify_all_repairs_a_track_whose_only_wrong_fields_are_artist_and_album() {
    use swarm_media::store::EntryRecord;

    let fx = fixture("reclassify-music").await;

    // A real DJ-mix-style deep nesting — classify() now correctly reads
    // artist/album from the top two folders, but this row was written the
    // old (bottom-anchored) way: garbage folder names one level up from the
    // file ended up as "artist"/"album".
    let relative = "Gabriel & Dresden/Organized Natures/01-29/29/track.mp3";
    let wrong_entry = EntryRecord {
        entry_key: entry_key(relative),
        relative_path: relative.to_string(),
        kind: MediaKind::Track,
        title: "track".to_string(),
        size: 10,
        modified_time: 0,
        fingerprint: "fp1".to_string(),
        artist: Some("01-29".to_string()),
        album: Some("29".to_string()),
        track_number: None,
        show_title: None,
        season: None,
        episode: None,
        year: None,
        duration_secs: None,
        video: None,
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
    fx.library.upsert(&wrong_entry).await.unwrap();
    fx.library
        .set_scrape_result(
            &wrong_entry.entry_key,
            Some("29 - Unknown"),
            &[],
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let roots = SharedRootResolver::new(RootResolver::single(fx.root.clone()));
    let report = fx.library.reclassify_all(&roots).await.unwrap();
    assert_eq!(report.changed, 1);
    assert_eq!(report.unchanged, 0);

    let fixed = fx
        .library
        .get(&wrong_entry.entry_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixed.artist.as_deref(), Some("Gabriel & Dresden"));
    assert_eq!(fixed.album.as_deref(), Some("Organized Natures"));
    assert_eq!(
        fixed.scraped_title, None,
        "stale wrong scrape data must be cleared, not just relabeled"
    );
    assert!(
        fx.library
            .has_archived_metadata(&wrong_entry.fingerprint, wrong_entry.size, false)
            .await
            .unwrap(),
        "reclassification must retain a detached rollback copy of invalidated metadata"
    );
}

#[tokio::test]
async fn moved_media_restores_archived_metadata_and_quarantines_the_old_path() {
    let fx = fixture("metadata-survives-move").await;
    let old_relative = "movies/Old Folder/Heat.1995.mkv";
    let new_relative = "movies/New Folder/Heat.1995.mkv";
    write(&fx.root, old_relative, &[7u8; 8_192]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let old_entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    fx.library
        .set_scrape_result(
            &old_entry.entry_key,
            Some("Heat"),
            &["Crime".into(), "Drama".into()],
            &[],
            Some("R"),
            Some(8.2),
            Some(12_345),
        )
        .await
        .unwrap();
    fx.library
        .set_overview(
            &old_entry.entry_key,
            "A meticulous detective pursues a master thief.",
        )
        .await
        .unwrap();
    fx.library
        .set_artwork(
            &old_entry.entry_key,
            ArtworkKind::Poster,
            "movies/Old Folder/images/Heat.1995-tmdb-poster.jpg",
        )
        .await
        .unwrap();

    std::fs::create_dir_all(fx.root.join("movies/New Folder")).unwrap();
    std::fs::rename(fx.root.join(old_relative), fx.root.join(new_relative)).unwrap();
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!((report.added, report.removed), (1, 1));
    assert!(fx
        .library
        .get(&old_entry.entry_key)
        .await
        .unwrap()
        .is_none());

    let restored = fx.library.list().await.unwrap().into_iter().next().unwrap();
    assert_eq!(restored.relative_path, new_relative);
    assert_eq!(restored.scraped_title.as_deref(), Some("Heat"));
    assert_eq!(restored.genres, vec!["Crime", "Drama"]);
    assert_eq!(
        restored.overview.as_deref(),
        Some("A meticulous detective pursues a master thief.")
    );
    assert_eq!(restored.rating.as_deref(), Some("R"));
    assert_eq!(restored.community_rating, Some(8.2));
    assert_eq!(restored.community_rating_votes, Some(12_345));
    assert_eq!(
        fx.library
            .artwork(&restored.entry_key, ArtworkKind::Poster)
            .await
            .unwrap()
            .map(|(path, _)| path),
        Some("movies/New Folder/images/Heat.1995-tmdb-poster.jpg".into())
    );

    assert_eq!(
        fx.library
            .mark_missing_by_path(old_relative, 0)
            .await
            .unwrap(),
        Some(MissingDisposition::ConfirmedMissing),
        "a later successful scan can confirm the quarantined old path without deleting its history"
    );
}

#[tokio::test]
async fn returned_media_reactivates_the_same_row_with_metadata_intact() {
    let fx = fixture("metadata-survives-return").await;
    let relative = "movies/Heat.1995.mkv";
    let bytes = vec![9u8; 8_192];
    write(&fx.root, relative, &bytes);
    write(&fx.root, "movies/Still Here.2000.mkv", &[1u8; 8_192]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx
        .library
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.relative_path == relative)
        .unwrap();
    fx.library
        .set_scrape_result(
            &entry.entry_key,
            Some("Heat"),
            &["Crime".into()],
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    std::fs::remove_file(fx.root.join(relative)).unwrap();
    let missing = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(missing.removed, 1);
    assert_eq!(fx.library.list().await.unwrap().len(), 1);
    assert_eq!(fx.library.catalog_snapshot().await.unwrap().1.len(), 1);
    assert!(fx
        .library
        .pending_changes()
        .await
        .unwrap()
        .iter()
        .any(|change| change.entry_key == entry.entry_key && change.operation == "delete"));

    fx.library.clear_pending_changes().await.unwrap();
    write(&fx.root, relative, &bytes);
    let returned = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(returned.added, 1);
    let restored = fx
        .library
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.relative_path == relative)
        .unwrap();
    assert_eq!(restored.entry_key, entry.entry_key);
    assert_eq!(restored.scraped_title.as_deref(), Some("Heat"));
    assert_eq!(restored.genres, vec!["Crime"]);
    assert!(fx
        .library
        .pending_changes()
        .await
        .unwrap()
        .iter()
        .any(|change| change.entry_key == entry.entry_key && change.operation == "upsert"));
}

#[tokio::test]
async fn explicitly_cleared_metadata_does_not_restore_after_a_move() {
    let fx = fixture("cleared-metadata-stays-cleared").await;
    let old_relative = "movies/Old/Heat.1995.mkv";
    let new_relative = "movies/New/Heat.1995.mkv";
    write(&fx.root, old_relative, &[3u8; 8_192]);
    scan_root(&fx.library, &fx.root).await.unwrap();
    let entry = fx.library.list().await.unwrap().into_iter().next().unwrap();
    fx.library
        .set_scrape_result(
            &entry.entry_key,
            Some("Heat"),
            &["Crime".into()],
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    fx.library
        .clear_scrape_result(&entry.entry_key)
        .await
        .unwrap();

    std::fs::create_dir_all(fx.root.join("movies/New")).unwrap();
    std::fs::rename(fx.root.join(old_relative), fx.root.join(new_relative)).unwrap();
    scan_root(&fx.library, &fx.root).await.unwrap();
    let moved = fx.library.list().await.unwrap().into_iter().next().unwrap();
    assert_eq!(moved.relative_path, new_relative);
    assert_eq!(moved.scraped_title, None);
    assert!(moved.genres.is_empty());
}

/// `discover_media_files`'s directory walk now runs off the async runtime
/// entirely (`spawn_blocking`, bridged back via a bounded channel — see that
/// function's doc comment for the incident that motivated it) and flushes
/// to SQLite in fixed 256-entry batches. A library at or below 256 files
/// never exercises more than one batch or one channel round trip; this
/// proves the walk-and-flush loop is actually correct across a real batch
/// boundary, not just within a single one — every file found, none
/// duplicated, and a second scan of the same unchanged tree reports zero
/// spurious adds/removals (which a batching bug — e.g. losing or
/// double-sending a boundary batch — would show up as).
#[tokio::test]
async fn scan_finds_every_file_across_a_batch_boundary() {
    let fx = fixture("batch-boundary").await;
    const FILE_COUNT: usize = 300; // > the 256-entry batch size in scan.rs

    for i in 0..FILE_COUNT {
        // split_track_number (classify.rs) only recognizes a 1-3 digit
        // leading run as a track number — {i:03} keeps every value (0..300)
        // within that, {i:04} would silently parse as no track number at all.
        write(
            &fx.root,
            &format!("music/Artist/Album/{i:03} - Track.flac"),
            format!("fake flac bytes {i}").as_bytes(),
        );
    }

    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(report.added, FILE_COUNT as u64);
    assert_eq!(report.removed, 0);

    let entries = fx.library.list().await.unwrap();
    assert_eq!(entries.len(), FILE_COUNT);
    let mut track_numbers: Vec<_> = entries.iter().filter_map(|e| e.track_number).collect();
    track_numbers.sort_unstable();
    track_numbers.dedup();
    assert_eq!(
        track_numbers.len(),
        FILE_COUNT,
        "every track number 0..FILE_COUNT must appear exactly once — a lost or \
         duplicated batch at the 256-entry boundary would show up here as a gap or a dupe"
    );

    // Rescanning the same, unchanged tree must be a clean no-op — proves
    // the batched walk's output is stable/deterministic across runs, not
    // just correct once.
    let second = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(second.added, 0);
    assert_eq!(second.removed, 0);
    assert_eq!(second.unchanged, FILE_COUNT as u64);
}

/// #143: subtitle sidecars — both a `Subs/` folder and a plain
/// same-stem `.srt` beside the media — are discovered by the scan, matched
/// to their owning entry, and registered as toggleable `external` tracks.
#[tokio::test]
async fn scan_registers_subtitle_sidecars_from_subs_folder_and_beside_media() {
    let fx = fixture("subtitle-sidecars").await;

    // A movie with its subtitles gathered into a `Subs` folder (DVD-rip
    // convention) — one video in the folder, so the language file attaches
    // even though its name ("2_English") doesn't match the video stem.
    write(
        &fx.root,
        "Movies/Inception (2010)/Inception.2010.1080p.mkv",
        b"movie bytes",
    );
    write(
        &fx.root,
        "Movies/Inception (2010)/Subs/2_English.srt",
        b"1\n00:00:01,000 --> 00:00:02,000\nHello\n",
    );

    // An episode with a plain sidecar beside it, disambiguated among its
    // siblings by the SxxEyy marker in the subtitle filename.
    write(
        &fx.root,
        "Shows/The Expanse/Season 2/The.Expanse.S02E05.mkv",
        b"ep bytes",
    );
    write(
        &fx.root,
        "Shows/The Expanse/Season 2/The.Expanse.S02E06.mkv",
        b"ep bytes 6",
    );
    write(
        &fx.root,
        "Shows/The Expanse/Season 2/The.Expanse.S02E06.en.srt",
        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n",
    );

    scan_root(&fx.library, &fx.root).await.unwrap();

    let movie_key = entry_key("Movies/Inception (2010)/Inception.2010.1080p.mkv");
    let movie_subs = fx.library.subtitle_tracks(&movie_key).await.unwrap();
    assert_eq!(
        movie_subs.len(),
        1,
        "the Subs-folder file attaches to the movie"
    );
    assert_eq!(movie_subs[0].source, "external");
    assert_eq!(movie_subs[0].language, "en");
    assert_eq!(movie_subs[0].format, "srt");

    let e6_key = entry_key("Shows/The Expanse/Season 2/The.Expanse.S02E06.mkv");
    let e5_key = entry_key("Shows/The Expanse/Season 2/The.Expanse.S02E05.mkv");
    assert_eq!(
        fx.library.subtitle_tracks(&e6_key).await.unwrap().len(),
        1,
        "the SxxEyy marker pins the sidecar to episode 6"
    );
    assert!(
        fx.library
            .subtitle_tracks(&e5_key)
            .await
            .unwrap()
            .is_empty(),
        "episode 5 gets no subtitle"
    );

    // Removing the sidecar on disk clears the registered track on rescan.
    std::fs::remove_file(
        fx.root
            .join("Shows/The Expanse/Season 2/The.Expanse.S02E06.en.srt"),
    )
    .unwrap();
    scan_root(&fx.library, &fx.root).await.unwrap();
    assert!(
        fx.library
            .subtitle_tracks(&e6_key)
            .await
            .unwrap()
            .is_empty(),
        "a deleted sidecar is reconciled away"
    );
    assert_eq!(
        fx.library.subtitle_tracks(&movie_key).await.unwrap().len(),
        1,
        "an untouched sidecar survives the rescan"
    );
}
