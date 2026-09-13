//! Issue #73: renaming files inside an already-scanned show/movie folder
//! left stale catalog rows `available` (and therefore still served to
//! clients) until the next scheduled/manual rescan, up to
//! `AUTO_LIBRARY_WATCH_INTERVAL` (15 minutes) later. A client hitting
//! `/media/*` or `/play/*` for a since-renamed entry is the server's
//! earliest possible signal that the row is stale — it should flip the row
//! unavailable right then, the same way a real scan's first miss already
//! does, instead of leaving every following request to fail identically
//! until a rescan happens to run.

use std::sync::Arc;
use swarm_core::peer::{MediaKind, PeerRequest};
use swarm_media::serve::MediaService;
use swarm_media::store::{EntryRecord, Library};

fn media_request(entry_key: &str) -> PeerRequest {
    PeerRequest {
        path: format!("/media/{entry_key}"),
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

fn sample_entry(entry_key: &str, relative_path: &str) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Episode,
        title: "Example".into(),
        size: 4,
        modified_time: 0,
        fingerprint: "fingerprint".into(),
        artist: None,
        album: None,
        track_number: None,
        show_title: Some("Dragon Ball GT".into()),
        season: Some(1),
        episode: Some(1),
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
        parent_entry_key: None,
        extra_type: None,
        extra_title: None,
        extra_relative_path: None,
        extra_category_path: None,
    }
}

#[tokio::test]
async fn a_stream_request_for_a_file_renamed_since_the_last_scan_marks_the_row_unavailable() {
    let root = std::env::temp_dir().join(format!(
        "swarm-missing-file-self-heal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let media_root = root.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let entry_key = "0123456789abcdef01234567";
    let relative_path = "Dragon Ball GT/Dragon Ball GT - 001.mkv";
    let media_path = media_root.join(relative_path);
    std::fs::create_dir_all(media_path.parent().unwrap()).unwrap();
    std::fs::write(&media_path, b"fake").unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    library
        .upsert(&sample_entry(entry_key, relative_path))
        .await
        .unwrap();
    assert!(
        library.get(entry_key).await.unwrap().is_some(),
        "entry must be available before the rename"
    );

    let service = MediaService::new(library.clone(), media_root);

    // Simulate the user renaming the episode file, the way updating a show
    // folder's naming scheme does in practice.
    std::fs::remove_file(&media_path).unwrap();

    let resolved = service.resolve(&media_request(entry_key)).await;
    assert_eq!(resolved.header.status, 404);

    // The whole point: the catalog must reflect the miss immediately,
    // without waiting for a rescan, so the very next catalog fetch already
    // stops offering clients the dead entry.
    assert!(
        library.get(entry_key).await.unwrap().is_none(),
        "stale entry must be marked unavailable as soon as streaming discovers it is gone"
    );
}
