use swarm_core::peer::MediaKind;
use swarm_media::store::{corrected_episode_title, EntryRecord, Library};

/// Issue #465 says S06E03 of *Tales from the Crypt* must read "Whirlpool".
/// The correction is keyed on a show-name substring, so it must only ever
/// touch that one show. Other shows whose names merely contain the phrase
/// (e.g. the animated "Tales from the Cryptkeeper") must keep their titles.
#[test]
fn cryptkeeper_spinoff_season_six_titles_are_not_rewritten() {
    for show in ["Tales from the Cryptkeeper", "Tales From The Cryptkeeper"] {
        for episode in [3, 5] {
            assert_eq!(
                corrected_episode_title(
                    Some(show),
                    Some(6),
                    Some(episode),
                    Some("Revenge Is the Nuts"),
                )
                .as_deref(),
                Some("Revenge Is the Nuts"),
                "{show} S06E{episode:02} must not be rewritten by the Tales from the Crypt correction"
            );
        }
    }
}

fn record(show: Option<&str>, season: u32, episode: u32, stored: &str) -> EntryRecord {
    let label = show.unwrap_or("untitled");
    EntryRecord {
        entry_key: format!("{label}-{season}-{episode}"),
        relative_path: format!("{label}/Season {season:02}/{label} S{season:02}E{episode:02}.mkv"),
        kind: MediaKind::Episode,
        title: format!("{label} S{season:02}E{episode:02}"),
        size: 1,
        modified_time: 0,
        fingerprint: format!("fp-{label}-{season}-{episode}"),
        artist: None,
        album: None,
        track_number: None,
        show_title: show.map(str::to_string),
        season: Some(season),
        episode: Some(episode),
        year: None,
        duration_secs: Some(1500.0),
        video: None,
        audio: None,
        scraped_title: show.map(str::to_string),
        episode_title: Some(stored.to_string()),
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

/// The library read path must not rewrite a spin-off's persisted title either:
/// the dashboard list is the surface a user would see the wrong name on.
#[tokio::test]
async fn cryptkeeper_spinoff_library_row_keeps_its_stored_title() {
    let dir = std::env::temp_dir().join(format!("swarm-adv-465-scope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let library = Library::open(dir.join("lib.db").to_str().unwrap()).await.unwrap();
    library
        .upsert(&record(Some("Tales from the Cryptkeeper"), 6, 3, "Revenge Is the Nuts"))
        .await
        .unwrap();
    library
        .set_episode_title("Tales from the Cryptkeeper-6-3", Some("Revenge Is the Nuts"))
        .await
        .unwrap();

    let listed = library.list().await.unwrap();
    let row = listed
        .iter()
        .find(|e| e.show_title.as_deref() == Some("Tales from the Cryptkeeper"))
        .expect("spin-off row listed");
    assert_eq!(
        row.episode_title.as_deref(),
        Some("Revenge Is the Nuts"),
        "Library::list rewrote a Tales from the Cryptkeeper title"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A row with no show title (unscraped or folder-only) must pass through
/// unchanged and must not panic on the show-name lookup.
#[test]
fn missing_show_title_passes_stored_title_through() {
    assert_eq!(
        corrected_episode_title(None, Some(6), Some(3), Some("Revenge Is the Nuts")).as_deref(),
        Some("Revenge Is the Nuts")
    );
}

/// Show-name matching is case- and surrounding-whitespace-insensitive for the
/// real show, so a folder like "  TALES FROM THE CRYPT " still heals.
#[test]
fn real_show_name_with_padding_and_case_still_heals() {
    assert_eq!(
        corrected_episode_title(Some("  TALES FROM THE CRYPT "), Some(6), Some(3), Some("Revenge Is the Nuts"))
            .as_deref(),
        Some("Whirlpool")
    );
}
