//! Issue #407 UAT: the normalization-aware fallback added for scrape artwork
//! existence and local-cover import must stay a *fallback*, not a guess.
//!
//! `RootResolver::resolve_existing`/`resolve_existing_dir` document that they
//! "cannot silently choose between two distinct files on a filesystem where
//! NFC and NFD names are genuinely different" and require "one unambiguous
//! match for every missing component". That invariant is a source-text
//! assertion in the sibling `test_issue_407_scrape_artwork_contract.py`
//! check, but nothing in this repository actually drives the ambiguous case
//! end to end. This UAT builds a genuine collision — two distinct on-disk
//! directory names that both fold to the same NFC form the catalog stores —
//! and asserts the scrape path refuses to guess between them rather than
//! silently serving (or importing into) whichever one a directory listing
//! happens to return first.
//!
//! It also exercises multi-component normalization mismatches (both the
//! album *and* its parent spelled differently on disk than in the catalog),
//! which the existing single-level fixtures never cover.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use swarm_core::peer::MediaKind;
use swarm_media::roots::{RootResolver, SharedRootResolver};
use swarm_media::scrape::{artwork, scrape_one_album, ScrapeConfig};
use swarm_media::store::{ArtworkKind, EntryRecord, Library};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use unicode_normalization::UnicodeNormalization;

/// Composed (fully NFC) catalog spelling: "a" + é (U+00E9) + ï (U+00EF).
const COMPOSED: &str = "a\u{e9}\u{ef}";
/// Decomposes only the first accent (é -> e + combining acute, U+0301);
/// leaves ï precomposed.
const PARTIAL_A: &str = "ae\u{301}\u{ef}";
/// Decomposes only the second accent (ï -> i + combining diaeresis,
/// U+0308); leaves é precomposed.
const PARTIAL_B: &str = "a\u{e9}i\u{308}";

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
        "swarm-adversarial-407-ambiguous-{tag}-{}-{number}",
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

/// Some filesystems (default macOS APFS among them) are themselves
/// normalization-insensitive: creating a second directory whose name only
/// differs by Unicode normalization silently collapses onto the first
/// (`create_dir_all` sees "already exists" and returns `Ok`), so there is
/// truly only one directory on disk and the resolver's own ambiguity guard
/// never gets exercised. This checks whether the fixture actually produced
/// two distinct directory entries, so the strict assertions below only run
/// where they can be meaningful (e.g. Linux ext4, or an SMB/NFS mount that
/// preserves the on-disk spelling — the exact scenario issue #407 is about).
fn filesystem_preserves_the_collision(parent: &Path) -> bool {
    std::fs::read_dir(parent).unwrap().count() == 2
}

/// Sanity-checks the fixture's own premise before trusting any assertion
/// built on it: two genuinely distinct byte strings that both fully
/// normalize to the same NFC form.
fn assert_fixture_is_a_genuine_collision() {
    assert_ne!(PARTIAL_A, PARTIAL_B, "the two on-disk spellings must be distinct byte strings");
    assert_ne!(PARTIAL_A, COMPOSED, "partial-A must not already be the exact catalog spelling");
    assert_ne!(PARTIAL_B, COMPOSED, "partial-B must not already be the exact catalog spelling");
    let nfc_a: String = PARTIAL_A.nfc().collect();
    let nfc_b: String = PARTIAL_B.nfc().collect();
    assert_eq!(nfc_a, COMPOSED, "partial-A must fully normalize to the catalog spelling");
    assert_eq!(nfc_b, COMPOSED, "partial-B must fully normalize to the catalog spelling");
}

#[tokio::test]
async fn artwork_exists_refuses_to_guess_between_two_ambiguous_directories() {
    assert_fixture_is_a_genuine_collision();
    let fx = fixture("exists").await;

    // Two distinct on-disk album directories that both fold to the same NFC
    // spelling the catalog will ask for. Only one carries a cover; if the
    // resolver picked arbitrarily instead of refusing, this cover could be
    // reported as present for a catalog path that, exactly, does not exist.
    let music_root = fx.root.join("music");
    std::fs::create_dir_all(&music_root).unwrap();
    let dir_a = music_root.join(PARTIAL_A);
    let dir_b = music_root.join(PARTIAL_B);
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    std::fs::write(dir_a.join("cover.jpg"), b"cover-in-a").unwrap();

    if !filesystem_preserves_the_collision(&music_root) {
        eprintln!(
            "SKIP: host filesystem is normalization-insensitive and collapsed the two candidate directories into one; the ambiguity this UAT targets cannot be constructed here (it is reachable on Linux ext4 or on the SMB/NFS mounts issue #407 describes)"
        );
        return;
    }

    let roots = resolver(&fx.root);
    let catalog_cover = format!("music/{COMPOSED}/cover.jpg");

    assert!(
        !artwork::exists(&roots, &catalog_cover).await,
        "an ambiguous NFC collision between two real directories must never be reported as an existing file"
    );
}

