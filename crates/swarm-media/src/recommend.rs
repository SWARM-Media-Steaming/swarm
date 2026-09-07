//! Buzz local recommendation engine (issue #121, Phase 1).
//!
//! A pure, dependency-light ranking function over the device's *own* SWARM
//! library. No LLM, no network, no external service — every signal comes
//! from catalog metadata already scraped by [`crate::scrape`] plus the
//! caller-supplied discovery intent and historical behaviour.
//!
//! The scoring model is the one sketched in the issue:
//!
//! ```text
//! score =
//!     current_session_match      // what the user picked *this* session
//!   + user_taste_match           // likes/dislikes, watch affinity
//!   + device_history_match       // genre/decade/kind affinity from history
//!   + historical_answer_match    // softer echo of past Buzz answers
//!   + title_similarity           // "something like ..." seeds
//!   + unwatched_bonus
//!   + quality_score
//!   - rejection_penalties        // recently rejected / disliked / rewatch
//! ```
//!
//! Every positive component is normalised to roughly `0.0..=1.0`, multiplied
//! by a configurable weight ([`ScoringWeights`], deserialisable so an
//! operator can retune without a rebuild), summed, and squashed back into
//! `0.0..=1.0`. Penalties are subtracted after weighting. Current-session
//! intent carries the largest default weight so history *guides* rather than
//! *traps* (issue requirement).
//!
//! The engine is deterministic: "Surprise Me" jitter is derived from a
//! caller-provided `session_seed` hashed with each `media_id`, so a given
//! session always ranks the library the same way and tests are stable.

use std::collections::{HashMap, HashSet};

use swarm_core::peer::{CatalogEntry, MediaKind};

// ---------------------------------------------------------------------------
// Library feature view
// ---------------------------------------------------------------------------

/// Feature view of one library asset the engine can score. Built from a
/// [`CatalogEntry`] via [`LibraryItem::from_catalog`], but kept as its own
/// type so the engine has no opinion about catalog storage and can be
/// exercised from tests with hand-built fixtures.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryItem {
    pub media_id: String,
    /// Display title — the scraped title when present, else the on-disk one.
    pub title: String,
    pub kind: MediaKind,
    pub year: Option<u32>,
    /// Lower-cased genre labels.
    pub genres: Vec<String>,
    pub runtime_secs: Option<f64>,
    /// Lower-cased cast names (billing order preserved).
    pub cast: Vec<String>,
    /// Free text used for keyword/mood matching (overview + episode/show
    /// titles). Lower-cased at construction.
    pub blurb: String,
    /// Community score on a 0–10 scale, if scraped.
    pub community_rating: Option<f64>,
    pub community_rating_votes: Option<u64>,
    /// Distinct devices that currently like this asset.
    pub like_count: u32,
    /// US content certification (`"PG-13"`, `"TV-MA"`, …) when scraped.
    pub content_rating: Option<String>,
}

impl LibraryItem {
    /// Project a catalog entry into the engine's feature view. Tracks are
    /// accepted (music can be recommended too) but carry few features.
    pub fn from_catalog(entry: &CatalogEntry) -> Self {
        let title = entry
            .scraped_title
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| entry.title.clone());
        let mut blurb = String::new();
        if let Some(o) = &entry.overview {
            blurb.push_str(o);
            blurb.push(' ');
        }
        if let Some(t) = &entry.episode_title {
            blurb.push_str(t);
            blurb.push(' ');
        }
        if let Some(t) = &entry.show_title {
            blurb.push_str(t);
        }
        LibraryItem {
            media_id: entry.entry_key.clone(),
            title,
            kind: entry.kind,
            year: entry.year,
            genres: entry
                .genres
                .iter()
                .map(|g| g.trim().to_ascii_lowercase())
                .filter(|g| !g.is_empty())
                .collect(),
            runtime_secs: entry.duration_secs,
            cast: entry
                .cast
                .iter()
                .map(|c| c.name.trim().to_ascii_lowercase())
                .filter(|n| !n.is_empty())
                .collect(),
            blurb: blurb.to_ascii_lowercase(),
            community_rating: entry.community_rating,
            community_rating_votes: entry.community_rating_votes,
            like_count: entry.like_count,
            content_rating: entry.rating.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Discovery intent (current session)
// ---------------------------------------------------------------------------

/// The three discovery modes shipping in the MVP. Later modes ("This or
/// That", "Deep Cut", "Group Mode", …) can be added as variants without
/// touching the scoring core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMode {
    /// Guided questions, full weighting.
    #[default]
    FindMeSomething,
    /// Lean on quality + novelty, damp personal history, add jitter.
    SurpriseMe,
    /// Let history speak loudest — minimal questions.
    BuzzKnowsBest,
}

/// A coarse "what kind of night is this" mood. Each maps to a set of genre
/// and keyword hints used by [`current_session_match`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mood {
    Funny,
    Weird,
    Action,
    Scary,
    Chill,
    Intense,
    Emotional,
    Nostalgic,
}

