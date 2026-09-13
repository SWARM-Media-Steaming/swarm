use swarm_media::roots::MediaRoot;
use swarm_media::store::{ArtworkKind, SubtitleRecord};
use swarm_server::transcription::whisper_subtitle_path;
use swarm_server::{ServerConfig, ServerCore, TokenStoreMode};

#[tokio::test]
async fn deletion_removes_asset_companions_and_preserves_shared_artwork() {
    let base = std::env::temp_dir().join(format!(
        "swarm-delete-asset-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let media_root = base.join("media");
    let data_dir = base.join("data");
    let target_file = media_root.join("movies/Delete Me.mkv");
    let sibling_file = media_root.join("movies/Keep Me.mkv");
    std::fs::create_dir_all(target_file.parent().unwrap()).unwrap();
    std::fs::write(&target_file, vec![1u8; 8_192]).unwrap();
    std::fs::write(&sibling_file, vec![2u8; 8_192]).unwrap();

    let core = ServerCore::start(ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".into(),
            path: media_root.clone(),
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir: data_dir.clone(),
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    })
    .await
    .unwrap();
    core.wait_for_scan().await.unwrap();

    let entries = core.library.list().await.unwrap();
    let target = entries
        .iter()
        .find(|entry| entry.relative_path.ends_with("Delete Me.mkv"))
        .unwrap();
    let sibling = entries
        .iter()
        .find(|entry| entry.relative_path.ends_with("Keep Me.mkv"))
        .unwrap();

    let images = media_root.join("movies/images");
    std::fs::create_dir_all(&images).unwrap();
    let exclusive_art = images.join("delete-me-poster.jpg");
    let shared_art = images.join("shared-cover.jpg");
    std::fs::write(&exclusive_art, b"exclusive artwork").unwrap();
    std::fs::write(&shared_art, b"shared artwork").unwrap();
    core.library
        .set_artwork(
            &target.entry_key,
            ArtworkKind::Poster,
            "movies/images/delete-me-poster.jpg",
        )
        .await
        .unwrap();
    for entry_key in [&target.entry_key, &sibling.entry_key] {
        core.library
            .set_artwork(
                entry_key,
                ArtworkKind::Cover,
                "movies/images/shared-cover.jpg",
            )
            .await
            .unwrap();
    }

    let thumbnails = images.join(".swarm-thumbnails");
    std::fs::create_dir_all(&thumbnails).unwrap();
    let target_thumbnail = thumbnails.join(format!("{}-poster-v1-w320.jpg", target.entry_key));
    let sibling_thumbnail = thumbnails.join(format!("{}-cover-v1-w320.jpg", sibling.entry_key));
    std::fs::write(&target_thumbnail, b"target thumbnail").unwrap();
    std::fs::write(&sibling_thumbnail, b"sibling thumbnail").unwrap();

    let whisper = whisper_subtitle_path(&target_file);
    std::fs::write(&whisper, b"WEBVTT\n").unwrap();
    let downloaded_dir = data_dir.join("subtitles");
    std::fs::create_dir_all(&downloaded_dir).unwrap();
    let downloaded = downloaded_dir.join(format!("{}-opensubtitles-en.vtt", target.entry_key));
    std::fs::write(&downloaded, b"WEBVTT\n").unwrap();
    for (id, source, path) in [
        ("whisper-en", "whisper", &whisper),
        ("opensubtitles-en", "opensubtitles", &downloaded),
    ] {
        core.library
            .upsert_subtitle(&SubtitleRecord {
                id: id.into(),
                entry_key: target.entry_key.clone(),
                language: "en".into(),
                label: "English".into(),
                source: source.into(),
                format: "vtt".into(),
                file_path: path.to_string_lossy().into_owned(),
                fingerprint: target.fingerprint.clone(),
            })
            .await
            .unwrap();
    }
    core.library
        .set_like(&target.entry_key, "device", "Living room", true)
        .await
        .unwrap();

    let report = core.delete_asset(&target.entry_key).await.unwrap();
    assert!(report.cleanup_warnings.is_empty());
    assert_eq!(report.removed_files, 5);
    assert!(!target_file.exists());
    assert!(!exclusive_art.exists());
    assert!(!target_thumbnail.exists());
    assert!(!whisper.exists());
    assert!(!downloaded.exists());
    assert!(
        shared_art.exists(),
        "artwork referenced by a sibling must remain"
    );
    assert!(sibling_thumbnail.exists());
    assert!(core.library.get(&target.entry_key).await.unwrap().is_none());
    assert!(core
        .library
        .get(&sibling.entry_key)
        .await
        .unwrap()
        .is_some());
    assert!(!core
        .library
        .like_counts()
        .await
        .unwrap()
        .contains_key(&target.entry_key));

    let error = core.delete_asset(&target.entry_key).await.unwrap_err();
    assert!(matches!(error, swarm_server::ServerError::EntryNotFound));
    let _ = std::fs::remove_dir_all(base);
}

/// #143: a side-loaded subtitle sidecar gathered into a movie folder's
/// `Subs/` subfolder is registered as an `external` track by the scan, and
/// deleting the movie also removes that sidecar (with no "unrecognized
/// subtitle path" warning).
#[tokio::test]
async fn deletion_removes_a_side_loaded_subtitle_sidecar_from_a_subs_folder() {
    let base = std::env::temp_dir().join(format!(
        "swarm-delete-sidecar-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let media_root = base.join("media");
    let data_dir = base.join("data");
    let movie_dir = media_root.join("Movies/Heat (1995)");
    let movie_file = movie_dir.join("Heat (1995) 1080p.mkv");
    let kept_file = media_root.join("Movies/Casino (1995)/Casino (1995).mkv");
    let sidecar = movie_dir.join("Subs/3_English.srt");
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    std::fs::create_dir_all(kept_file.parent().unwrap()).unwrap();
    std::fs::write(&movie_file, vec![1u8; 8_192]).unwrap();
    std::fs::write(&kept_file, vec![2u8; 8_192]).unwrap();
    std::fs::write(
        &sidecar,
        b"1\n00:00:01,000 --> 00:00:02,000\nCome with me if you want to live\n",
    )
    .unwrap();

    let core = ServerCore::start(ServerConfig {
        media_roots: vec![MediaRoot {
            label: "local".into(),
            path: media_root.clone(),
            asset_type: Default::default(),
        }],
        scan_options: Default::default(),
        data_dir: data_dir.clone(),
        bind: "127.0.0.1:0".parse().unwrap(),
        http_media_bind: "127.0.0.1:0".parse().unwrap(),
        http_media_tls_bind: None,
        allowed_fingerprints: vec![],
        token_store_mode: TokenStoreMode::FileOnly,
        managed_rendezvous_url: None,
    })
    .await
    .unwrap();
    core.wait_for_scan().await.unwrap();

    let target = core
        .library
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.relative_path.ends_with("Heat (1995) 1080p.mkv"))
        .unwrap();
    let tracks = core
        .library
        .subtitle_tracks(&target.entry_key)
        .await
        .unwrap();
    assert_eq!(tracks.len(), 1, "the Subs-folder sidecar is registered");
    assert_eq!(tracks[0].source, "external");
    assert_eq!(tracks[0].format, "srt");

    let report = core.delete_asset(&target.entry_key).await.unwrap();
    assert!(
        report.cleanup_warnings.is_empty(),
        "a matched sidecar is a recognized companion: {:?}",
        report.cleanup_warnings
    );
    assert!(!movie_file.exists());
    assert!(!sidecar.exists(), "the side-loaded sidecar is removed too");
    assert!(kept_file.exists(), "an unrelated movie is untouched");

    let _ = std::fs::remove_dir_all(base);
}
