use std::sync::Arc;
use swarm_core::peer::{MediaKind, PeerRequest};
use swarm_media::roots::{RootResolver, SharedRootResolver};
use swarm_media::serve::{Body, MediaService};
use swarm_media::store::{ArtworkKind, EntryRecord, Library};
use swarm_media::transcode::TranscodeConfig;

fn request(path: &str) -> PeerRequest {
    PeerRequest {
        path: path.into(),
        range: None,
        if_none_match: None,
        playback: None,
        error_report: None,
        like: None,
    }
}

fn track_entry(entry_key: &str, relative_path: &str, artist: &str, album: &str) -> EntryRecord {
    EntryRecord {
        relative_path: relative_path.into(),
        kind: MediaKind::Track,
        title: "Example Track".into(),
        artist: Some(artist.into()),
        album: Some(album.into()),
        ..entry(entry_key)
    }
}

fn entry(entry_key: &str) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: "movies/example.mp4".into(),
        kind: MediaKind::Movie,
        title: "Example".into(),
        size: 1,
        modified_time: 0,
        fingerprint: "fingerprint".into(),
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
        parent_entry_key: None,
        extra_type: None,
        extra_title: None,
        extra_relative_path: None,
        extra_category_path: None,
    }
}

#[tokio::test]
async fn card_artwork_uses_a_persistent_versioned_thumbnail() {
    let root =
        std::env::temp_dir().join(format!("swarm-artwork-thumbnail-{}", rand::random::<u64>()));
    let media_root = root.join("media");
    let images = media_root.join("movies/images");
    std::fs::create_dir_all(&images).unwrap();
    let original = images.join("poster.png");
    image::RgbImage::from_pixel(1200, 1800, image::Rgb([25, 75, 125]))
        .save(&original)
        .unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let entry_key = "0123456789abcdef01234567";
    library.upsert(&entry(entry_key)).await.unwrap();
    library
        .set_artwork(entry_key, ArtworkKind::Poster, "movies/images/poster.png")
        .await
        .unwrap();
    let service = MediaService::new(library, media_root);

    let first = service
        .resolve(&request(&format!("/art/{entry_key}/poster?v=v1&w=320")))
        .await;
    assert_eq!(first.header.status, 200);
    assert_eq!(first.header.etag.as_deref(), Some("v1-w320"));
    assert_eq!(first.header.content_type.as_deref(), Some("image/jpeg"));
    let Body::File {
        path: first_path, ..
    } = first.body
    else {
        panic!("thumbnail should be file-backed")
    };
    let thumbnail = image::open(&first_path).unwrap();
    assert_eq!((thumbnail.width(), thumbnail.height()), (320, 480));

    let second = service
        .resolve(&request(&format!("/art/{entry_key}/poster?v=v1&w=320")))
        .await;
    let Body::File {
        path: second_path, ..
    } = second.body
    else {
        panic!("cached thumbnail should be file-backed")
    };
    assert_eq!(second_path, first_path);

    let full = service
        .resolve(&request(&format!("/art/{entry_key}/poster?v=v1")))
        .await;
    let Body::File {
        path: full_path, ..
    } = full.body
    else {
        panic!("full artwork should be file-backed")
    };
    assert_eq!(full_path, original);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn artwork_disk_cache_is_opt_in_read_through_and_version_invalidated() {
    let root = std::env::temp_dir().join(format!(
        "swarm-artwork-disk-cache-{}",
        rand::random::<u64>()
    ));
    let media_root = root.join("media");
    let images = media_root.join("movies/images");
    let cache_root = root.join("local-artwork-cache");
    std::fs::create_dir_all(&images).unwrap();
    let original = images.join("poster.png");
    image::RgbImage::from_pixel(4, 6, image::Rgb([10, 20, 30]))
        .save(&original)
        .unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let entry_key = "abcdef0123456789abcdef01";
    library.upsert(&entry(entry_key)).await.unwrap();
    library
        .set_artwork(entry_key, ArtworkKind::Poster, "movies/images/poster.png")
        .await
        .unwrap();
    let service = MediaService::with_roots_and_artwork_cache(
        library.clone(),
        SharedRootResolver::new(RootResolver::single(media_root)),
        TranscodeConfig::disabled(root.join("hls")),
        cache_root.clone(),
    );
    let art_request = request(&format!("/art/{entry_key}/poster"));

    let disabled = service.resolve(&art_request).await;
    let Body::File {
        path: disabled_path,
        ..
    } = disabled.body
    else {
        panic!("artwork should be file-backed")
    };
    assert_eq!(disabled_path, original, "cache must be off by default");
    assert!(!cache_root.exists());

    service.set_artwork_disk_cache_enabled(true);
    let first = service
        .resolve_for_client(&art_request, true, "Living Room TV")
        .await;
    assert_eq!(first.header.etag.as_deref(), Some("v1"));
    let Body::File {
        path: first_path, ..
    } = first.body
    else {
        panic!("cached artwork should be file-backed")
    };
    assert!(first_path.starts_with(&cache_root));
    assert_ne!(first_path, original);
    let first_bytes = std::fs::read(&first_path).unwrap();

    std::fs::remove_file(&original).unwrap();
    let cache_hit = service
        .resolve_for_client(&art_request, true, "Bedroom TV")
        .await;
    assert_eq!(cache_hit.header.status, 200);
    let Body::File { path: hit_path, .. } = cache_hit.body else {
        panic!("cache hit should be file-backed")
    };
    assert_eq!(hit_path, first_path);
    assert_eq!(std::fs::read(hit_path).unwrap(), first_bytes);
    let activity = service.artwork_cache_snapshot().await;
    assert!(activity.enabled);
    assert!(activity.disk_bytes > 0);
    assert!(activity.file_count > 0);
    assert_eq!(activity.events.len(), 2);
    assert_eq!(activity.events[0].client, "Living Room TV");
    assert_eq!(
        activity.events[0].kind,
        swarm_media::artwork_cache::ArtworkCacheEventKind::Cached
    );
    assert_eq!(activity.events[1].client, "Bedroom TV");
    assert_eq!(
        activity.events[1].kind,
        swarm_media::artwork_cache::ArtworkCacheEventKind::ServedFromCache
    );

    image::RgbImage::from_pixel(4, 6, image::Rgb([90, 80, 70]))
        .save(&original)
        .unwrap();
    let replacement_bytes = std::fs::read(&original).unwrap();
    library
        .set_artwork(entry_key, ArtworkKind::Poster, "movies/images/poster.png")
        .await
        .unwrap();
    let replaced = service.resolve(&art_request).await;
    assert_eq!(replaced.header.etag.as_deref(), Some("v2"));
    let Body::File {
        path: replaced_path,
        ..
    } = replaced.body
    else {
        panic!("replacement artwork should be file-backed")
    };
    assert_ne!(replaced_path, first_path);
    assert_eq!(std::fs::read(replaced_path).unwrap(), replacement_bytes);
    assert!(
        !first_path.exists(),
        "superseded cache file should be removed"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn artist_artwork_falls_back_to_the_first_album_cover_when_no_photo_was_scraped() {
    let root =
        std::env::temp_dir().join(format!("swarm-artist-fallback-{}", rand::random::<u64>()));
    let media_root = root.join("media");
    let images = media_root.join("music/images");
    std::fs::create_dir_all(&images).unwrap();
    let cover = images.join("album-cover.jpg");
    image::RgbImage::from_pixel(8, 8, image::Rgb([200, 100, 50]))
        .save(&cover)
        .unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let track_a = "aaaaaaaaaaaaaaaaaaaaaaaa";
    let track_b = "bbbbbbbbbbbbbbbbbbbbbbbb";
    // "A First Album" sorts before "Z Second Album", so the fallback must
    // pick the cover from the first one even though it's inserted second.
    library
        .upsert(&track_entry(
            track_b,
            "music/z-second-album/track.mp3",
            "Test Artist",
            "Z Second Album",
        ))
        .await
        .unwrap();
    library
        .upsert(&track_entry(
            track_a,
            "music/a-first-album/track.mp3",
            "Test Artist",
            "A First Album",
        ))
        .await
        .unwrap();
    library
        .set_artwork(track_a, ArtworkKind::Cover, "music/images/album-cover.jpg")
        .await
        .unwrap();
    let service = MediaService::new(library.clone(), media_root);

    // Neither track has its own artist photo, so a request against either
    // one's entry_key must resolve to the same fallback cover.
    let resolved = service
        .resolve(&request(&format!("/art/{track_b}/artist")))
        .await;
    assert_eq!(resolved.header.status, 200);
    let Body::File { path, .. } = resolved.body else {
        panic!("fallback artist artwork should be file-backed")
    };
    assert_eq!(path, cover);

    // A real artist photo, once scraped, always wins over the fallback.
    library
        .set_artwork(track_b, ArtworkKind::ArtistPhoto, "music/images/photo.jpg")
        .await
        .unwrap();
    std::fs::write(images.join("photo.jpg"), b"not a real image, just bytes").unwrap();
    let with_photo = service
        .resolve(&request(&format!("/art/{track_b}/artist")))
        .await;
    assert_eq!(with_photo.header.status, 200);
    let Body::File { path, .. } = with_photo.body else {
        panic!("scraped artist photo should be file-backed")
    };
    assert_eq!(path, images.join("photo.jpg"));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn artist_artwork_404s_when_the_artist_has_no_art_anywhere() {
    let root =
        std::env::temp_dir().join(format!("swarm-artist-no-fallback-{}", rand::random::<u64>()));
    let media_root = root.join("media");
    std::fs::create_dir_all(&media_root).unwrap();

    let library = Arc::new(
        Library::open(root.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap(),
    );
    let track_key = "cccccccccccccccccccccccc";
    library
        .upsert(&track_entry(
            track_key,
            "music/no-art/track.mp3",
            "Unadorned Artist",
            "Only Album",
        ))
        .await
        .unwrap();
    let service = MediaService::new(library.clone(), media_root);

    let resolved = service
        .resolve(&request(&format!("/art/{track_key}/artist")))
        .await;
    assert_eq!(resolved.header.status, 404);

    let _ = std::fs::remove_dir_all(root);
}