impl Mood {
    /// The defining genre labels for this mood — a hit here is a strong
    /// current-session signal.
    pub fn primary_genres(self) -> &'static [&'static str] {
        match self {
            Mood::Funny => &["comedy"],
            Mood::Weird => &["science fiction", "fantasy"],
            Mood::Action => &["action"],
            Mood::Scary => &["horror"],
            Mood::Chill => &["comedy", "romance", "family"],
            Mood::Intense => &["thriller", "crime"],
            Mood::Emotional => &["drama", "romance"],
            Mood::Nostalgic => &["family", "animation"],
        }
    }

    /// Adjacent genres that only weakly imply this mood.
    pub fn secondary_genres(self) -> &'static [&'static str] {
        match self {
            Mood::Funny => &["animation"],
            Mood::Weird => &["mystery", "animation"],
            Mood::Action => &["adventure", "thriller", "war", "science fiction"],
            Mood::Scary => &["thriller", "mystery"],
            Mood::Chill => &["documentary", "animation", "music"],
            Mood::Intense => &["drama", "mystery", "war"],
            Mood::Emotional => &["history", "music"],
            Mood::Nostalgic => &["adventure", "fantasy"],
        }
    }

    /// All genre labels this mood favours (primary then secondary) — used
    /// for reason phrasing and historical-answer matching.
    pub fn genres(self) -> Vec<&'static str> {
        self.primary_genres()
            .iter()
            .chain(self.secondary_genres())
            .copied()
            .collect()
    }

    /// `1.0` for a primary genre, `0.5` for a secondary one, else `0.0`.
    fn genre_tier(self, genre: &str) -> f32 {
        if self.primary_genres().contains(&genre) {
            1.0
        } else if self.secondary_genres().contains(&genre) {
            0.5
        } else {
            0.0
        }
    }

    /// Free-text keywords this mood favours (matched against title + blurb).
    pub fn keywords(self) -> &'static [&'static str] {
        match self {
            Mood::Funny => &["funny", "hilarious", "comedy", "laugh"],
            Mood::Weird => &["surreal", "bizarre", "strange", "weird", "trippy"],
            Mood::Action => &["explosive", "chase", "fight", "mission"],
            Mood::Scary => &["terrifying", "haunt", "nightmare", "slasher", "scary"],
            Mood::Chill => &["heartwarming", "cozy", "gentle", "feel-good"],
            Mood::Intense => &["gripping", "relentless", "tense", "twist"],
            Mood::Emotional => &["moving", "poignant", "tearjerker", "heartbreaking"],
            Mood::Nostalgic => &["classic", "childhood", "timeless"],
        }
    }

    /// Human label for recommendation reasons.
    pub fn reason_label(self) -> &'static str {
        match self {
            Mood::Funny => "something funny",
            Mood::Weird => "something weird",
            Mood::Action => "some action",
            Mood::Scary => "something scary",
            Mood::Chill => "something easy",
            Mood::Intense => "something intense",
            Mood::Emotional => "something moving",
            Mood::Nostalgic => "something nostalgic",
        }
    }
}

/// Which media kind the session asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindPreference {
    #[default]
    DontCare,
    Movie,
    Show,
}

/// Which era the session asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EraPreference {
    #[default]
    DontCare,
    Older,
    Newer,
}

