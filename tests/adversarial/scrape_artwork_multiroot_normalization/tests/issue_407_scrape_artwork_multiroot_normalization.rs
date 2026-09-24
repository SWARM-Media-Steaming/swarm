//! Issue #407 boundary UAT: a root label is part of the stored path, but it
//! must not prevent an NFC catalog directory from finding its NFD SMB entry.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use swarm_core::peer::MediaKind;
use swarm_media::roots::{MediaRoot, MediaRootAssetType, RootResolver, SharedRootResolver};
use swarm_media::scrape::{scrape_one_album, ScrapeConfig};
use swarm_media::store::{ArtworkKind, EntryRecord, Library};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const NFC_CAFE: &str = "Caf\u{e9}";
const NFD_CAFE: &str = "Cafe\u{301}";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    music_root: PathBuf,
    library: Library,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

async fn fixture() -> Fixture {
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "swarm-adversarial-407-multiroot-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let music_root = base.join("music-root");
    std::fs::create_dir_all(&music_root).unwrap();
    let library = Library::open(base.join("library.sqlite").to_str().unwrap())
        .await
        .unwrap();
    Fixture {
        base,
        music_root,
        library,
    }
}

fn roots(music_root: &Path, base: &Path) -> SharedRootResolver {
    SharedRootResolver::new(RootResolver::new(vec![
        MediaRoot {
            label: "music".into(),
            path: music_root.to_path_buf(),
            asset_type: MediaRootAssetType::Music,
        },
        MediaRoot {
            label: "other".into(),
            path: base.join("other-root"),
            asset_type: MediaRootAssetType::Mixed,
        },
    ]))
}

fn track(relative_path: String) -> EntryRecord {
    EntryRecord {
        entry_key: "adversarial-407-multiroot".into(),
        relative_path,
        kind: MediaKind::Track,
        title: "Track".into(),
        size: 10,
        modified_time: 0,
        fingerprint: "adversarial-407-multiroot-fingerprint".into(),
        artist: Some("Artist".into()),
        album: Some("Album".into()),
        track_number: Some(1),
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

async fn no_match_musicbrainz() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"releases\":[]}")
            .await
            .unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn local_cover_import_preserves_the_labelled_catalog_path_across_normalization() {
    assert_ne!(NFC_CAFE, NFD_CAFE, "fixture requires distinct UTF-8 spellings");
    let fx = fixture().await;
    let physical_album = fx.music_root.join(format!("albums/{NFD_CAFE}"));
    std::fs::create_dir_all(&physical_album).unwrap();
    std::fs::write(physical_album.join("01.flac"), b"track").unwrap();
    std::fs::write(physical_album.join("folder.jpg"), b"physical-local-cover").unwrap();

    let catalog_track = format!("music/albums/{NFC_CAFE}/01.flac");
    let entry = track(catalog_track);
    fx.library.upsert(&entry).await.unwrap();

    let report = scrape_one_album(
        &fx.library,
        &roots(&fx.music_root, &fx.base),
        &ScrapeConfig {
            musicbrainz_base: Some(no_match_musicbrainz().await),
            ..Default::default()
        },
        &entry,
    )
    .await
    .unwrap();

    let expected_catalog_cover = format!("music/albums/{NFC_CAFE}/images/album-cover.jpg");
    assert_eq!(report.not_found, 1, "the local provider intentionally has no match");
    assert!(
        matches!(
            fx.library
                .artwork(&entry.entry_key, ArtworkKind::Cover)
                .await
                .unwrap(),
            Some((path, _)) if path == expected_catalog_cover
        ),
        "the artwork row must retain the labelled NFC catalog convention"
    );
    assert_eq!(
        std::fs::read(physical_album.join("images/album-cover.jpg")).unwrap(),
        b"physical-local-cover",
        "the bytes must be imported beside the directory physically exposed by the mount"
    );
}
