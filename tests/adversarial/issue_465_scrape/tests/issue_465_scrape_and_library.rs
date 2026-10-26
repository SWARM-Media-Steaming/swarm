use std::io::{Read, Write};
use std::net::TcpListener;
use swarm_media::scrape::tmdb::TmdbClient;
use swarm_core::peer::MediaKind;
use swarm_media::store::{corrected_episode_title, EntryRecord, Library};

const CRYPT_TMDB_ID: u64 = 2391;
const BUNDLE: &str = "Let the Punishment Fit the Crime / Only Skin Deep / Whirlpool";

/// Live TMDb shape (verified against themoviedb.org/tv/2391/season/6): 13
/// episodes where episode 1 bundles the three premiere episodes.
const TMDB_S06: [&str; 13] = [
    BUNDLE,
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

fn season_json(titles: &[&str]) -> String {
    let episodes: Vec<String> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            format!(
                r#"{{"episode_number":{},"name":{:?},"overview":"ov{}","still_path":"/still{}.jpg"}}"#,
                i + 1,
                t,
                i + 1,
                i + 1
            )
        })
        .collect();
    format!(r#"{{"poster_path":"/poster.jpg","episodes":[{}]}}"#, episodes.join(","))
}

/// Serves `body` to every request on an ephemeral port; returns the base URL.
fn mock_tmdb(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}")
}

async fn season(titles: &[&str], show_id: u64, season_number: u32) -> swarm_media::scrape::tmdb::ScrapedSeason {
    let base = mock_tmdb(season_json(titles));
    TmdbClient::with_base_urls("0123456789abcdef0123456789abcdef", base, "http://img")
        .season_details(show_id, season_number)
        .await
        .expect("season details")
}

#[tokio::test]
async fn scraper_returns_whirlpool_for_s06e03_and_keeps_later_episodes_aligned() {
    let s = season(&TMDB_S06, CRYPT_TMDB_ID, 6).await;
    let title = |n: u32| s.episode_details.get(&n).map(|e| e.title.as_str());
    assert_eq!(title(1), Some("Let the Punishment Fit the Crime"));
    assert_eq!(title(2), Some("Only Skin Deep"));
    assert_eq!(title(3), Some("Whirlpool"));
    assert_eq!(title(4), Some("Operation Friendship"));
    assert_eq!(title(5), Some("Revenge Is the Nuts"));
    assert_eq!(title(15), Some("You, Murderer"));
    assert_eq!(s.episode_details.len(), 15);
    assert_eq!(title(16), None);
    for (n, ep) in &s.episode_details {
        assert!(!ep.title.contains(" / Only") && !ep.title.is_empty(), "episode {n}");
    }
}

#[tokio::test]
async fn shifted_episodes_keep_their_own_overview_and_still() {
    let s = season(&TMDB_S06, CRYPT_TMDB_ID, 6).await;
    // TMDb ep 2 "Operation Friendship" (ov2/still2) is now episode 4, etc.
    for tmdb_n in 2..=13u32 {
        let n = tmdb_n + 2;
        let ep = s.episode_details.get(&n).unwrap();
        assert_eq!(ep.overview.as_deref(), Some(format!("ov{tmdb_n}").as_str()), "overview of ep {n}");
        let still = format!("http://img/w780/still{tmdb_n}.jpg");
        assert_eq!(ep.still_url.as_deref(), Some(still.as_str()), "still of ep {n}");
        assert_eq!(s.episode_still_urls.get(&n), Some(&still), "still map of ep {n}");
    }
    // The bundle's synopsis describes the three stories; it must not be
    // attached to the episodes it does not describe as if it were theirs
    // exclusively wrong: episode 3 (Whirlpool) must not carry Revenge's text.
    assert_ne!(s.episode_details.get(&3).unwrap().overview.as_deref(), Some("ov3"));
}

#[tokio::test]
async fn unbundled_slash_spacing_and_other_scopes_are_handled_conservatively() {
    // No spaces around slashes still splits.
    let compact = "Let the Punishment Fit the Crime/Only Skin Deep/Whirlpool";
    let mut titles = TMDB_S06;
    titles[0] = compact;
    let s = season(&titles, CRYPT_TMDB_ID, 6).await;
    assert_eq!(s.episode_details.get(&3).unwrap().title, "Whirlpool");

    // Same payload for a different show id or season must be left verbatim.
    let other_show = season(&TMDB_S06, 1396, 6).await;
    assert_eq!(other_show.episode_details.get(&1).unwrap().title, BUNDLE);
    assert_eq!(other_show.episode_details.get(&3).unwrap().title, "Revenge Is the Nuts");
    let other_season = season(&TMDB_S06, CRYPT_TMDB_ID, 5).await;
    assert_eq!(other_season.episode_details.get(&3).unwrap().title, "Revenge Is the Nuts");
}

