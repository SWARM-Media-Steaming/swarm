//! Plex migration compatibility (issue #247).
//!
//! The enforced principle: **if Plex can correctly scan and use an existing
//! library according to its current documented conventions, SWARM must also
//! scan and use that library without requiring any filesystem change.**
//!
//! Each test builds a representative Plex-valid layout on disk, scans it,
//! and asserts (a) the catalog SWARM produced matches what Plex would show,
//! (b) not one file was renamed, moved, or created, and (c) a clean Plex
//! library produces zero deterministic validation problems.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use swarm_core::entry_key::entry_key;
use swarm_core::peer::MediaKind;
use swarm_media::roots::MediaRoot;
use swarm_media::scan::{scan_root, scan_roots};
use swarm_media::store::{EntryRecord, Library};

struct Fixture {
    root: PathBuf,
    library: Library,
    _base: PathBuf,
}

async fn fixture(tag: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("swarm-plex-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("media");
    std::fs::create_dir_all(&root).unwrap();
    let library = Library::open(base.join("library.sqlite").to_str().unwrap())
        .await
        .unwrap();
    Fixture {
        root,
        library,
        _base: base,
    }
}

fn write(root: &Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Every path under `dir`, relative and forward-slashed — the fingerprint
/// used to prove a scan changed nothing on disk.
fn tree(dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path.clone());
            }
            out.insert(
                path.strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    out
}

fn find<'a>(entries: &'a [EntryRecord], relative: &str) -> &'a EntryRecord {
    let key = entry_key(relative);
    entries
        .iter()
        .find(|e| e.entry_key == key)
        .unwrap_or_else(|| panic!("no catalog entry for {relative}"))
}

#[tokio::test]
async fn a_representative_plex_movie_library_scans_unchanged() {
    let fx = fixture("movies").await;

    // 1. Movie in its own folder with the full local-asset set.
    write(
        &fx.root,
        "Movies/Blade Runner (1982)/Blade Runner (1982).mkv",
        b"v",
    );
    for asset in [
        "movie.nfo",
        "poster.jpg",
        "background.jpg",
        "logo.png",
        "Blade Runner (1982).en.srt",
    ] {
        write(
            &fx.root,
            &format!("Movies/Blade Runner (1982)/{asset}"),
            b"x",
        );
    }

    // 2. Loose movie in the library root (Plex accepts this).
    write(&fx.root, "Movies/Sintel (2010).mkv", b"v");

    // 3. Plex agent id + edition tokens in the name.
    write(
        &fx.root,
        "Movies/Dune (2021) {edition-IMAX Enhanced} {tmdb-438631}/Dune (2021) {edition-IMAX Enhanced} {tmdb-438631}.mkv",
        b"v",
    );

    // 4. Extras: a canonical extras folder AND a filename-suffix extra.
    write(
        &fx.root,
        "Movies/Big Buck Bunny (2008)/Big Buck Bunny (2008).mkv",
        b"v",
    );
    write(
        &fx.root,
        "Movies/Big Buck Bunny (2008)/Behind The Scenes/The Peach Open Movie.mkv",
        b"v",
    );
    write(
        &fx.root,
        "Movies/Big Buck Bunny (2008)/Big Buck Bunny (2008)-trailer.mkv",
        b"v",
    );

    let before = tree(&fx.root);
    let report = scan_roots(&fx.library, &[MediaRoot { label: "local".into(), path: fx.root.clone(), asset_type: Default::default() }], None)
        .await
        .unwrap();
    assert_eq!(tree(&fx.root), before, "the scan must not touch the filesystem");
    assert!(
        report.validation_issues.is_empty(),
        "a valid Plex movie library must produce no validation problems: {:#?}",
        report.validation_issues
    );

    let entries = fx.library.list().await.unwrap();
    // The sidecars (.nfo/.jpg/.png/.srt) never became catalog entries.
    assert!(entries.iter().all(|e| e.kind != MediaKind::Track));
    assert_eq!(entries.iter().filter(|e| e.kind == MediaKind::Movie).count(), 6);

    let blade = find(&entries, "Movies/Blade Runner (1982)/Blade Runner (1982).mkv");
    assert_eq!(blade.title, "Blade Runner");
    assert_eq!(blade.year, Some(1982));

    let dune = find(
        &entries,
        "Movies/Dune (2021) {edition-IMAX Enhanced} {tmdb-438631}/Dune (2021) {edition-IMAX Enhanced} {tmdb-438631}.mkv",
    );
    assert_eq!(dune.title, "Dune", "Plex tokens are stripped from the title");
    assert_eq!(dune.year, Some(2021));

    let trailer = find(
        &entries,
        "Movies/Big Buck Bunny (2008)/Big Buck Bunny (2008)-trailer.mkv",
    );
    assert_eq!(trailer.title, "Big Buck Bunny");

    // The subtitle beside the feature attached to it.
    let blade_subs = fx
        .library
        .subtitle_tracks(&entry_key(
            "Movies/Blade Runner (1982)/Blade Runner (1982).mkv",
        ))
        .await
        .unwrap();
    assert_eq!(blade_subs.len(), 1);
    assert_eq!(blade_subs[0].language, "en");
    assert_eq!(blade_subs[0].source, "external");
}