/// Everything the user has told Buzz *this* session. Empty/`DontCare`
/// fields contribute nothing rather than penalising.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionAnswers {
    pub mode: DiscoveryMode,
    pub moods: Vec<Mood>,
    /// Explicit genre asks (any case; normalised on use).
    pub genres: Vec<String>,
    pub kind: KindPreference,
    pub era: EraPreference,
    /// Specific decades the user picked, e.g. `1990` for "the 90s".
    pub decades: Vec<u32>,
    /// Free-text keyword asks ("heist", "space", …).
    pub keywords: Vec<String>,
    /// "Keep it short" style asks.
    pub max_runtime_secs: Option<f64>,
    /// Titles for "Something Like …" — matched by [`title_similarity`].
    pub like_titles: Vec<String>,
    /// A stable per-session value; drives deterministic "Surprise Me" jitter.
    pub session_seed: u64,
}

// ---------------------------------------------------------------------------
// Historical behaviour
// ---------------------------------------------------------------------------

/// Per-asset watch state, aggregated by the caller from client-reported
/// playback outcomes / Buzz `media_played` events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WatchStat {
    pub play_count: u32,
    pub last_played_unix: Option<i64>,
    /// At least one play reached the end.
    pub completed: bool,
}

/// Per-asset rejection state, aggregated from Buzz `recommendation_skipped`
/// / `not_interested` events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RejectionStat {
    pub count: u32,
    pub last_rejected_unix: Option<i64>,
}

/// Aggregated history for the requesting device (and profile, when SWARM
/// has one). All affinity maps hold arbitrary non-negative weights — the
/// engine only ever compares them relative to the map's own maximum, so the
/// caller can use raw counts, recency-decayed counts, whatever.
#[derive(Debug, Clone, Default)]
pub struct DeviceHistory {
    pub genre_affinity: HashMap<String, f32>,
    pub decade_affinity: HashMap<u32, f32>,
    pub kind_affinity: HashMap<MediaKind, f32>,
    /// Lower-case actor name -> affinity weight.
    pub actor_affinity: HashMap<String, f32>,
    /// media_id -> watch stats.
    pub watched: HashMap<String, WatchStat>,
    pub liked: HashSet<String>,
    pub disliked: HashSet<String>,
    /// media_id -> rejection stats (Buzz "not interested" / skipped).
    pub rejected: HashMap<String, RejectionStat>,
    /// Softer echo of past Buzz answers: mood -> times chosen.
    pub mood_answer_history: HashMap<Mood, f32>,
    /// Softer echo of past Buzz answers: lower-case genre -> times chosen.
    pub genre_answer_history: HashMap<String, f32>,
    /// Titles the device recently watched/liked — similarity seeds even when
    /// the session didn't ask for "something like".
    pub similarity_seed_titles: Vec<String>,
}

impl DeviceHistory {
    /// Has this device played this asset at all?
    fn watched_item(&self, media_id: &str) -> Option<&WatchStat> {
        self.watched.get(media_id).filter(|w| w.play_count > 0)
    }
}

// ---------------------------------------------------------------------------
// Weights
// ---------------------------------------------------------------------------

/// Configurable component weights. Deserialisable with per-field defaults so
/// an operator can drop a partial `buzz_weights.json` and retune scoring
/// without a rebuild. Defaults deliberately make `current_session_match`
/// dominant.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ScoringWeights {
    pub current_session_match: f32,
    pub user_taste_match: f32,
    pub device_history_match: f32,
    pub historical_answer_match: f32,
    pub title_similarity: f32,
    pub unwatched_bonus: f32,
    pub quality_score: f32,
    /// Penalty (subtracted) for an asset rejected this or a recent session.
    pub rejection_penalty: f32,
    /// Penalty for suggesting something already watched.
    pub rewatch_penalty: f32,
    /// Penalty for something the device explicitly disliked.
    pub dislike_penalty: f32,
    /// Half-life, seconds, of the rejection penalty's recency decay.
    pub rejection_halflife_secs: f32,
    /// "Surprise Me" jitter amplitude, added to the pre-squash raw score.
    pub surprise_jitter: f32,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        ScoringWeights {
            current_session_match: 1.3,
            user_taste_match: 0.3,
            device_history_match: 0.22,
            historical_answer_match: 0.13,
            title_similarity: 0.3,
            unwatched_bonus: 0.18,
            quality_score: 0.2,
            rejection_penalty: 0.7,
            rewatch_penalty: 0.3,
            dislike_penalty: 1.0,
            rejection_halflife_secs: 14.0 * 24.0 * 3600.0,
            surprise_jitter: 0.25,
        }
    }
}

