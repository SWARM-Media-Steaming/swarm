use std::io::{Read, Write};
use std::net::TcpListener;
use swarm_core::peer::MediaKind;
use swarm_media::classify::classify;
use swarm_media::scrape::tmdb::TmdbClient;
use swarm_media::store::{corrected_episode_title, EntryRecord, Library};

/// Real season 6 running order (Wikipedia / TVmaze / IMDb agree on the order;
/// TMDb's numbering differs after E11, so the order is the invariant).
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

const BUNDLE: &str = "Let the Punishment Fit the Crime / Only Skin Deep / Whirlpool";
const CRYPT_TMDB_ID: u64 = 2391;

/// What TMDb served before the correction: episode 1 bundles three premieres,
/// so every later title sits two positions early. Persisted rows from scrapes
/// made before the fix hold exactly these strings.
const OLD_TMDB_S06: [&str; 13] = [
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

/// Real classifier output for realistic Crypt folder/file names must carry a
/// show key the correction recognises, so an S06E03 file reads "Whirlpool".
#[test]
fn classifier_output_for_real_s06e03_filenames_heals_to_whirlpool() {
    let paths = [
        "Tales from the Crypt (1989)/Season 06/Tales from the Crypt (1989) - S06E03 - Revenge Is the Nuts.mkv",
        "Tales.from.the.Crypt.S06E03.1080p.BluRay.mkv",
        "Tales from the Crypt/Season 6/Tales from the Crypt S06E03.mkv",
        "Tales from the Crypt/Season 06/S06E03 - Revenge Is the Nuts.mkv",
        "Tales From The Crypt S6E03.mkv",
        "Tales from the Crypt/Season 06/Tales from the Crypt - 6x03 - Revenge Is the Nuts.mkv",
        "Tales from the Crypt S06 Disc 1/Tales from the Crypt S06E03.mkv",
        "TALES FROM THE CRYPT/S06/S06E03 Revenge.mkv",
        "Tales.from.the.Crypt.1989.S06E03.mkv",
    ];
    for path in paths {
        let c = classify(path).unwrap_or_else(|| panic!("unclassified: {path}"));
        assert_eq!(c.season, Some(6), "season for {path}");
        assert_eq!(c.episode, Some(3), "episode for {path}");
        let show = c.show_title.as_deref();
        assert_eq!(
            corrected_episode_title(show, c.season, c.episode, Some("Revenge Is the Nuts")).as_deref(),
            Some("Whirlpool"),
            "classified show {show:?} from {path}"
        );
    }
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

/// Every season 6 position the scraper returns must be the real episode, not
/// only E1-E5 and E15 as the existing UAT checks.
#[tokio::test]
async fn scraper_titles_every_season_six_position_in_real_order() {
    let base = mock_tmdb(season_json(&OLD_TMDB_S06));
    let s = TmdbClient::with_base_urls("0123456789abcdef0123456789abcdef", base, "http://img")
        .season_details(CRYPT_TMDB_ID, 6)
        .await
        .expect("season details");
    assert_eq!(s.episode_details.len(), 15);
    for (index, expected) in CANONICAL_S06.iter().enumerate() {
        let n = (index + 1) as u32;
        assert_eq!(
            s.episode_details.get(&n).map(|e| e.title.as_str()),
            Some(*expected),
            "scraped S06E{n:02}"
        );
    }
    assert!(s.episode_details.get(&16).is_none());
}

/// A library scraped before the fix holds the shifted TMDb title at every
/// position. Reading it back must show the real title at every position,
/// through the same Library read path the dashboard uses.
#[tokio::test]
async fn pre_fix_persisted_rows_read_back_in_real_order_at_every_position() {
    let dir = std::env::temp_dir().join(format!("swarm-adv-465-heal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let library = Library::open(dir.join("lib.db").to_str().unwrap()).await.unwrap();

    for (index, stale) in OLD_TMDB_S06.iter().enumerate() {
        let episode = (index + 1) as u32;
        library
            .upsert(&record("Tales from the Crypt", 6, episode, stale))
            .await
            .unwrap();
        library
            .set_episode_title(&format!("Tales from the Crypt-6-{episode}"), Some(stale))
            .await
            .unwrap();
    }

    let listed = library.list().await.unwrap();
    for (index, expected) in CANONICAL_S06.iter().enumerate().take(OLD_TMDB_S06.len()) {
        let episode = (index + 1) as u32;
        let row = listed
            .iter()
            .find(|e| e.episode == Some(episode))
            .unwrap_or_else(|| panic!("row for S06E{episode:02}"));
        assert_eq!(row.episode_title.as_deref(), Some(*expected), "listed S06E{episode:02}");
        assert_eq!(
            row.to_catalog_entry().episode_title.as_deref(),
            Some(*expected),
            "catalog S06E{episode:02}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The read path applies the correction on every read, so re-applying it to
/// its own output (From<EntryRow> then to_catalog_entry) must be a no-op for
/// every position and every stored shape a real library can contain.
#[test]
fn correction_is_idempotent_across_positions_and_stored_shapes() {
    let mut stored: Vec<&str> = Vec::new();
    stored.extend(CANONICAL_S06);
    stored.extend(OLD_TMDB_S06);
    stored.push("");
    stored.push("unrelated stale title");
    for episode in 1..=17u32 {
        for value in &stored {
            if value.is_empty() {
                continue;
            }
            let once = corrected_episode_title(Some("Tales from the Crypt"), Some(6), Some(episode), Some(value));
            let twice = corrected_episode_title(
                Some("Tales from the Crypt"),
                Some(6),
                Some(episode),
                once.as_deref(),
            );
            assert_eq!(twice, once, "S06E{episode:02} stored {value:?}");
        }
    }
}