#[tokio::test]
async fn a_representative_plex_tv_library_scans_unchanged() {
    let fx = fixture("tv").await;

    // Show folder assets (ignored as sidecars).
    for asset in ["tvshow.nfo", "poster.jpg", "background.jpg", "theme.mp3"] {
        write(&fx.root, &format!("TV/Firefly (2002)/{asset}"), b"x");
    }
    // Season 01, ordinary + multi-episode file.
    write(
        &fx.root,
        "TV/Firefly (2002)/Season 01/Firefly (2002) - S01E01 - Serenity.mkv",
        b"v",
    );
    write(
        &fx.root,
        "TV/Firefly (2002)/Season 01/Firefly (2002) - S01E02-E03 - The Train Job.mkv",
        b"v",
    );
    // Specials (== Season 00) with a Plex extras folder inside it.
    write(
        &fx.root,
        "TV/Firefly (2002)/Specials/Firefly (2002) - S00E01 - Here's How It Was.mkv",
        b"v",
    );
    write(
        &fx.root,
        "TV/Firefly (2002)/Season 00/Deleted Scenes/Objects in Space.mkv",
        b"v",
    );
    // Subtitles: a /Subs folder for one episode, /Subtitles for another,
    // forced + SDH qualifiers, 3-letter language code.
    write(
        &fx.root,
        "TV/Firefly (2002)/Season 01/Subs/Firefly (2002) - S01E01 - Serenity.en.forced.srt",
        b"1\n00:00:01,000 --> 00:00:02,000\nx\n",
    );
    write(
        &fx.root,
        "TV/Firefly (2002)/Season 01/Subtitles/Firefly (2002) - S01E02-E03 - The Train Job.eng.sdh.srt",
        b"1\n00:00:01,000 --> 00:00:02,000\nx\n",
    );

    let before = tree(&fx.root);
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(tree(&fx.root), before, "the scan must not touch the filesystem");
    assert!(
        report.validation_issues.is_empty(),
        "a valid Plex TV library must produce no validation problems: {:#?}",
        report.validation_issues
    );

    let entries = fx.library.list().await.unwrap();
    let episodes: Vec<_> = entries
        .iter()
        .filter(|e| e.kind == MediaKind::Episode)
        .collect();
    assert_eq!(episodes.len(), 4);
    assert!(
        episodes
            .iter()
            .all(|e| e.show_title.as_deref() == Some("Firefly")),
        "every episode groups under the same show: {:#?}",
        episodes.iter().map(|e| &e.show_title).collect::<Vec<_>>()
    );

    let s1e1 = find(
        &entries,
        "TV/Firefly (2002)/Season 01/Firefly (2002) - S01E01 - Serenity.mkv",
    );
    assert_eq!((s1e1.season, s1e1.episode), (Some(1), Some(1)));

    let multi = find(
        &entries,
        "TV/Firefly (2002)/Season 01/Firefly (2002) - S01E02-E03 - The Train Job.mkv",
    );
    assert_eq!((multi.season, multi.episode), (Some(1), Some(2)));

    let special = find(
        &entries,
        "TV/Firefly (2002)/Specials/Firefly (2002) - S00E01 - Here's How It Was.mkv",
    );
    assert_eq!(special.season, Some(0));

    let deleted = find(
        &entries,
        "TV/Firefly (2002)/Season 00/Deleted Scenes/Objects in Space.mkv",
    );
    assert_eq!(deleted.season, Some(0));

    // Both subtitle folders resolved to the media beside them.
    let s1e1_subs = fx
        .library
        .subtitle_tracks(&s1e1.entry_key)
        .await
        .unwrap();
    assert_eq!(s1e1_subs.len(), 1, "the /Subs folder attached the forced track");
    assert_eq!(s1e1_subs[0].language, "en");
    assert!(s1e1_subs[0].label.to_lowercase().contains("forced"));

    let multi_subs = fx
        .library
        .subtitle_tracks(&multi.entry_key)
        .await
        .unwrap();
    assert_eq!(multi_subs.len(), 1, "the /Subtitles folder attached the SDH track");
    assert_eq!(multi_subs[0].language, "en");
    assert!(multi_subs[0].label.to_uppercase().contains("SDH"));
}

