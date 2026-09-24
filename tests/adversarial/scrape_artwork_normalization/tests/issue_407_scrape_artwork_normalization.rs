//! Issue #407 UAT: catalog artwork references must survive NFC/NFD spelling
//! differences exposed by network filesystems during non-force music scrapes.
//!
//! The expected behavior comes from the scrape contract, rather than the
//! implementation: a present cover prevents a non-force replacement, and a
//! conventional local cover beside an album is imported before provider
//! lookup. A catalog row may spell an SMB-listed directory differently, but
//! that must not change either outcome. Exact spellings remain preferred;
//! this fixture only models the one unambiguous equivalent directory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use swarm_core::peer::MediaKind;
use swarm_media::roots::{RootResolver, SharedRootResolver};
use swarm_media::scrape::{artwork, scrape_one_album, ScrapeConfig};
use swarm_media::store::{ArtworkKind, EntryRecord, Library};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const NFC_CAFE: &str = "Caf\u{e9}";
const NFD_CAFE: &str = "Cafe\u{301}";

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    library: Library,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

async fn fixture(tag: &str) -> Fixture {
    let number = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "swarm-adversarial-407-{tag}-{}-{number}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("media");
    std::fs::create_dir_all(&root).unwrap();
    let library = Library::open(base.join("library.sqlite").to_str().unwrap())
        .await
        .unwrap();
    Fixture {
        base,
        root,
        library,
    }
}

fn resolver(root: &Path) -> SharedRootResolver {
    SharedRootResolver::new(RootResolver::single(root.to_path_buf()))
}

fn track(entry_key: &str, relative_path: &str) -> EntryRecord {
    EntryRecord {
        entry_key: entry_key.into(),
        relative_path: relative_path.into(),
        kind: MediaKind::Track,
        title: "Track".into(),
        size: 10,
        modified_time: 0,
        fingerprint: format!("fingerprint-{entry_key}"),
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

/// The scrape request receives a real local HTTP response, never an external
/// service. No match makes provider artwork impossible, isolating local-cover
/// behavior while still exercising the public scrape flow.
async fn no_match_musicbrainz() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"releases\":[]}",
            )
            .await
            .unwrap();
    });
    format!("http://{address}")
}

fn normalization_sensitive(root: &Path) -> bool {
    let probe = root.join("normalization-probe");
    std::fs::create_dir_all(probe.join(NFD_CAFE)).unwrap();
    !probe.join(NFC_CAFE).is_dir()
}

#[tokio::test]
async fn non_force_scrape_retains_cover_at_an_unambiguously_normalized_path() {
    let fx = fixture("existing-cover").await;
    let actual_album = fx.root.join(format!("music/{NFD_CAFE}"));
    std::fs::create_dir_all(&actual_album).unwrap();
    std::fs::write(actual_album.join("track.flac"), b"track").unwrap();
    std::fs::write(actual_album.join("cover.jpg"), b"existing-cover").unwrap();
    // A competing local candidate makes an erroneous re-import observable.
    std::fs::write(actual_album.join("folder.jpg"), b"must-not-replace-cover").unwrap();

    let entry_key = "adversarial-407-existing";
    let catalog_track = format!("music/{NFC_CAFE}/track.flac");
    let catalog_cover = format!("music/{NFC_CAFE}/cover.jpg");
    let entry = track(entry_key, &catalog_track);
    fx.library.upsert(&entry).await.unwrap();
    fx.library
        .set_artwork(entry_key, ArtworkKind::Cover, &catalog_cover)
        .await
        .unwrap();

    let roots = resolver(&fx.root);
    assert!(
        artwork::exists(&roots, &catalog_cover).await,
        "an existing unambiguous NFC/NFD-equivalent cover is not missing artwork"
    );
    let config = ScrapeConfig {
        musicbrainz_base: Some(no_match_musicbrainz().await),
        ..Default::default()
    };
    let report = scrape_one_album(&fx.library, &roots, &config, &entry)
        .await
        .unwrap();

    assert_eq!(
        report.not_found, 1,
        "the fixture's local provider has no release"
    );
    assert!(
        matches!(
            fx.library.artwork(entry_key, ArtworkKind::Cover).await.unwrap(),
            Some((path, _)) if path == catalog_cover
        ),
        "a non-force scrape must retain a cover that is already present on disk"
    );
    assert_eq!(
        std::fs::read(actual_album.join("cover.jpg")).unwrap(),
        b"existing-cover"
    );
    assert!(
        !actual_album.join("images/album-cover.jpg").exists(),
        "an existing cover must suppress local-cover import rather than creating a replacement"
    );
    if normalization_sensitive(&fx.root) {
        assert!(
            !roots.resolve(&format!("music/{NFC_CAFE}/cover.jpg")).is_file(),
            "fixture must be a true raw-join miss when the filesystem preserves NFC/NFD distinctions"
        );
    }
}

#[tokio::test]
async fn scrape_imports_local_cover_beside_a_normalized_album_directory() {
    let fx = fixture("local-import").await;
    let actual_album = fx.root.join(format!("music/{NFD_CAFE}"));
    std::fs::create_dir_all(&actual_album).unwrap();
    std::fs::write(actual_album.join("track.flac"), b"track").unwrap();
    std::fs::write(actual_album.join("folder.jpg"), b"local-front-cover").unwrap();

    let entry_key = "adversarial-407-import";
    let catalog_track = format!("music/{NFC_CAFE}/track.flac");
    let entry = track(entry_key, &catalog_track);
    fx.library.upsert(&entry).await.unwrap();

    let roots = resolver(&fx.root);
    let config = ScrapeConfig {
        musicbrainz_base: Some(no_match_musicbrainz().await),
        ..Default::default()
    };
    let report = scrape_one_album(&fx.library, &roots, &config, &entry)
        .await
        .unwrap();

    let expected = format!("music/{NFC_CAFE}/images/album-cover.jpg");
    assert_eq!(
        report.not_found, 1,
        "the fixture's local provider has no release"
    );
    assert!(
        matches!(
            fx.library.artwork(entry_key, ArtworkKind::Cover).await.unwrap(),
            Some((path, _)) if path == expected
        ),
        "the imported cover path must retain the catalog root/path convention"
    );
    assert_eq!(
        std::fs::read(actual_album.join("images/album-cover.jpg")).unwrap(),
        b"local-front-cover",
        "the selected local cover must be written beside the physically listed album directory"
    );
    if normalization_sensitive(&fx.root) {
        assert!(
            !roots.resolve(&format!("music/{NFC_CAFE}")).is_dir(),
            "fixture must be a true raw-join directory miss when the filesystem preserves NFC/NFD distinctions"
        );
    }
}