impl ScoringWeights {
    /// Parse a (possibly partial) JSON object; missing fields keep defaults.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    fn positive_total(&self) -> f32 {
        (self.current_session_match
            + self.user_taste_match
            + self.device_history_match
            + self.historical_answer_match
            + self.title_similarity
            + self.unwatched_bonus
            + self.quality_score)
            .max(f32::MIN_POSITIVE)
    }

    /// Mode-specific reweighting applied before scoring.
    fn for_mode(mut self, mode: DiscoveryMode) -> Self {
        match mode {
            DiscoveryMode::FindMeSomething => {}
            DiscoveryMode::SurpriseMe => {
                self.user_taste_match *= 0.4;
                self.device_history_match *= 0.3;
                self.historical_answer_match *= 0.3;
                self.quality_score *= 1.2;
                self.unwatched_bonus *= 1.6;
            }
            DiscoveryMode::BuzzKnowsBest => {
                self.device_history_match *= 1.8;
                self.historical_answer_match *= 1.8;
                self.user_taste_match *= 1.4;
                self.current_session_match *= 0.7;
            }
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// One ranked recommendation with the short, human "why Buzz picked it"
/// reasons the client renders verbatim.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Recommendation {
    pub media_id: String,
    pub title: String,
    /// Final score, `0.0..=1.0`.
    pub score: f32,
    pub reasons: Vec<String>,
}

/// Internal per-component breakdown, retained so reasons can be generated
/// from whatever actually moved the needle.
#[derive(Debug, Clone, Copy, Default)]
struct Breakdown {
    session: f32,
    taste: f32,
    history: f32,
    hist_answers: f32,
    similarity: f32,
    unwatched: f32,
    quality: f32,
    rejection_pen: f32,
    rewatch_pen: f32,
    dislike_pen: f32,
}

// ---------------------------------------------------------------------------
// Engine entry point
// ---------------------------------------------------------------------------

/// Rank `library` for the given discovery intent and history.
///
/// * `now_unix` — current wall-clock seconds, for recency decay.
/// * `limit` — maximum recommendations to return (`0` means "all").
///
/// Assets the device explicitly disliked, and assets rejected within the
/// last hour, are dropped entirely rather than merely penalised, so a
/// "try again" tap never re-serves the thing the user just waved off.
pub fn recommend(
    library: &[LibraryItem],
    answers: &SessionAnswers,
    history: &DeviceHistory,
    weights: &ScoringWeights,
    now_unix: i64,
    limit: usize,
) -> Vec<Recommendation> {
    let weights = weights.for_mode(answers.mode);
    let positive_total = weights.positive_total();
    let genre_max = map_max(&history.genre_affinity);
    let decade_max = map_max(&history.decade_affinity);
    let kind_max = history
        .kind_affinity
        .values()
        .copied()
        .fold(0.0_f32, f32::max);
    let actor_max = map_max(&history.actor_affinity);
    let mood_hist_max = history
        .mood_answer_history
        .values()
        .copied()
        .fold(0.0_f32, f32::max);
    let genre_hist_max = map_max(&history.genre_answer_history);

    let mut seed_titles: Vec<String> = answers
        .like_titles
        .iter()
        .chain(history.similarity_seed_titles.iter())
        .map(|t| t.to_ascii_lowercase())
        .collect();
    seed_titles.sort();
    seed_titles.dedup();

    let mut scored: Vec<(Recommendation, f32)> = Vec::new();

    for item in library {
        // Hard filters.
        if history.disliked.contains(&item.media_id) {
            continue;
        }
        if let Some(r) = history.rejected.get(&item.media_id) {
            if let Some(ts) = r.last_rejected_unix {
                if now_unix.saturating_sub(ts) < 3600 {
                    continue;
                }
            }
        }

        let lower_title = item.title.to_ascii_lowercase();
        let mut b = Breakdown {
            session: current_session_match(item, answers),
            taste: user_taste_match(item, history, genre_max, actor_max),
            history: device_history_match(
                item, history, genre_max, decade_max, kind_max, actor_max,
            ),
            hist_answers: historical_answer_match(item, history, mood_hist_max, genre_hist_max),
            similarity: seed_titles
                .iter()
                .map(|s| title_similarity(&lower_title, s))
                .fold(0.0_f32, f32::max),
            unwatched: if history.watched_item(&item.media_id).is_none() {
                1.0
            } else {
                0.0
            },
            quality: quality_score(item),
            ..Breakdown::default()
        };

        if let Some(w) = history.watched_item(&item.media_id) {
            // Rewatch: milder if it was long ago or never completed.
            let recency = w
                .last_played_unix
                .map(|ts| recency_fraction(now_unix, ts, 30.0 * 24.0 * 3600.0))
                .unwrap_or(0.5);
            b.rewatch_pen =
                (0.4 + 0.6 * recency) * (1.0 + (w.play_count.min(5) as f32 - 1.0) * 0.1);
        }
        if let Some(r) = history.rejected.get(&item.media_id) {
            let recency = r
                .last_rejected_unix
                .map(|ts| recency_fraction(now_unix, ts, weights.rejection_halflife_secs))
                .unwrap_or(0.5);
            b.rejection_pen = recency * (1.0 + (r.count.min(5) as f32 - 1.0) * 0.15);
        }
        if history.disliked.contains(&item.media_id) {
            b.dislike_pen = 1.0;
        }

        let raw_positive = weights.current_session_match * b.session
            + weights.user_taste_match * b.taste
            + weights.device_history_match * b.history
            + weights.historical_answer_match * b.hist_answers
            + weights.title_similarity * b.similarity
            + weights.unwatched_bonus * b.unwatched
            + weights.quality_score * b.quality;

        let jitter = if matches!(answers.mode, DiscoveryMode::SurpriseMe) {
            weights.surprise_jitter * jitter_fraction(answers.session_seed, &item.media_id)
        } else {
            0.0
        };

        let penalty = weights.rejection_penalty * b.rejection_pen
            + weights.rewatch_penalty * b.rewatch_pen
            + weights.dislike_penalty * b.dislike_pen;

        let normalised = ((raw_positive + jitter) / positive_total).clamp(0.0, 1.0);
        let score = (normalised - penalty / positive_total).clamp(0.0, 1.0);

        let reasons = build_reasons(item, answers, history, &b, &weights);
        scored.push((
            Recommendation {
                media_id: item.media_id.clone(),
                title: item.title.clone(),
                score: round3(score),
                reasons,
            },
            // Tie-breaker: prefer higher quality, then stable id order.
            item.community_rating.unwrap_or(0.0) as f32,
        ));
    }

    scored.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.0.media_id.cmp(&b.0.media_id))
    });

    if limit > 0 {
        scored.truncate(limit);
    }
    scored.into_iter().map(|(r, _)| r).collect()
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// How well the asset matches what the user asked for *this* session.
fn current_session_match(item: &LibraryItem, answers: &SessionAnswers) -> f32 {
    let mut signals: Vec<f32> = Vec::new();

    // Mood → genre/keyword.
    if !answers.moods.is_empty() {
        let mut best = 0.0_f32;
        for mood in &answers.moods {
            let genre_tier = item
                .genres
                .iter()
                .map(|ig| mood.genre_tier(ig))
                .fold(0.0_f32, f32::max);
            let kw_hit = mood
                .keywords()
                .iter()
                .any(|k| item.blurb.contains(k) || item.title.to_ascii_lowercase().contains(k));
            let s = match (genre_tier, kw_hit) {
                (t, true) if t >= 1.0 => 1.0,
                (t, false) if t >= 1.0 => 0.9,
                (t, true) if t > 0.0 => 0.45,
                (t, false) if t > 0.0 => 0.3,
                (_, true) => 0.3,
                _ => 0.0,
            };
            best = best.max(s);
        }
        signals.push(best);
    }

    // Explicit genre asks.
    if !answers.genres.is_empty() {
        let asked: Vec<String> = answers
            .genres
            .iter()
            .map(|g| g.to_ascii_lowercase())
            .collect();
        let hit = asked.iter().any(|g| {
            item.genres
                .iter()
                .any(|ig| ig.contains(g.as_str()) || g.contains(ig))
        });
        signals.push(if hit { 1.0 } else { 0.0 });
    }

    // Keyword asks.
    if !answers.keywords.is_empty() {
        let hits = answers
            .keywords
            .iter()
            .filter(|k| {
                let k = k.to_ascii_lowercase();
                item.blurb.contains(&k) || item.title.to_ascii_lowercase().contains(&k)
            })
            .count();
        signals.push((hits as f32 / answers.keywords.len() as f32).min(1.0));
    }

    // Movie / show.
    match answers.kind {
        KindPreference::DontCare => {}
        KindPreference::Movie => signals.push(if item.kind == MediaKind::Movie {
            1.0
        } else {
            0.0
        }),
        KindPreference::Show => signals.push(if item.kind == MediaKind::Episode {
            1.0
        } else {
            0.0
        }),
    }

    // Era.
    match (answers.era, item.year) {
        (EraPreference::DontCare, _) | (_, None) => {}
        (EraPreference::Older, Some(y)) => signals.push(era_ramp(y, false)),
        (EraPreference::Newer, Some(y)) => signals.push(era_ramp(y, true)),
    }

    // Specific decade.
    if !answers.decades.is_empty() {
        if let Some(y) = item.year {
            let decade = (y / 10) * 10;
            signals.push(if answers.decades.contains(&decade) {
                1.0
            } else {
                0.0
            });
        } else {
            signals.push(0.0);
        }
    }

    // Runtime cap.
    if let (Some(cap), Some(rt)) = (answers.max_runtime_secs, item.runtime_secs) {
        signals.push(if rt <= cap * 1.1 { 1.0 } else { 0.0 });
    }

    if signals.is_empty() {
        // No constraints given (pure "Surprise Me" / "Buzz Knows Best"):
        // neutral, so quality/history decide.
        return 0.0;
    }
    signals.iter().sum::<f32>() / signals.len() as f32
}

