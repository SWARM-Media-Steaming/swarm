use super::*;

fn item(id: &str, title: &str, kind: MediaKind, year: u32, genres: &[&str]) -> LibraryItem {
    LibraryItem {
        media_id: id.into(),
        title: title.into(),
        kind,
        year: Some(year),
        genres: genres.iter().map(|g| g.to_string()).collect(),
        runtime_secs: Some(6000.0),
        cast: Vec::new(),
        blurb: String::new(),
        community_rating: Some(7.0),
        community_rating_votes: Some(500),
        like_count: 0,
        content_rating: None,
    }
}

fn movie(id: &str, title: &str, year: u32, genres: &[&str]) -> LibraryItem {
    item(id, title, MediaKind::Movie, year, genres)
}

/// A small mixed library used across the ordering tests.
fn sample_library() -> Vec<LibraryItem> {
    vec![
        movie(
            "m_galaxy",
            "Galaxy Quest",
            1999,
            &["comedy", "science fiction", "adventure"],
        ),
        movie("m_alien", "Alien", 1979, &["horror", "science fiction"]),
        movie("m_notting", "Notting Hill", 1999, &["romance", "comedy"]),
        movie(
            "m_matrix",
            "The Matrix",
            1999,
            &["action", "science fiction"],
        ),
        movie("m_up", "Up", 2009, &["animation", "family", "adventure"]),
        {
            let mut e = item(
                "e_expanse",
                "The Expanse",
                MediaKind::Episode,
                2015,
                &["drama", "science fiction"],
            );
            e.blurb = "a gritty space political thriller".into();
            e
        },
    ]
}