#[tokio::test]
async fn scrape_does_not_import_a_local_cover_through_an_ambiguous_album_directory() {
    assert_fixture_is_a_genuine_collision();
    let fx = fixture("import").await;

    let music_root = fx.root.join("music");
    std::fs::create_dir_all(&music_root).unwrap();
    let dir_a = music_root.join(PARTIAL_A);
    let dir_b = music_root.join(PARTIAL_B);
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    std::fs::write(dir_a.join("track.flac"), b"track").unwrap();
    std::fs::write(dir_a.join("folder.jpg"), b"local-front-cover").unwrap();

    if !filesystem_preserves_the_collision(&music_root) {
        eprintln!(
            "SKIP: host filesystem is normalization-insensitive and collapsed the two candidate directories into one; the ambiguity this UAT targets cannot be constructed here (it is reachable on Linux ext4 or on the SMB/NFS mounts issue #407 describes)"
        );
        return;
    }

    let entry_key = "adversarial-407-ambiguous-import";
    let catalog_track = format!("music/{COMPOSED}/track.flac");
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

    assert_eq!(report.not_found, 1, "the fixture's local provider has no release");
    assert!(
        fx.library
            .artwork(entry_key, ArtworkKind::Cover)
            .await
            .unwrap()
            .is_none(),
        "an ambiguous album directory must not let local-cover import guess which real folder to enumerate"
    );
    assert!(
        !dir_a.join("images/album-cover.jpg").exists(),
        "no import should be written into either candidate directory when the match is ambiguous"
    );
    assert!(
        !dir_b.join("images/album-cover.jpg").exists(),
        "no import should be written into either candidate directory when the match is ambiguous"
    );
}

#[tokio::test]
async fn resolve_existing_walks_multiple_mismatched_path_components() {
    let fx = fixture("nested").await;

    const NFD_ARTIST: &str = "Cafe\u{301} Trio";
    const NFC_ARTIST: &str = "Caf\u{e9} Trio";
    // "Été 1998" fully decomposed: E+combining-acute, t, e+combining-acute.
    const NFD_ALBUM: &str = "E\u{301}te\u{301} 1998";
    // "Été 1998" fully composed: É (U+00C9), t, é (U+00E9).
    const NFC_ALBUM_COMPOSED: &str = "\u{c9}t\u{e9} 1998";

    let actual_dir = fx.root.join("music").join(NFD_ARTIST).join(NFD_ALBUM);
    std::fs::create_dir_all(&actual_dir).unwrap();
    std::fs::write(actual_dir.join("track.flac"), b"track").unwrap();
    std::fs::write(actual_dir.join("cover.jpg"), b"nested-cover").unwrap();

    let entry_key = "adversarial-407-nested";
    let catalog_track = format!("music/{NFC_ARTIST}/{NFC_ALBUM_COMPOSED}/track.flac");
    let catalog_cover = format!("music/{NFC_ARTIST}/{NFC_ALBUM_COMPOSED}/cover.jpg");
    let entry = track(entry_key, &catalog_track);
    fx.library.upsert(&entry).await.unwrap();
    fx.library
        .set_artwork(entry_key, ArtworkKind::Cover, &catalog_cover)
        .await
        .unwrap();

    let roots = resolver(&fx.root);
    let nfd_album_nfc: String = NFD_ALBUM.nfc().collect();
    assert_eq!(
        nfd_album_nfc, NFC_ALBUM_COMPOSED,
        "fixture album spelling must be a genuine NFD/NFC pair of the same text"
    );
    assert!(
        artwork::exists(&roots, &catalog_cover).await,
        "a cover reachable only by normalizing two nested mismatched path components must still be found"
    );
}