#[tokio::test]
async fn a_representative_plex_music_library_scans_unchanged() {
    let fx = fixture("music").await;

    write(
        &fx.root,
        "Music/Boards of Canada/Music Has the Right to Children/01 - Wildlife Analysis.flac",
        b"a",
    );
    write(
        &fx.root,
        "Music/Boards of Canada/Music Has the Right to Children/02 - An Eagle in Your Mind.flac",
        b"a",
    );
    for asset in ["cover.jpg", "artist.jpg", "01 - Wildlife Analysis.lrc"] {
        write(
            &fx.root,
            &format!("Music/Boards of Canada/Music Has the Right to Children/{asset}"),
            b"x",
        );
    }

    let before = tree(&fx.root);
    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert_eq!(tree(&fx.root), before);
    assert!(
        report.validation_issues.is_empty(),
        "music must not be judged by movie/TV rules: {:#?}",
        report.validation_issues
    );

    let entries = fx.library.list().await.unwrap();
    let tracks: Vec<_> = entries
        .iter()
        .filter(|e| e.kind == MediaKind::Track)
        .collect();
    assert_eq!(tracks.len(), 2);
    for t in &tracks {
        assert_eq!(t.artist.as_deref(), Some("Boards of Canada"));
        assert_eq!(t.album.as_deref(), Some("Music Has the Right to Children"));
    }
}

#[tokio::test]
async fn a_genuinely_malformed_file_is_reported_with_a_plex_compatible_fix() {
    let fx = fixture("malformed").await;
    // Media extension, but no title/marker/structure Plex could ever use.
    write(&fx.root, "Movies/________.mkv", b"v");
    // A real episode dumped loose with no Season folder — Plex would fail
    // to place it under a season.
    write(&fx.root, "TV/The Wire/The.Wire.S01E01.mkv", b"v");

    let report = scan_root(&fx.library, &fx.root).await.unwrap();
    assert!(
        report.validation_issues.len() >= 2,
        "both problems are reported: {:#?}",
        report.validation_issues
    );
    for issue in &report.validation_issues {
        assert!(!issue.problem.is_empty());
        assert!(!issue.expected.is_empty());
        assert!(!issue.recommended_fix.is_empty());
    }
    assert!(report
        .validation_issues
        .iter()
        .any(|i| i.current_path.ends_with("The.Wire.S01E01.mkv")
            && i.problem.contains("Season")));
}