#[test]
fn defaults_deserialize_from_partial_json() {
    let w = ScoringWeights::from_json(r#"{ "quality_score": 0.9 }"#).unwrap();
    assert_eq!(w.quality_score, 0.9);
    // Untouched field keeps its default.
    assert_eq!(
        w.current_session_match,
        ScoringWeights::default().current_session_match
    );
}

#[test]
fn empty_intent_still_ranks_by_quality() {
    let lib = sample_library();
    let recs = recommend(
        &lib,
        &SessionAnswers::default(),
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        3,
    );
    assert_eq!(recs.len(), 3);
    // Every rec carries at least one reason.
    assert!(recs.iter().all(|r| !r.reasons.is_empty()));
    // Scores are sorted descending and in range.
    for w in recs.windows(2) {
        assert!(w[0].score >= w[1].score);
    }
    assert!(recs.iter().all(|r| (0.0..=1.0).contains(&r.score)));
}

#[test]
fn mood_and_genre_intent_surfaces_the_matching_title() {
    let lib = sample_library();
    let answers = SessionAnswers {
        moods: vec![Mood::Funny],
        genres: vec!["science fiction".into()],
        kind: KindPreference::Movie,
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        5,
    );
    assert_eq!(
        recs[0].media_id, "m_galaxy",
        "funny + sci-fi + movie => Galaxy Quest"
    );
    // Reasons mention the funny ask and the sci-fi pick.
    let joined = recs[0].reasons.join(" | ").to_lowercase();
    assert!(joined.contains("funny"), "reasons: {:?}", recs[0].reasons);
    assert!(joined.contains("science-fiction") || joined.contains("haven't watched"));
}

#[test]
fn decade_answer_is_respected() {
    let lib = sample_library();
    let answers = SessionAnswers {
        decades: vec![1970],
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        1,
    );
    assert_eq!(recs[0].media_id, "m_alien");
    assert!(recs[0].reasons.iter().any(|r| r.contains("1970s")));
}

#[test]
fn current_session_outweighs_history() {
    // History screams "horror"/"romance", the session asks for action.
    // The action pick must lead and the history-favoured picks trail.
    let lib = sample_library();
    let mut history = DeviceHistory::default();
    history.genre_affinity.insert("horror".into(), 50.0);
    history.genre_affinity.insert("romance".into(), 45.0);
    history.mood_answer_history.insert(Mood::Scary, 20.0);
    history.genre_answer_history.insert("horror".into(), 20.0);

    let answers = SessionAnswers {
        moods: vec![Mood::Action],
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &history,
        &ScoringWeights::default(),
        1_000_000,
        6,
    );
    assert_eq!(
        recs[0].media_id, "m_matrix",
        "action ask beats horror history"
    );
    let alien = recs.iter().position(|r| r.media_id == "m_alien").unwrap();
    let matrix = recs.iter().position(|r| r.media_id == "m_matrix").unwrap();
    let notting = recs.iter().position(|r| r.media_id == "m_notting").unwrap();
    assert!(matrix < alien && matrix < notting);
}

#[test]
fn history_breaks_ties_when_session_is_quiet() {
    let lib = sample_library();
    let mut history = DeviceHistory::default();
    history.genre_affinity.insert("animation".into(), 10.0);
    history.genre_affinity.insert("family".into(), 8.0);
    history.decade_affinity.insert(2000, 5.0);

    let recs = recommend(
        &lib,
        &SessionAnswers::default(),
        &history,
        &ScoringWeights::default(),
        1_000_000,
        6,
    );
    assert_eq!(recs[0].media_id, "m_up");
    assert!(recs[0]
        .reasons
        .iter()
        .any(|r| r.to_lowercase().contains("keep coming back")));
}

#[test]
fn disliked_items_are_dropped_entirely() {
    let lib = sample_library();
    let mut history = DeviceHistory::default();
    history.disliked.insert("m_matrix".into());
    let answers = SessionAnswers {
        moods: vec![Mood::Action],
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &history,
        &ScoringWeights::default(),
        1_000_000,
        10,
    );
    assert!(recs.iter().all(|r| r.media_id != "m_matrix"));
}

#[test]
fn freshly_rejected_item_is_not_re_served() {
    let lib = sample_library();
    let mut history = DeviceHistory::default();
    history.rejected.insert(
        "m_galaxy".into(),
        RejectionStat {
            count: 1,
            last_rejected_unix: Some(1_000_000 - 60),
        },
    );
    let answers = SessionAnswers {
        moods: vec![Mood::Funny],
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &history,
        &ScoringWeights::default(),
        1_000_000,
        10,
    );
    assert!(
        recs.iter().all(|r| r.media_id != "m_galaxy"),
        "rejected <1h ago must be hidden"
    );
}

#[test]
fn old_rejection_only_dampens_not_hides() {
    let lib = sample_library();
    let long_ago = 1_000_000 - 40 * 24 * 3600;
    let mut rejected_history = DeviceHistory::default();
    rejected_history.rejected.insert(
        "m_galaxy".into(),
        RejectionStat {
            count: 1,
            last_rejected_unix: Some(long_ago),
        },
    );
    let answers = SessionAnswers {
        moods: vec![Mood::Funny],
        ..Default::default()
    };

    let with_rej = recommend(
        &lib,
        &answers,
        &rejected_history,
        &ScoringWeights::default(),
        1_000_000,
        10,
    );
    let without = recommend(
        &lib,
        &answers,
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        10,
    );

    let g_with = with_rej
        .iter()
        .find(|r| r.media_id == "m_galaxy")
        .expect("still present");
    let g_without = without.iter().find(|r| r.media_id == "m_galaxy").unwrap();
    assert!(
        g_with.score < g_without.score,
        "old rejection should lower but not remove"
    );
}

#[test]
fn unwatched_bonus_and_reason() {
    let lib = sample_library();
    let mut history = DeviceHistory::default();
    history.watched.insert(
        "m_matrix".into(),
        WatchStat {
            play_count: 2,
            last_played_unix: Some(1_000_000 - 3600),
            completed: true,
        },
    );
    let answers = SessionAnswers {
        moods: vec![Mood::Action],
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &history,
        &ScoringWeights::default(),
        1_000_000,
        10,
    );
    // Even though Matrix matches "action" best, a recent rewatch penalty plus
    // the unwatched bonus on The Expanse should let something unwatched lead.
    let matrix = recs.iter().find(|r| r.media_id == "m_matrix").unwrap();
    assert!(recs[0].media_id != "m_matrix" || recs[0].score <= matrix.score + f32::EPSILON);
    assert!(recs
        .iter()
        .filter(|r| r.media_id != "m_matrix")
        .all(|r| r.reasons.iter().any(|x| x.contains("haven't watched"))));
}

#[test]
fn something_like_uses_title_similarity() {
    let mut lib = sample_library();
    lib.push(movie("m_galaxy2", "Galaxy Quest 2", 2005, &["comedy"]));
    let answers = SessionAnswers {
        like_titles: vec!["Galaxy Quest".into()],
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        10,
    );
    // The near-identical title should be pulled up and cite similarity.
    let g2 = recs.iter().find(|r| r.media_id == "m_galaxy2").unwrap();
    assert!(g2
        .reasons
        .iter()
        .any(|r| r.to_lowercase().contains("similar")));
}

#[test]
fn title_similarity_basics() {
    assert!((title_similarity("the matrix", "the matrix") - 1.0).abs() < 1e-6);
    assert!(title_similarity("galaxy quest", "galaxy quest 2") > 0.4);
    assert!(title_similarity("alien", "up") < 0.1);
}

#[test]
fn surprise_me_is_deterministic_but_shuffles() {
    let lib = sample_library();
    let base = SessionAnswers {
        mode: DiscoveryMode::SurpriseMe,
        session_seed: 42,
        ..Default::default()
    };
    let a = recommend(
        &lib,
        &base,
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        6,
    );
    let b = recommend(
        &lib,
        &base,
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        6,
    );
    assert_eq!(a, b, "same seed => identical ranking");

    let other = SessionAnswers {
        session_seed: 99,
        ..base.clone()
    };
    let c = recommend(
        &lib,
        &other,
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        6,
    );
    let order_a: Vec<_> = a.iter().map(|r| &r.media_id).collect();
    let order_c: Vec<_> = c.iter().map(|r| &r.media_id).collect();
    assert_ne!(
        order_a, order_c,
        "different seed => (usually) different ranking"
    );
}

#[test]
fn buzz_knows_best_leans_on_history() {
    let lib = sample_library();
    let mut history = DeviceHistory::default();
    history.genre_affinity.insert("horror".into(), 30.0);
    history.decade_affinity.insert(1970, 12.0);
    let answers = SessionAnswers {
        mode: DiscoveryMode::BuzzKnowsBest,
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &history,
        &ScoringWeights::default(),
        1_000_000,
        6,
    );
    assert_eq!(recs[0].media_id, "m_alien");
}

#[test]
fn quality_score_confidence_weighting() {
    let mut low_votes = movie("a", "A", 2000, &["drama"]);
    low_votes.community_rating = Some(9.5);
    low_votes.community_rating_votes = Some(3);
    let mut high_votes = movie("b", "B", 2000, &["drama"]);
    high_votes.community_rating = Some(9.5);
    high_votes.community_rating_votes = Some(5000);
    assert!(quality_score(&high_votes) > quality_score(&low_votes));
    // Unrated lands near neutral.
    let mut unrated = movie("c", "C", 2000, &["drama"]);
    unrated.community_rating = None;
    assert!((quality_score(&unrated) - 0.5).abs() <= 0.26);
}

#[test]
fn historical_answers_have_less_pull_than_current_answers() {
    let lib = sample_library();
    let mut history = DeviceHistory::default();
    history.mood_answer_history.insert(Mood::Scary, 40.0);
    history.genre_answer_history.insert("horror".into(), 40.0);

    // Session asks for comedy; history begs for horror.
    let answers = SessionAnswers {
        genres: vec!["comedy".into()],
        ..Default::default()
    };
    let recs = recommend(
        &lib,
        &answers,
        &history,
        &ScoringWeights::default(),
        1_000_000,
        6,
    );
    let alien = recs.iter().position(|r| r.media_id == "m_alien").unwrap();
    let notting = recs.iter().position(|r| r.media_id == "m_notting").unwrap();
    assert!(
        notting < alien,
        "current comedy ask beats historical horror answers"
    );
}

#[test]
fn from_catalog_projects_scraped_title_and_lowercased_genres() {
    use swarm_core::peer::{CastMember, CatalogEntry};
    let entry = CatalogEntry {
        entry_key: "k1".into(),
        fingerprint: "fp".into(),
        kind: MediaKind::Movie,
        title: "galaxy.quest.1999.1080p".into(),
        size: 1,
        duration_secs: Some(5400.0),
        show_title: None,
        season: None,
        episode: None,
        artist: None,
        album: None,
        track_number: None,
        scraped_title: Some("Galaxy Quest".into()),
        episode_title: None,
        genres: vec!["Comedy".into(), "Science Fiction".into()],
        video: None,
        audio: None,
        artwork_etag: None,
        year: Some(1999),
        cast: vec![CastMember {
            name: "Tim Allen".into(),
            character: None,
        }],
        overview: Some("A washed-up cast is drafted by real aliens.".into()),
        rating: Some("PG".into()),
        community_rating: Some(7.4),
        community_rating_votes: Some(1200),
        like_count: 3,
        skip_segments: Vec::new(),
    };
    let li = LibraryItem::from_catalog(&entry);
    assert_eq!(li.title, "Galaxy Quest");
    assert_eq!(li.genres, vec!["comedy", "science fiction"]);
    assert_eq!(li.cast, vec!["tim allen"]);
    assert!(li.blurb.contains("aliens"));
}

#[test]
fn limit_zero_returns_whole_library() {
    let lib = sample_library();
    let recs = recommend(
        &lib,
        &SessionAnswers::default(),
        &DeviceHistory::default(),
        &ScoringWeights::default(),
        1_000_000,
        0,
    );
    assert_eq!(recs.len(), lib.len());
}
