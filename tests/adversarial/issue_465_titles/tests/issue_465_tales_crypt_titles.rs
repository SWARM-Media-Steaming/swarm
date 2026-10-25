use swarm_media::store::corrected_episode_title;

const CANONICAL_S06: [&str; 15] = [
    "Let the Punishment Fit the Crime",
    "Only Skin Deep",
    "Whirlpool",
    "Operation Friendship",
    "Revenge Is the Nuts",
    "The Bribe",
    "The Pit",
    "The Assassin",
    "Staired in Horror",
    "In the Groove",
    "Surprise Party",
    "Doctor of Horror",
    "Comes the Dawn",
    "99 & 44/100% Pure Horror",
    "You, Murderer",
];

#[test]
fn corrected_existing_library_titles_match_real_season_six_episode_order() {
    // A persisted row may contain any title from the incorrectly shifted TMDb
    // season. Its display title must follow the real episode-number ordering.
    for (index, expected) in CANONICAL_S06.iter().enumerate() {
        let episode = (index + 1) as u32;
        let stale_tmdb_title = if episode == 3 {
            "Revenge Is the Nuts"
        } else if episode == 5 {
            "The Pit"
        } else {
            "stale title from prior scrape"
        };
        let actual = corrected_episode_title(
            Some("Tales from the Crypt"),
            Some(6),
            Some(episode),
            Some(stale_tmdb_title),
        );
        assert_eq!(actual.as_deref(), Some(*expected), "S06E{episode:02}");
    }
}

#[test]
fn slash_bundled_premiere_maps_each_episode_number_to_its_own_title() {
    let bundled = "Let the Punishment Fit the Crime / Only Skin Deep / Whirlpool";
    for episode in 1..=3 {
        assert_eq!(
            corrected_episode_title(Some("Tales from the Crypt"), Some(6), Some(episode), Some(bundled)).as_deref(),
            Some(CANONICAL_S06[(episode - 1) as usize]),
            "bundled title at S06E{episode:02}"
        );
    }
}

#[test]
fn correction_is_limited_to_target_show_season_and_valid_episode_numbers() {
    let cases = [
        (Some("Breaking Bad"), Some(6), Some(3), "Revenge Is the Nuts"),
        (Some("Tales from the Crypt"), Some(5), Some(3), "Revenge Is the Nuts"),
        (Some("Tales from the Crypt"), Some(6), Some(0), "Revenge Is the Nuts"),
        (Some("Tales from the Crypt"), Some(6), Some(16), "Revenge Is the Nuts"),
    ];
    for (show, season, episode, stored) in cases {
        assert_eq!(
            corrected_episode_title(show, season, episode, Some(stored)).as_deref(),
            Some(stored),
            "unexpected correction for {show:?} S{season:?}E{episode:?}"
        );
    }
}

#[test]
fn missing_or_empty_stored_titles_remain_missing() {
    assert_eq!(corrected_episode_title(Some("Tales from the Crypt"), Some(6), Some(3), None), None);
    assert_eq!(corrected_episode_title(Some("Tales from the Crypt"), Some(6), Some(3), Some("")), None);
}