/// Likes/dislikes and the genres/actors this device's *watch* behaviour
/// leans toward.
fn user_taste_match(
    item: &LibraryItem,
    history: &DeviceHistory,
    genre_max: f32,
    actor_max: f32,
) -> f32 {
    let mut signals: Vec<f32> = Vec::new();
    if history.liked.contains(&item.media_id) {
        signals.push(1.0);
    }
    let genre_lean = affinity_for_genres(&item.genres, &history.genre_affinity, genre_max);
    if genre_lean > 0.0 {
        signals.push(genre_lean);
    }
    let actor_lean = affinity_for_names(&item.cast, &history.actor_affinity, actor_max);
    if actor_lean > 0.0 {
        signals.push(actor_lean * 0.8);
    }
    if signals.is_empty() {
        0.0
    } else {
        (signals.iter().sum::<f32>() / signals.len() as f32).min(1.0)
    }
}

/// Genre / decade / kind affinity accumulated across this device's history.
fn device_history_match(
    item: &LibraryItem,
    history: &DeviceHistory,
    genre_max: f32,
    decade_max: f32,
    kind_max: f32,
    actor_max: f32,
) -> f32 {
    let mut signals: Vec<f32> = Vec::new();
    let g = affinity_for_genres(&item.genres, &history.genre_affinity, genre_max);
    if g > 0.0 {
        signals.push(g);
    }
    if let Some(y) = item.year {
        let decade = (y / 10) * 10;
        if let Some(v) = history.decade_affinity.get(&decade) {
            signals.push((v / decade_max).clamp(0.0, 1.0));
        }
    }
    if kind_max > 0.0 {
        if let Some(v) = history.kind_affinity.get(&item.kind) {
            signals.push((v / kind_max).clamp(0.0, 1.0));
        }
    }
    let a = affinity_for_names(&item.cast, &history.actor_affinity, actor_max);
    if a > 0.0 {
        signals.push(a);
    }
    if signals.is_empty() {
        0.0
    } else {
        signals.iter().sum::<f32>() / signals.len() as f32
    }
}

