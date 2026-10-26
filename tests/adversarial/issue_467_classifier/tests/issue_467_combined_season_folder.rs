use swarm_core::peer::MediaKind;
use swarm_media::classify::classify;

fn assert_episode(path: &str, show: &str, season: u32, episode: u32) {
    let entry = classify(path).unwrap_or_else(|| panic!("expected media classification for {path:?}"));
    assert_eq!(entry.kind, MediaKind::Episode, "kind for {path}");
    assert_eq!(entry.show_title.as_deref(), Some(show), "show for {path}");
    assert_eq!(entry.season, Some(season), "season for {path}");
    assert_eq!(entry.episode, Some(episode), "episode for {path}");
}

/// The directory is authoritative for show identity and season; filename's
/// episode marker remains authoritative for episode number.
#[test]
fn explicit_combined_season_folder_supplies_show_and_season() {
    assert_episode(
        "Tales from the Crypt - Season 6/Episode 3.mkv",
        "Tales from the Crypt",
        6,
        3,
    );
}

#[test]
fn combined_season_folder_handles_common_case_padding_and_episode_markers() {
    for (path, season, episode) in [
        ("Tales from the Crypt - Season 06/Episode 03.mkv", 6, 3),
        ("Tales from the Crypt Season 6/Episode 3.mkv", 6, 3),
        ("Tales from the Crypt - season 6/Episode 3.mkv", 6, 3),
        (
            "Tales from the Crypt - Season 6/Tales from the Crypt S06E03.mkv",
            6,
            3,
        ),
        ("Tales from the Crypt - Season 6/6x03.mkv", 6, 3),
    ] {
        assert_episode(path, "Tales from the Crypt", season, episode);
    }
}

#[test]
fn nested_episode_directory_does_not_hide_combined_season_ancestor() {
    assert_episode(
        "Library/Tales from the Crypt - Season 6/Disc 1/Episode 3.mkv",
        "Tales from the Crypt",
        6,
        3,
    );
}

#[test]
fn standalone_season_word_in_movie_title_is_not_a_season_folder() {
    let entry = classify("Movies/Season of the Witch (2011).mkv").unwrap();
    assert_eq!(entry.kind, MediaKind::Movie);
    assert_eq!(entry.title, "Season of the Witch");
    assert_eq!(entry.season, None);
}

#[test]
fn normal_show_titles_ending_in_digits_are_not_split_as_season_metadata() {
    let entry = classify("TV/Show 24/Episode 3.mkv").unwrap();
    assert_eq!(entry.kind, MediaKind::Episode);
    assert_eq!(entry.show_title.as_deref(), Some("Show 24"));
    assert_eq!(entry.season, Some(1));
    assert_eq!(entry.episode, Some(3));
}