#[tokio::test]
async fn already_correct_tmdb_data_is_not_shifted_twice() {
    let fixed: Vec<&str> = vec![
        "Let the Punishment Fit the Crime",
        "Only Skin Deep",
        "Whirlpool",
        "Operation Friendship",
        "Revenge Is the Nuts",
    ];
    let s = season(&fixed, CRYPT_TMDB_ID, 6).await;
    assert_eq!(s.episode_details.len(), 5);
    assert_eq!(s.episode_details.get(&3).unwrap().title, "Whirlpool");
    assert_eq!(s.episode_details.get(&5).unwrap().title, "Revenge Is the Nuts");
}

#[tokio::test]
async fn tmdb_bundle_with_only_the_first_episode_does_not_panic() {
    let s = season(&[BUNDLE], CRYPT_TMDB_ID, 6).await;
    assert_eq!(s.episode_details.len(), 3);
    assert_eq!(s.episode_details.get(&3).unwrap().title, "Whirlpool");
}

#[test]
fn show_title_variants_from_folder_names_are_corrected() {
    for show in [
        "Tales from the Crypt (1989)",
        "Tales From The Crypt",
        "TALES FROM THE CRYPT",
    ] {
        assert_eq!(
            corrected_episode_title(Some(show), Some(6), Some(3), Some("Revenge Is the Nuts")).as_deref(),
            Some("Whirlpool"),
            "{show}"
        );
    }
}

fn record(show: &str, season: u32, episode: u32, stored: &str) -> EntryRecord {
    EntryRecord {
        entry_key: format!("{show}-{season}-{episode}"),
        relative_path: format!("{show}/Season {season:02}/{show} S{season:02}E{episode:02}.mkv"),
        kind: MediaKind::Episode,
        title: format!("{show} S{season:02}E{episode:02}"),
        size: 1,
        modified_time: 0,
        fingerprint: format!("fp-{show}-{season}-{episode}"),
        artist: None,
        album: None,
        track_number: None,
        show_title: Some(show.to_string()),
        season: Some(season),
        episode: Some(episode),
        year: None,
        duration_secs: Some(1500.0),
        video: None,
        audio: None,
        scraped_title: Some(show.to_string()),
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

/// An already-scraped library persisted the wrong title. Every way the
/// server hands that row to a consumer (catalog for TV clients, and the
/// dashboard's `Library::list`) must show the real title, not only the
/// catalog conversion.
#[tokio::test]
async fn persisted_wrong_title_is_healed_in_catalog_and_list() {
    let dir = std::env::temp_dir().join(format!("swarm-adv-465-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lib.db");
    let library = Library::open(db.to_str().unwrap()).await.unwrap();
    library
        .upsert(&record("Tales from the Crypt", 6, 3, "Revenge Is the Nuts"))
        .await
        .unwrap();
    library
        .upsert(&record("Breaking Bad", 6, 3, "Revenge Is the Nuts"))
        .await
        .unwrap();

    for (key, title) in [
        ("Tales from the Crypt-6-3", "Revenge Is the Nuts"),
        ("Breaking Bad-6-3", "Revenge Is the Nuts"),
    ] {
        library.set_episode_title(key, Some(title)).await.unwrap();
    }

    let (_, catalog) = library.catalog_snapshot().await.unwrap();
    let crypt = catalog.iter().find(|e| e.show_title.as_deref() == Some("Tales from the Crypt")).unwrap();
    assert_eq!(crypt.episode_title.as_deref(), Some("Whirlpool"), "catalog");
    let other = catalog.iter().find(|e| e.show_title.as_deref() == Some("Breaking Bad")).unwrap();
    assert_eq!(other.episode_title.as_deref(), Some("Revenge Is the Nuts"));

    let listed = library.list().await.unwrap();
    let crypt = listed.iter().find(|e| e.show_title.as_deref() == Some("Tales from the Crypt")).unwrap();
    assert_eq!(
        crypt.episode_title.as_deref(),
        Some("Whirlpool"),
        "Library::list (dashboard Media tab) still exposes the stale stored title"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