/// Softer echo of the answers this device has given Buzz in *past* sessions.
fn historical_answer_match(
    item: &LibraryItem,
    history: &DeviceHistory,
    mood_hist_max: f32,
    genre_hist_max: f32,
) -> f32 {
    let mut signals: Vec<f32> = Vec::new();
    if mood_hist_max > 0.0 {
        let mut best = 0.0_f32;
        for (mood, w) in &history.mood_answer_history {
            let hit = mood
                .genres()
                .iter()
                .any(|g| item.genres.iter().any(|ig| ig == g));
            if hit {
                best = best.max((w / mood_hist_max).clamp(0.0, 1.0));
            }
        }
        if best > 0.0 {
            signals.push(best);
        }
    }
    if genre_hist_max > 0.0 {
        let lean = affinity_for_genres(&item.genres, &history.genre_answer_history, genre_hist_max);
        if lean > 0.0 {
            signals.push(lean);
        }
    }
    if signals.is_empty() {
        0.0
    } else {
        signals.iter().sum::<f32>() / signals.len() as f32
    }
}

/// Community rating scaled by how much voting backs it — an unrated or
/// barely-voted title lands at a neutral ~0.5 rather than 0.
fn quality_score(item: &LibraryItem) -> f32 {
    let Some(rating) = item.community_rating else {
        // Fall back to local likes as a weak quality proxy.
        return (0.5 + (item.like_count.min(5) as f32) * 0.05).min(0.75);
    };
    let base = (rating as f32 / 10.0).clamp(0.0, 1.0);
    let votes = item.community_rating_votes.unwrap_or(0) as f32;
    let confidence = (votes / (votes + 50.0)).clamp(0.0, 1.0);
    0.5 + (base - 0.5) * confidence
}

