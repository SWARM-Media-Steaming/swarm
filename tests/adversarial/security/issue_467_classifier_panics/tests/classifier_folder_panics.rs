//! Security regression tests for issue #467's `parse_show_season_word_folder`.
//!
//! The classifier runs inline on every relative path the library scan walks,
//! including paths whose folder names come from whoever can place files in a
//! scanned root (a shared NAS, a download folder). A folder name that makes the
//! classifier panic aborts the scan of that whole root, so each malicious shape
//! below must classify without panicking.

use swarm_core::peer::MediaKind;
use swarm_media::classify::classify;

#[test]
fn multibyte_folder_name_does_not_panic_the_classifier() {
    // `é` is two UTF-8 bytes; the 6-byte window the parser takes before the
    // trailing digits starts inside that character, which is not a boundary.
    let entry = classify("éabcde 6/Episode 3.mkv")
        .expect("a folder name with a multibyte character must still classify");
    assert_eq!(entry.kind, MediaKind::Episode);
    assert_eq!(entry.episode, Some(3));
}

#[test]
fn multibyte_folder_name_under_a_movie_path_does_not_panic_the_classifier() {
    let entry = classify("Movies/éabcde 6/Movie.mkv")
        .expect("a movie under a multibyte-named folder must still classify");
    assert_eq!(entry.kind, MediaKind::Movie);
}

#[test]
fn case_folding_that_grows_the_name_does_not_panic_the_classifier() {
    // `İ` (2 bytes) lowercases to 3 bytes, so the lowercased copy of the
    // folder name is longer than the original. The `Specials` branch uses the
    // lowercased index against the original bytes and runs past the end.
    let folder = format!("{} Specials", "İ".repeat(20));
    let path = format!("{folder}/Episode 3.mkv");
    let entry = classify(&path).expect("a case-folding-growing folder name must still classify");
    assert_eq!(entry.kind, MediaKind::Episode);
    assert_eq!(entry.episode, Some(3));
}