// ---------------------------------------------------------------------------
// Reasons
// ---------------------------------------------------------------------------

fn build_reasons(
    item: &LibraryItem,
    answers: &SessionAnswers,
    history: &DeviceHistory,
    b: &Breakdown,
    weights: &ScoringWeights,
) -> Vec<String> {
    let mut out: Vec<(f32, String)> = Vec::new();

    // Session-driven reasons are the most specific — surface them first.
    if b.session > 0.0 {
        for mood in &answers.moods {
            let genre_hit = mood
                .genres()
                .iter()
                .any(|g| item.genres.iter().any(|ig| ig == g));
            let kw_hit = mood
                .keywords()
                .iter()
                .any(|k| item.blurb.contains(k) || item.title.to_ascii_lowercase().contains(k));
            if genre_hit || kw_hit {
                out.push((
                    weights.current_session_match + 0.5,
                    format!("You wanted {}", mood.reason_label()),
                ));
                break;
            }
        }
        for g in &answers.genres {
            let gl = g.to_ascii_lowercase();
            if item
                .genres
                .iter()
                .any(|ig| ig.contains(&gl) || gl.contains(ig))
            {
                out.push((
                    weights.current_session_match + 0.4,
                    format!("Matches your {} pick", pretty_genre(g)),
                ));
                break;
            }
        }
        if !answers.decades.is_empty() {
            if let Some(y) = item.year {
                let decade = (y / 10) * 10;
                if answers.decades.contains(&decade) {
                    out.push((
                        weights.current_session_match + 0.3,
                        format!("From the {decade}s, like you asked"),
                    ));
                }
            }
        }
        match (answers.era, item.year) {
            (EraPreference::Older, Some(y)) if era_ramp(y, false) > 0.5 => {
                out.push((
                    weights.current_session_match + 0.2,
                    "One of the older ones".into(),
                ));
            }
            (EraPreference::Newer, Some(y)) if era_ramp(y, true) > 0.5 => {
                out.push((
                    weights.current_session_match + 0.2,
                    "Fresh and recent".into(),
                ));
            }
            _ => {}
        }
        match answers.kind {
            KindPreference::Movie if item.kind == MediaKind::Movie => {
                out.push((weights.current_session_match + 0.1, "It's a movie".into()));
            }
            KindPreference::Show if item.kind == MediaKind::Episode => {
                out.push((weights.current_session_match + 0.1, "It's a show".into()));
            }
            _ => {}
        }
    }

    if b.similarity > 0.45 {
        out.push((
            weights.title_similarity * b.similarity + 0.3,
            "Similar to something you like".into(),
        ));
    }

    if b.unwatched > 0.0 {
        out.push((
            weights.unwatched_bonus + 0.05,
            "You haven't watched it yet".into(),
        ));
    }

    if b.taste > 0.35 || b.history > 0.35 {
        if let Some(g) = dominant_affinity_genre(item, history) {
            out.push((
                weights.user_taste_match * b.taste + weights.device_history_match * b.history,
                format!("You keep coming back to {}", pretty_genre(&g)),
            ));
        }
    }
    if history.liked.contains(&item.media_id) {
        out.push((
            weights.user_taste_match + 0.5,
            "You liked this one before".into(),
        ));
    }

    if b.hist_answers > 0.4 {
        out.push((
            weights.historical_answer_match * b.hist_answers,
            "Your kind of thing, going by past nights".into(),
        ));
    }

    if b.quality > 0.72 {
        out.push((
            weights.quality_score * b.quality,
            "Highly rated by other viewers".into(),
        ));
    }

    if let Some(actor) = dominant_cast(item, history) {
        out.push((0.4, format!("Stars {}", title_case(&actor))));
    }

    out.sort_by(|a, c| c.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut reasons: Vec<String> = Vec::new();
    for (_, r) in out {
        if !reasons.contains(&r) {
            reasons.push(r);
        }
        if reasons.len() == 3 {
            break;
        }
    }
    if reasons.is_empty() {
        reasons.push("A solid pick from your library".into());
    }
    reasons
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn map_max<K>(m: &HashMap<K, f32>) -> f32 {
    m.values().copied().fold(0.0_f32, f32::max)
}

fn affinity_for_genres(genres: &[String], map: &HashMap<String, f32>, max: f32) -> f32 {
    if max <= 0.0 {
        return 0.0;
    }
    genres
        .iter()
        .filter_map(|g| map.get(g))
        .map(|v| (v / max).clamp(0.0, 1.0))
        .fold(0.0_f32, f32::max)
}

fn affinity_for_names(names: &[String], map: &HashMap<String, f32>, max: f32) -> f32 {
    if max <= 0.0 {
        return 0.0;
    }
    names
        .iter()
        .filter_map(|n| map.get(n))
        .map(|v| (v / max).clamp(0.0, 1.0))
        .fold(0.0_f32, f32::max)
}

fn dominant_affinity_genre(item: &LibraryItem, history: &DeviceHistory) -> Option<String> {
    item.genres
        .iter()
        .filter_map(|g| {
            let w = history
                .genre_affinity
                .get(g)
                .copied()
                .unwrap_or(0.0)
                .max(history.genre_answer_history.get(g).copied().unwrap_or(0.0));
            (w > 0.0).then(|| (g.clone(), w))
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(g, _)| g)
}

fn dominant_cast(item: &LibraryItem, history: &DeviceHistory) -> Option<String> {
    item.cast
        .iter()
        .filter_map(|n| {
            let w = history.actor_affinity.get(n).copied().unwrap_or(0.0);
            (w > 0.0).then(|| (n.clone(), w))
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(n, _)| n)
}

/// Token-set Jaccard similarity between two lower-cased titles, ignoring a
/// few stop words and a trailing `(year)`.
pub fn title_similarity(a: &str, b: &str) -> f32 {
    let ta = title_tokens(a);
    let tb = title_tokens(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    if ta == tb {
        return 1.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    inter / union
}

fn title_tokens(s: &str) -> HashSet<String> {
    const STOP: &[&str] = &["the", "a", "an", "of", "and", "part", "vol", "volume"];
    s.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.trim().to_ascii_lowercase())
        .filter(|w| w.len() > 1 && !STOP.contains(&w.as_str()) && w.parse::<u32>().is_err())
        .collect()
}

/// `true` == favour newer. Ramps 0→1 across 1970..=2020 (or the reverse).
fn era_ramp(year: u32, favour_newer: bool) -> f32 {
    let t = ((year as f32 - 1970.0) / 50.0).clamp(0.0, 1.0);
    if favour_newer {
        t
    } else {
        1.0 - t
    }
}

/// `1.0` when `ts == now`, decaying to `0.5` at one half-life, →`0` as it ages.
fn recency_fraction(now_unix: i64, ts: i64, halflife_secs: f32) -> f32 {
    let age = (now_unix.saturating_sub(ts)).max(0) as f32;
    0.5_f32.powf(age / halflife_secs.max(1.0))
}

/// Deterministic `[0,1)` jitter from a session seed and a media id (FNV-1a).
fn jitter_fraction(seed: u64, media_id: &str) -> f32 {
    let mut h: u64 = 0xcbf29ce484222325 ^ seed;
    for byte in media_id.as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    ((h >> 11) as f32) / ((1u64 << 53) as f32)
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

fn pretty_genre(g: &str) -> String {
    match g.to_ascii_lowercase().as_str() {
        "science fiction" | "sci-fi" | "scifi" => "science-fiction".into(),
        other => other.to_string(),
    }
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
