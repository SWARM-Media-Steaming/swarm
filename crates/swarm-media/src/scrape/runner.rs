//! One-shot bulk scrape job: walk entries missing a scrape result, resolve
//! them against TMDb (movies/episodes) or MusicBrainz+Cover Art
//! Archive+Wikimedia (tracks), and write the results back to the library.
//!
//! Inherited scar tissue from Batocera.Drone's scrape jobs: entries are
//! processed one-at-a-time so a single 404 fails one title instead of the
//! whole run (`NotFound` vs `Unavailable` — only the latter leaves the entry
//! unscraped for a retry); music is matched **per (artist, album)**, not per
//! track, both because that's the natural unit for MusicBrainz releases and
//! because it turns N tracks into one MusicBrainz call under its 1 req/s
//! throttle; artwork downloads are best-effort and never affect the
//! matched/not-found/failed counts. Concurrent runs are the caller's
//! responsibility to prevent (see `ServerCore`'s scrape guard).

use crate::classify;
use crate::roots::SharedRootResolver;
use crate::scrape::artwork;
use crate::scrape::coverart::{CoverArtClient, CoverArtError};
use crate::scrape::introdb::{IntroDbClient, IntroDbError};
use crate::scrape::lrclib::{LrclibClient, LrclibError};
use crate::scrape::musicbrainz::{MbError, MusicBrainzClient};
use crate::scrape::tmdb::{
    ScrapedEpisode, ScrapedSeason, ScrapedVideo, TmdbClient, TmdbError, TmdbOverride,
};
use crate::scrape::wikimedia::WikimediaClient;
use crate::store::{ArtworkKind, CastMember, EntryRecord, Library};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use swarm_core::peer::MediaKind;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Default)]
pub struct ScrapeConfig {
    /// Movie/episode scraping is skipped entirely (counted as `skipped`)
    /// when no key is configured — TMDb requires one, unlike the keyless
    /// music sources.
    pub tmdb_api_key: Option<String>,
    /// Overrides for the real service URLs — used by tests to point at a
    /// local mock, and available to self-hosters who want to run behind a
    /// mirror. `None` means the real public endpoint.
    pub tmdb_api_base: Option<String>,
    pub tmdb_image_base: Option<String>,
    pub introdb_api_base: Option<String>,
    pub musicbrainz_base: Option<String>,
    pub coverart_base: Option<String>,
    pub wikimedia_base: Option<String>,
    pub lrclib_base: Option<String>,
}

/// One entry (or, for a music `(artist, album)` group, one representative
/// track) that didn't come back `matched`, with enough detail to actually
/// act on it — the aggregate counts alone can't distinguish "this title
/// genuinely isn't on TMDb" from "every request failed because of a bad
/// API key," which is exactly the ambiguity that made a real 401-vs-key-
/// format bug (see `tmdb::is_v4_read_access_token`) invisible until someone
/// went looking at debug logs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScrapeIssue {
    pub entry_key: String,
    pub title: String,
    pub reason: String,
    /// Lets the AI tab's "Scan & scrape assist" panel group issues into
    /// Movies/Shows/Music instead of one jumbled list.
    pub kind: MediaKind,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BulkScrapeReport {
    pub matched: u64,
    pub not_found: u64,
    pub failed: u64,
    pub skipped: u64,
    pub issues: Vec<ScrapeIssue>,
}

/// One entry's outcome, for [`ScrapeProgressEvent`] — a superset of what
/// [`BulkScrapeReport`]'s counters track (adds `Skipped` so `processed`
/// reliably reaches `total` even for entries the report only ever counts in
/// bulk, e.g. every video when no TMDb key is configured).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrapeOutcome {
    Matched,
    NotFound,
    Failed,
    Skipped,
}

/// Live per-entry progress, emitted as each entry (or, for a music album
/// group, each track within it) finishes — carries the freshly-written
/// `scraped_title`/`genres`/`cast` directly (not just a "something changed"
/// signal) so a listener can patch its own rendering immediately, with no
/// extra round trip back to the library for the data that just changed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ScrapeProgressEvent {
    pub entry_key: String,
    /// The entry's original (path-derived) title — stable identification
    /// even before/without a scrape match, unlike `scraped_title` below.
    pub title: String,
    pub processed: u64,
    pub total: u64,
    pub outcome: ScrapeOutcome,
    /// Set for `NotFound`/`Failed` — the same human-readable reason
    /// [`ScrapeIssue`] carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Set for a `Matched` movie/episode only — tracks never get a
    /// `scraped_title` (see `scrape_one_album_group`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scraped_title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<CastMember>,
}

/// Optional live progress side-channel for [`run_bulk_scrape`] — a plain
/// `mpsc` sender, not a Tauri type, so this crate stays usable by the
/// headless daemon with zero UI dependency; the GUI layer is what turns
/// received events into `app.emit(...)` calls. `total` is fixed once at
/// construction (the entry count already known before the loop starts);
/// `processed` counts up via an atomic since [`ScrapeProgress::emit`] is
/// called from sequential `.await` points, not concurrently, but a plain
/// `Cell` wouldn't be `Send` across them.
pub struct ScrapeProgress {
    sender: UnboundedSender<ScrapeProgressEvent>,
    total: u64,
    processed: AtomicU64,
}

impl ScrapeProgress {
    fn new(sender: UnboundedSender<ScrapeProgressEvent>, total: u64) -> Self {
        Self {
            sender,
            total,
            processed: AtomicU64::new(0),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        entry_key: &str,
        title: &str,
        outcome: ScrapeOutcome,
        reason: Option<String>,
        scraped_title: Option<String>,
        genres: Vec<String>,
        cast: Vec<CastMember>,
    ) {
        let processed = self.processed.fetch_add(1, Ordering::Relaxed) + 1;
        // A closed receiver (nothing currently listening, or the listener
        // was dropped) just means no one's watching this run — never fail
        // the scrape itself over it.
        let _ = self.sender.send(ScrapeProgressEvent {
            entry_key: entry_key.to_string(),
            title: title.to_string(),
            processed,
            total: self.total,
            outcome,
            reason,
            scraped_title,
            genres,
            cast,
        });
    }

    fn matched(
        &self,
        entry_key: &str,
        title: &str,
        scraped_title: Option<String>,
        genres: Vec<String>,
        cast: Vec<CastMember>,
    ) {
        self.emit(
            entry_key,
            title,
            ScrapeOutcome::Matched,
            None,
            scraped_title,
            genres,
            cast,
        );
    }

    fn issue(&self, entry_key: &str, title: &str, outcome: ScrapeOutcome, reason: String) {
        self.emit(
            entry_key,
            title,
            outcome,
            Some(reason),
            None,
            vec![],
            vec![],
        );
    }

    fn skipped(&self, entry_key: &str, title: &str) {
        self.emit(
            entry_key,
            title,
            ScrapeOutcome::Skipped,
            None,
            None,
            vec![],
            vec![],
        );
    }
}

pub async fn run_bulk_scrape(
    library: &Library,
    roots: &SharedRootResolver,
    config: &ScrapeConfig,
    cancel: &AtomicBool,
    progress_tx: Option<UnboundedSender<ScrapeProgressEvent>>,
    force: bool,
) -> sqlx::Result<BulkScrapeReport> {
    if !force {
        let repaired = artwork::reconcile_references(library, roots).await?;
        if repaired.recovered > 0 || repaired.cleared > 0 {
            tracing::info!(
                recovered = repaired.recovered,
                cleared = repaired.cleared,
                "reconciled artwork references before missing-only scrape"
            );
        }
    }
    // `force` re-scrapes everything, overwriting whatever's already there —
    // `scrape_videos`/`scrape_tracks` below always overwrite unconditionally
    // per entry regardless of prior state, so simply widening which entries
    // are handed in is the whole difference; no separate "overwrite" branch
    // needed in the per-entry scraping logic itself.
    let entries = if force {
        library.list().await?
    } else {
        library.incomplete_scrape().await?
    };
    let (videos, tracks): (Vec<EntryRecord>, Vec<EntryRecord>) = entries
        .into_iter()
        .partition(|e| matches!(e.kind, MediaKind::Movie | MediaKind::Episode));
    let lyric_tracks = if force {
        tracks.clone()
    } else {
        library.missing_track_lyrics().await?
    };
    let progress =
        progress_tx.map(|sender| ScrapeProgress::new(sender, (videos.len() + tracks.len()) as u64));

    // Movies/episodes (TMDb) and music (MusicBrainz/Cover Art Archive/
    // Wikimedia) hit entirely independent external services with
    // independent rate limits, so run them concurrently rather than
    // sequentially — one waiting on a TMDb response no longer blocks the
    // other's MusicBrainz request (and vice versa). `tokio::join!` (not
    // `spawn`) is enough for this: these are I/O-bound, so cooperative
    // concurrency on one task already lets both make progress while the
    // other's request is in flight, and it avoids the `Send`/`'static`
    // bounds `spawn` would force onto every borrowed argument here. A
    // shared `&mut BulkScrapeReport` isn't expressible across two
    // concurrently-polled futures, so each accumulates its own and the two
    // are merged once both finish; `scrape_tracks` no longer needs the
    // explicit pre-flight cancel check the old sequential call had — its
    // own loop already checks `cancel` before every iteration, so a
    // cancellation set during the video half still stops the track half on
    // its very first iteration either way.
    let mut video_report = BulkScrapeReport::default();
    let mut track_report = BulkScrapeReport::default();
    let (video_result, track_result) = tokio::join!(
        scrape_videos(
            library,
            roots,
            config,
            &videos,
            cancel,
            &mut video_report,
            progress.as_ref(),
            force,
        ),
        scrape_tracks(
            library,
            roots,
            config,
            TrackScrapeEntries {
                albums: &tracks,
                lyrics: &lyric_tracks
            },
            cancel,
            &mut track_report,
            progress.as_ref(),
            force,
        ),
    );
    video_result?;
    track_result?;

    Ok(BulkScrapeReport {
        matched: video_report.matched + track_report.matched,
        not_found: video_report.not_found + track_report.not_found,
        failed: video_report.failed + track_report.failed,
        skipped: video_report.skipped + track_report.skipped,
        issues: [video_report.issues, track_report.issues].concat(),
    })
}

/// Scene-release tags that can still arrive from embedded container titles
/// or filenames without a release-year boundary, and choke TMDb's search
/// matcher when left in the query: confirmed live
/// against the real API that `"10 Cloverfield Lane 1080p BluRay x264"`
/// returns zero results while `"10 Cloverfield Lane"` matches immediately.
/// Not a stored-title change — this only affects the string sent to TMDb
/// search, so it can't touch anything `classify.rs`'s own tests already
/// cover. Truncates the query at the first case-insensitive occurrence of
/// any of these (a token boundary — not a raw substring match, so a real
/// title that happens to contain one of these strings as part of a real
/// word, e.g. wouldn't false-positive on a partial match inside a longer
/// word) and trims what's left.
const SEARCH_QUERY_NOISE_TOKENS: &[&str] = &[
    "480p",
    "720p",
    "1080p",
    "2160p",
    "4k",
    "bluray",
    "blu-ray",
    "webrip",
    "web-dl",
    "webdl",
    "hdtv",
    "dvdrip",
    "brrip",
    "bdrip",
    "x264",
    "x265",
    "h264",
    "h265",
    "hevc",
    "avc",
    "xvid",
    "10bit",
    "8bit",
    "ddp5",
    "ddp",
    "dts",
    "aac",
    "ac3",
    "atmos",
    // Scene-release qualifier tags — describe the *release*, not the movie,
    // but (unlike the codec/resolution tags above) don't reliably follow
    // right after the title: "Proper" can sit between the title and the
    // resolution tag (e.g. "28 Years Later Proper 1080p..."), which used to
    // survive into the search query and broke an otherwise-correct match.
    "proper",
    "repack",
    "rerip",
    "internal",
    "limited",
    "unrated",
    "extended",
    "remastered",
    "theatrical",
    "directors.cut",
    "uncut",
    "retail",
    "readnfo",
    "nfofix",
    "subbed",
    "dubbed",
    // Streaming-service source tags, same reasoning.
    "amzn",
    "nf",
    "dsnp",
    "hulu",
    "hmax",
    "atvp",
    "pcok",
    // Release/distribution labels found in real failed rows. TMDb returns
    // no results for e.g. "Dawn of the Dead Arrow" or "Mortal Kombat II
    // NORDIC", while the title immediately before the label matches.
    "arrow",
    "nordic",
    // Polish release shorthand (lektor polski), confirmed in a real
    // filename: "Mumia LP r d11" fails while "Mumia" + year 2017 resolves
    // TMDb movie 282035.
    "lp",
];

/// A bare `WIDTHxHEIGHT` pixel-dimension tag (`1920x812`, `1280X720`) — the
/// actual encoded frame size, commonly seen on BDRip encodes that crop
/// letterbox bars off a standard resolution (`1920x812` rather than a named
/// `1080p`/`720p` tag). Confirmed live: `"Battle For The Planet Of The Apes
/// 1920x812 BDRip x264 DTS-HD MA"` returned zero TMDb results with the tag
/// attached. Not in [`SEARCH_QUERY_NOISE_TOKENS`] since it isn't one fixed
/// string — any width/height pair can appear — so it needs its own
/// whole-word pattern check instead of a literal-list entry.
fn is_pixel_dimension_word(word: &str) -> bool {
    let trimmed = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let Some(x_pos) = trimmed.find(['x', 'X']) else {
        return false;
    };
    let is_digit_run = |s: &str| (2..=4).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    is_digit_run(&trimmed[..x_pos]) && is_digit_run(&trimmed[x_pos + 1..])
}

/// See [`SEARCH_QUERY_NOISE_TOKENS`]. Splits on whitespace, drops every
/// token from the first noise token onward, and rejoins — cheap and
/// dependency-free, matching this module's existing hand-rolled style.
fn search_query_for(title: &str) -> String {
    // Scene names commonly use dots/underscores as word separators. Tags
    // read from the container can preserve those verbatim even when the
    // filename classifier has already normalized them.
    let normalized = title.replace(['.', '_'], " ");
    // Some release names append a human-readable payload after a spaced
    // dash, e.g. "Thirteen (Thir13en) Ghosts - Horror 2001 Eng Subs".
    // Only cut at the dash when that suffix contains both a year and a
    // recognizable release descriptor; a real title such as
    // "Mission: Impossible - Ghost Protocol" remains untouched.
    let release_suffix_tokens = [
        "horror",
        "eng",
        "english",
        "subs",
        "multi-subs",
        "bluray",
        "brrip",
        "webrip",
        "720p",
        "1080p",
        "2160p",
    ];
    let candidate = normalized
        .match_indices(" - ")
        .find_map(|(index, _)| {
            let suffix = &normalized[index + 3..];
            let has_year = suffix.split(|c: char| !c.is_ascii_digit()).any(|part| {
                part.len() == 4
                    && part
                        .parse::<u32>()
                        .is_ok_and(|year| (1900..=2099).contains(&year))
            });
            let has_release_descriptor = suffix.split_whitespace().any(|word| {
                let lower = word.to_lowercase();
                release_suffix_tokens.contains(&lower.trim_matches(|c: char| !c.is_alphanumeric()))
            });
            (has_year && has_release_descriptor).then_some(normalized[..index].trim())
        })
        .unwrap_or(normalized.trim());
    let words = candidate.split_whitespace().collect::<Vec<_>>();
    let mut kept = Vec::new();
    for (index, word) in words.iter().enumerate() {
        let lower = word.to_lowercase();
        let compound_edition = matches!(lower.as_str(), "director" | "directors" | "director's")
            && words
                .get(index + 1)
                .is_some_and(|next| next.eq_ignore_ascii_case("cut"));
        if compound_edition
            || is_pixel_dimension_word(&lower)
            || SEARCH_QUERY_NOISE_TOKENS
                .iter()
                .any(|tag| lower == *tag || lower.trim_end_matches(['.', ',']) == *tag)
        {
            break;
        }
        kept.push(*word);
    }
    if kept.is_empty() {
        title.to_string()
    } else {
        kept.join(" ")
    }
}

/// A year embedded in a container title is still useful when the cleaner
/// filename omitted it. Do not treat an all-numeric title (notably the real
/// films "1917" and "1984") as its own release year.
///
/// A 4-digit run immediately followed by `x`/`X` and another digit run is a
/// `WIDTHxHEIGHT` pixel-dimension tag (`1920x812`), not a year, even though
/// the width half often happens to fall in the plausible year range —
/// confirmed live: `"...1920x812 BDRip..."` was being read as year 1920,
/// which then hard-filtered TMDb's search to zero results. Both halves of
/// the dimension tag are skipped so a real year elsewhere in the title is
/// still found.
fn embedded_year_hint(title: &str) -> Option<u32> {
    if title.trim().chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let bytes = title.as_bytes();
    let mut years = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        let followed_by_dimension_height = end < bytes.len()
            && matches!(bytes[end], b'x' | b'X')
            && bytes.get(end + 1).is_some_and(u8::is_ascii_digit);
        if followed_by_dimension_height {
            let mut height_end = end + 1;
            while height_end < bytes.len() && bytes[height_end].is_ascii_digit() {
                height_end += 1;
            }
            i = height_end;
            continue;
        }
        if end - start == 4 {
            if let Ok(year) = title[start..end].parse::<u32>() {
                if (1900..=2099).contains(&year) {
                    years.push(year);
                }
            }
        }
        i = end;
    }
    years.into_iter().next_back()
}

fn remove_year_from_query(query: &str, year: Option<u32>) -> String {
    let Some(year) = year else {
        return query.to_string();
    };
    let year = year.to_string();
    let stripped = query
        .split_whitespace()
        .filter(|word| word.trim_matches(|c: char| !c.is_ascii_digit()) != year)
        .collect::<Vec<_>>()
        .join(" ");
    if stripped.is_empty() {
        query.to_string()
    } else {
        stripped
    }
}

/// Build conservative movie-search fallbacks from two independent sources:
/// the embedded display title and the filename-derived classifier result.
/// Container tags are often excellent, but real files also contain junk tags
/// such as `JAWS_t02 mkv-muxed (1)` while their filenames are clean. Trying
/// the path-derived query only after a definitive no-match preserves useful
/// tags without allowing one bad tag to permanently hide a valid movie.
fn movie_search_candidates(entry: &EntryRecord) -> Vec<(String, Option<u32>)> {
    let embedded_year = entry.year.or_else(|| embedded_year_hint(&entry.title));
    let mut candidates = Vec::new();
    let mut push = |title: &str, year: Option<u32>| {
        let query = remove_year_from_query(&search_query_for(title), year);
        if !query.trim().is_empty()
            && !candidates
                .iter()
                .any(|(existing, existing_year): &(String, Option<u32>)| {
                    existing.eq_ignore_ascii_case(&query) && *existing_year == year
                })
        {
            candidates.push((query, year));
        }
    };
    push(&entry.title, embedded_year);
    if let Some(path_entry) = classify::classify(&entry.relative_path) {
        push(&path_entry.title, path_entry.year.or(embedded_year));
    }
    candidates
}

async fn search_movie_for_entry(
    tmdb: &TmdbClient,
    entry: &EntryRecord,
) -> Result<ScrapedVideo, TmdbError> {
    for (query, year) in movie_search_candidates(entry) {
        match tmdb.search_and_fetch_movie(&query, year).await {
            Err(TmdbError::NotFound) => continue,
            result => return result,
        }
    }
    Err(TmdbError::NotFound)
}

async fn cache_introdb_segments(
    library: &Library,
    introdb: &IntroDbClient,
    entry: &EntryRecord,
    tmdb_id: u64,
) -> sqlx::Result<()> {
    // TheIntroDB's TV identity requires a positive season and episode.
    // Specials/unstructured files cannot be looked up reliably, but an
    // empty cached result still records that no valid query was possible.
    let identity = match entry.kind {
        MediaKind::Movie => Some((None, None)),
        MediaKind::Episode => match (entry.season, entry.episode) {
            (Some(season), Some(episode)) if season > 0 && episode > 0 => {
                Some((Some(season), Some(episode)))
            }
            _ => None,
        },
        MediaKind::Track => None,
    };
    let Some((season, episode)) = identity else {
        return library
            .set_introdb_segments(&entry.entry_key, tmdb_id, &[])
            .await;
    };
    match introdb
        .segments(tmdb_id, season, episode, entry.duration_secs)
        .await
    {
        Ok(segments) => {
            library
                .set_introdb_segments(&entry.entry_key, tmdb_id, &segments)
                .await
        }
        Err(IntroDbError::Unavailable(reason)) => {
            tracing::warn!(entry = %entry.entry_key, %reason, "introdb unavailable, will retry next run");
            library.mark_introdb_retry(&entry.entry_key).await
        }
    }
}

/// TMDb specials are season 0, but local DVD extras usually have no numeric
/// episode number. Match their filename title to TMDb's season-0 episode
/// names, retaining the episode metadata instead of displaying the show's
/// title for every special. Compacting punctuation also handles common
/// filename variants such as "Sit-down" vs "Sitdown".
fn compact_episode_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn match_tmdb_episode_title<'a>(
    title: &str,
    season_number: Option<u32>,
    episode_number: Option<u32>,
    season: &'a ScrapedSeason,
) -> Option<&'a ScrapedEpisode> {
    if let Some(number) = episode_number {
        return season.episode_details.get(&number);
    }
    if season_number != Some(0) {
        return None;
    }
    let local = compact_episode_title(title);
    if local.len() < 8 {
        return None;
    }
    season.episode_details.values().find(|episode| {
        let remote = compact_episode_title(&episode.title);
        !remote.is_empty()
            && (remote == local || remote.starts_with(&local) || remote.ends_with(&local))
    })
}

fn match_tmdb_episode<'a>(
    entry: &EntryRecord,
    season: &'a ScrapedSeason,
) -> Option<&'a ScrapedEpisode> {
    match_tmdb_episode_title(&entry.title, entry.season, entry.episode, season)
}

#[allow(clippy::too_many_arguments)]
async fn scrape_videos(
    library: &Library,
    roots: &SharedRootResolver,
    config: &ScrapeConfig,
    entries: &[EntryRecord],
    cancel: &AtomicBool,
    report: &mut BulkScrapeReport,
    progress: Option<&ScrapeProgress>,
    force: bool,
) -> sqlx::Result<()> {
    let Some(api_key) = &config.tmdb_api_key else {
        report.skipped += entries.len() as u64;
        if let Some(p) = progress {
            for entry in entries {
                p.skipped(&entry.entry_key, &entry.title);
            }
        }
        return Ok(());
    };
    let tmdb = match (&config.tmdb_api_base, &config.tmdb_image_base) {
        (Some(api_base), Some(image_base)) => {
            TmdbClient::with_base_urls(api_key.clone(), api_base, image_base)
        }
        _ => TmdbClient::new(api_key.clone()),
    };
    let introdb = config
        .introdb_api_base
        .as_deref()
        .map(IntroDbClient::with_base_url)
        .unwrap_or_else(IntroDbClient::new);
    // One TMDb TV lookup per show, shared across every episode entry —
    // avoids N calls for an N-episode season.
    let mut tv_cache: HashMap<String, Result<ScrapedVideo, TmdbError>> = HashMap::new();
    // Season details contain both the season poster and every episode still,
    // so one request per (show, season) is sufficient even for a full season.
    let mut tv_season_cache: HashMap<(u64, u32), Result<ScrapedSeason, TmdbError>> = HashMap::new();

    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let outcome = match entry.kind {
            MediaKind::Movie => search_movie_for_entry(&tmdb, entry).await,
            MediaKind::Episode => {
                let raw_query = entry
                    .show_title
                    .clone()
                    .unwrap_or_else(|| entry.title.clone());
                let query = search_query_for(&raw_query);
                let key = query.to_lowercase();
                if !tv_cache.contains_key(&key) {
                    let result = tmdb.search_and_fetch_tv(&query).await;
                    tv_cache.insert(key.clone(), result);
                }
                match tv_cache.get(&key).expect("just inserted").clone() {
                    Ok(mut scraped) => {
                        if let Some(season_number) = entry.season {
                            let season_key = (scraped.tmdb_id, season_number);
                            let season_result =
                                if let Some(cached) = tv_season_cache.get(&season_key) {
                                    cached.clone()
                                } else {
                                    let result =
                                        tmdb.season_details(scraped.tmdb_id, season_number).await;
                                    tv_season_cache.insert(season_key, result.clone());
                                    result
                                };
                            match season_result {
                                Ok(season) => {
                                    scraped.season_poster_url = season.poster_url.clone();
                                    if let Some(episode) = match_tmdb_episode(entry, &season) {
                                        scraped.episode_title = Some(episode.title.clone());
                                        scraped.episode_overview = episode.overview.clone();
                                        if let Some(still) = &episode.still_url {
                                            scraped.backdrop_url = Some(still.clone());
                                        }
                                    }
                                    Ok(scraped)
                                }
                                // Some libraries contain specials or custom
                                // season numbering that TMDB does not. Keep
                                // the valid show-level match/artwork instead
                                // of turning that entry into a false no-match.
                                Err(TmdbError::NotFound) => Ok(scraped),
                                Err(error) => Err(error),
                            }
                        } else {
                            Ok(scraped)
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            MediaKind::Track => unreachable!("videos partition excludes tracks"),
        };
        match outcome {
            Ok(scraped) => {
                library
                    .set_scrape_result(
                        &entry.entry_key,
                        Some(&scraped.title),
                        &scraped.genres,
                        &scraped.cast,
                        scraped.certification.as_deref(),
                        scraped.community_rating,
                        scraped.community_rating_votes,
                    )
                    .await?;
                library
                    .set_episode_title(&entry.entry_key, scraped.episode_title.as_deref())
                    .await?;
                cache_introdb_segments(library, &introdb, entry, scraped.tmdb_id).await?;
                if let Some(overview) = scraped
                    .episode_overview
                    .as_deref()
                    .or(scraped.overview.as_deref())
                {
                    library.set_overview(&entry.entry_key, overview).await?;
                }
                if let Some(url) = &scraped.poster_url {
                    save_video_artwork(
                        library,
                        roots,
                        entry,
                        ArtworkKind::Poster,
                        "poster",
                        url,
                        force,
                    )
                    .await;
                }
                if let Some(url) = &scraped.season_poster_url {
                    save_video_artwork(
                        library,
                        roots,
                        entry,
                        ArtworkKind::SeasonPoster,
                        "season-poster",
                        url,
                        force,
                    )
                    .await;
                }
                if let Some(url) = &scraped.backdrop_url {
                    save_video_artwork(
                        library,
                        roots,
                        entry,
                        ArtworkKind::Backdrop,
                        "backdrop",
                        url,
                        force,
                    )
                    .await;
                }
                report.matched += 1;
                if let Some(p) = progress {
                    p.matched(
                        &entry.entry_key,
                        &entry.title,
                        Some(scraped.title.clone()),
                        scraped.genres.clone(),
                        scraped.cast.clone(),
                    );
                }
            }
            Err(TmdbError::NotFound) => {
                library
                    .set_scrape_result(&entry.entry_key, None, &[], &[], None, None, None)
                    .await?;
                library.clear_introdb_segments(&entry.entry_key).await?;
                report.not_found += 1;
                let reason = "no match found on TMDb".to_string();
                report.issues.push(ScrapeIssue {
                    entry_key: entry.entry_key.clone(),
                    title: entry.title.clone(),
                    reason: reason.clone(),
                    kind: entry.kind,
                });
                if let Some(p) = progress {
                    p.issue(
                        &entry.entry_key,
                        &entry.title,
                        ScrapeOutcome::NotFound,
                        reason,
                    );
                }
            }
            Err(TmdbError::Unavailable(reason)) => {
                tracing::warn!(entry = %entry.entry_key, %reason, "tmdb unavailable, will retry next run");
                report.failed += 1;
                report.issues.push(ScrapeIssue {
                    entry_key: entry.entry_key.clone(),
                    title: entry.title.clone(),
                    reason: reason.clone(),
                    kind: entry.kind,
                });
                if let Some(p) = progress {
                    p.issue(
                        &entry.entry_key,
                        &entry.title,
                        ScrapeOutcome::Failed,
                        reason,
                    );
                }
            }
        }
    }
    Ok(())
}

/// True when `kind` already has a recorded path for this entry *and* that
/// file is still on disk — the condition under which a non-force scrape
/// should leave it alone rather than re-fetching from TMDb/MusicBrainz/Cover
/// Art Archive. A recorded path whose file has gone missing is treated as
/// absent so it still gets repaired.
async fn artwork_already_present(
    library: &Library,
    roots: &SharedRootResolver,
    entry_key: &str,
    kind: ArtworkKind,
) -> bool {
    match library.artwork(entry_key, kind).await {
        Ok(Some((path, _version))) => artwork::exists(roots, &path).await,
        _ => false,
    }
}

async fn save_video_artwork(
    library: &Library,
    roots: &SharedRootResolver,
    entry: &EntryRecord,
    kind: ArtworkKind,
    label: &str,
    url: &str,
    force: bool,
) {
    if !force && artwork_already_present(library, roots, &entry.entry_key, kind).await {
        return;
    }
    let Ok(bytes) = download_bytes(url).await else {
        return;
    };
    let filename = format!(
        "{}-tmdb-{label}.jpg",
        artwork::sanitize_stem(artwork::file_stem(&entry.relative_path))
    );
    if let Ok(relative) =
        artwork::save_artwork(roots, &entry.relative_path, &filename, &bytes).await
    {
        let _ = library.set_artwork(&entry.entry_key, kind, &relative).await;
    }
}

/// The three music-scraper clients bundled together purely to keep
/// [`scrape_one_album_group`]'s argument count sane — always constructed
/// and passed as one unit.
struct MusicScrapers {
    mb: MusicBrainzClient,
    coverart: CoverArtClient,
    wikimedia: WikimediaClient,
    lrclib: LrclibClient,
}

struct TrackScrapeEntries<'a> {
    albums: &'a [EntryRecord],
    lyrics: &'a [EntryRecord],
}

impl MusicScrapers {
    fn from_config(config: &ScrapeConfig) -> Self {
        Self {
            mb: config
                .musicbrainz_base
                .as_deref()
                .map_or_else(MusicBrainzClient::new, MusicBrainzClient::with_base_url),
            coverart: config
                .coverart_base
                .as_deref()
                .map_or_else(CoverArtClient::new, CoverArtClient::with_base_url),
            wikimedia: config
                .wikimedia_base
                .as_deref()
                .map_or_else(WikimediaClient::new, WikimediaClient::with_base_url),
            lrclib: config
                .lrclib_base
                .as_deref()
                .map_or_else(LrclibClient::new, LrclibClient::with_base_url),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn scrape_tracks(
    library: &Library,
    roots: &SharedRootResolver,
    config: &ScrapeConfig,
    entries: TrackScrapeEntries<'_>,
    cancel: &AtomicBool,
    report: &mut BulkScrapeReport,
    progress: Option<&ScrapeProgress>,
    force: bool,
) -> sqlx::Result<()> {
    let scrapers = MusicScrapers::from_config(config);

    let mut groups: HashMap<(String, String), Vec<&EntryRecord>> = HashMap::new();
    for entry in entries.albums {
        match (&entry.artist, &entry.album) {
            (Some(artist), Some(album)) if !artist.is_empty() && !album.is_empty() => {
                groups
                    .entry((artist.clone(), album.clone()))
                    .or_default()
                    .push(entry);
            }
            _ => {
                report.skipped += 1;
                if let Some(p) = progress {
                    p.skipped(&entry.entry_key, &entry.title);
                }
            }
        }
    }

    let mut musicbrainz_outage = None;
    for ((artist, album), group) in groups {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(reason) = musicbrainz_outage.as_deref() {
            record_musicbrainz_failure(&group, reason, report, progress);
            continue;
        }
        if let Some(reason) = scrape_one_album_group(
            library, roots, &scrapers, &artist, &album, &group, report, progress, force,
        )
        .await?
        {
            tracing::warn!(
                %artist,
                %album,
                %reason,
                "musicbrainz unavailable; pausing MusicBrainz requests until the next scrape run"
            );
            musicbrainz_outage = Some(reason);
        }
    }
    scrape_track_lyrics(library, &scrapers.lrclib, entries.lyrics, cancel).await?;
    Ok(())
}

/// Fetch lyrics one track at a time. LRCLIB's exact endpoint uses duration
/// alongside tags, so this runs after scanning/ffprobe and does not guess
/// when any required field is absent. Provider outages are retryable; a
/// definitive 404 is cached as a no-match marker.
async fn scrape_track_lyrics(
    library: &Library,
    lrclib: &LrclibClient,
    entries: &[EntryRecord],
    cancel: &AtomicBool,
) -> sqlx::Result<()> {
    let mut made_request = false;
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (Some(artist), Some(album), Some(duration_secs)) = (
            entry.artist.as_deref(),
            entry.album.as_deref(),
            entry.duration_secs,
        ) else {
            continue;
        };
        if artist.is_empty() || album.is_empty() || duration_secs <= 0.0 {
            continue;
        }
        // LRCLIB explicitly asks batch clients to leave 200–500 ms between
        // sequential requests. This remains intentionally serial and keeps
        // a full-library scrape considerate of the free public service.
        if made_request {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        made_request = true;
        match lrclib
            .lookup(&entry.title, artist, album, duration_secs)
            .await
        {
            Ok(lyrics) => library.set_track_lyrics(&entry.entry_key, &lyrics).await?,
            Err(LrclibError::NotFound) => {
                library
                    .mark_track_lyrics_not_found(&entry.entry_key)
                    .await?
            }
            Err(LrclibError::RateLimited(retry_after)) => {
                tracing::warn!(entry_key = %entry.entry_key, title = %entry.title, ?retry_after, "lrclib rate limited the scrape; honoring Retry-After");
                tokio::time::sleep(retry_after).await;
            }
            Err(LrclibError::Unavailable(reason)) => {
                tracing::warn!(entry_key = %entry.entry_key, title = %entry.title, %reason, "lrclib unavailable, will retry next run");
            }
            Err(LrclibError::BadRequest(reason)) => {
                // A 400 is deterministic for this exact request — retrying it
                // every scrape pass forever just re-runs the same rejected
                // lookup (#230). Cache it the same way as a definitive
                // no-match so it is naturally retried after the same 30-day
                // cooldown, in case it was actually a transient upstream bug.
                tracing::warn!(entry_key = %entry.entry_key, title = %entry.title, %reason, "lrclib rejected the lookup; caching as no-match");
                library
                    .mark_track_lyrics_not_found(&entry.entry_key)
                    .await?
            }
        }
    }
    Ok(())
}

/// Cleans a classify()-derived album name before sending it to
/// MusicBrainz's search — real libraries embed a release year, the
/// artist's own name, and/or catalog-number/edition metadata directly in
/// the album folder name (`"2004 - No Silence (AVTCD-95770)"`,
/// `"Cosmic Gate - The Drums (7243 886923 2 8)"`), none of which
/// MusicBrainz's search can match against as-is. Confirmed live against the
/// real MusicBrainz API for both patterns: the raw folder names found zero
/// results; the cleaned forms ("No Silence", "The Drums") found the correct
/// release, in the second case only after *also* stripping the redundant
/// leading artist name. Strips, in order: a leading `NNNN - ` year prefix,
/// a leading `"<artist> - "` prefix matching `artist` (case-insensitive —
/// real libraries duplicate the artist name into the album folder more
/// often once the year prefix is already gone), then repeatedly strips
/// trailing `(...)`/`[...]` groups (catalog numbers, edition/format tags
/// like `[2CD]` often stack more than one). Falls back to the original text
/// if cleaning would leave nothing — same "never send an empty query"
/// guard as [search_query_for].
fn search_query_for_album(artist: &str, album: &str) -> String {
    let mut s = album.trim();
    let bytes = s.as_bytes();
    if bytes.len() > 4 && bytes[..4].iter().all(u8::is_ascii_digit) {
        if let Some(after_dash) = s[4..].trim_start().strip_prefix('-') {
            s = after_dash.trim_start();
        }
    }
    if s.len() > artist.len() && s[..artist.len()].eq_ignore_ascii_case(artist) {
        if let Some(after_dash) = s[artist.len()..].trim_start().strip_prefix('-') {
            s = after_dash.trim_start();
        }
    }
    loop {
        let trimmed = s.trim_end();
        if let Some(rest) = trimmed.strip_suffix(')') {
            if let Some(open) = rest.rfind('(') {
                s = rest[..open].trim_end();
                continue;
            }
        }
        if let Some(rest) = trimmed.strip_suffix(']') {
            if let Some(open) = rest.rfind('[') {
                s = rest[..open].trim_end();
                continue;
            }
        }
        s = trimmed;
        break;
    }
    let cleaned = s.trim_end_matches('-').trim();
    if cleaned.is_empty() {
        album.to_string()
    } else {
        cleaned.to_string()
    }
}

fn local_cover_score(filename: &str) -> Option<u8> {
    let path = Path::new(filename);
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
    let tokens: Vec<&str> = stem
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    if [
        "back", "gatefold", "insert", "booklet", "inside", "label", "disc", "cd",
    ]
    .iter()
    .any(|marker| tokens.contains(marker))
    {
        return None;
    }
    if matches!(stem.as_str(), "cover" | "folder" | "front" | "album") {
        return Some(100);
    }
    if stem.contains("front") || stem.contains("cover") || stem.contains("folder") {
        return Some(90);
    }
    Some(10)
}

/// Import a conventional image already stored beside an album's tracks.
/// Vinyl/CD rips commonly ship a front scan plus back/gatefold/insert scans;
/// prefer an explicit front/cover name and reject those non-front variants.
/// The selected bytes are copied into SWARM's managed `images/` location so
/// later moves/reconciliation follow the same path convention as downloaded
/// Cover Art Archive artwork.
async fn import_local_album_cover(
    roots: &SharedRootResolver,
    entry: &EntryRecord,
) -> Option<String> {
    let relative_parent = Path::new(&entry.relative_path).parent()?;
    let relative_parent = relative_parent.to_str()?;
    let absolute_parent = roots.resolve_existing_dir(relative_parent);
    let mut directory = tokio::fs::read_dir(absolute_parent).await.ok()?;
    let mut candidates = Vec::new();
    while let Ok(Some(candidate)) = directory.next_entry().await {
        let Ok(file_type) = candidate.file_type().await else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let filename = candidate.file_name().to_string_lossy().into_owned();
        let Some(score) = local_cover_score(&filename) else {
            continue;
        };
        candidates.push((score, filename.to_ascii_lowercase(), candidate.path()));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let (_, _, source) = candidates.into_iter().next()?;
    let extension = source.extension()?.to_str()?.to_ascii_lowercase();
    let bytes = tokio::fs::read(source).await.ok()?;
    artwork::save_artwork(
        roots,
        &entry.relative_path,
        &format!("album-cover.{extension}"),
        &bytes,
    )
    .await
    .ok()
}

/// One (artist, album) group's worth of MusicBrainz + Cover Art Archive +
/// Wikimedia work — release-level data applied to every track in the group
/// (see the module docs on why this is per-album, not per-track). Shared by
/// the bulk loop above and [`scrape_one_track`]'s pinpoint path so a single
/// manually-triggered rescrape of one track re-syncs its whole album, same
/// as a bulk run would.
#[allow(clippy::too_many_arguments)]
async fn scrape_one_album_group(
    library: &Library,
    roots: &SharedRootResolver,
    scrapers: &MusicScrapers,
    artist: &str,
    album: &str,
    group: &[&EntryRecord],
    report: &mut BulkScrapeReport,
    progress: Option<&ScrapeProgress>,
    force: bool,
) -> sqlx::Result<Option<String>> {
    let MusicScrapers {
        mb,
        coverart,
        wikimedia,
        ..
    } = scrapers;

    // Local front-cover scans are useful even when MusicBrainz has no match
    // or is temporarily unavailable. Attach one once per album before the
    // provider lookup; a forced scrape may still replace it with provider art.
    if let Some(first) = group.first() {
        let has_cover =
            artwork_already_present(library, roots, &first.entry_key, ArtworkKind::Cover).await;
        if !has_cover {
            if let Some(relative) = import_local_album_cover(roots, first).await {
                for track in group {
                    library
                        .set_artwork(&track.entry_key, ArtworkKind::Cover, &relative)
                        .await?;
                }
            }
        }
    }

    match mb
        .search_release(artist, &search_query_for_album(artist, album))
        .await
    {
        Err(MbError::NotFound) => {
            for track in group {
                library
                    .set_scrape_result(&track.entry_key, None, &[], &[], None, None, None)
                    .await?;
                let reason =
                    format!("no match found on MusicBrainz for \"{artist} \u{2013} {album}\"");
                report.issues.push(ScrapeIssue {
                    entry_key: track.entry_key.clone(),
                    title: track.title.clone(),
                    reason: reason.clone(),
                    kind: track.kind,
                });
                if let Some(p) = progress {
                    p.issue(
                        &track.entry_key,
                        &track.title,
                        ScrapeOutcome::NotFound,
                        reason.clone(),
                    );
                }
            }
            report.not_found += group.len() as u64;
        }
        Err(MbError::Unavailable(reason)) => {
            record_musicbrainz_failure(group, &reason, report, progress);
            return Ok(Some(reason));
        }
        Ok(release_mbid) => {
            let details = match mb.release_lookup(&release_mbid).await {
                Ok(details) => Some(details),
                Err(MbError::NotFound) => None,
                Err(MbError::Unavailable(reason)) => {
                    record_musicbrainz_failure(group, &reason, report, progress);
                    return Ok(Some(reason));
                }
            };
            let genres = details
                .as_ref()
                .map(|d| d.genres.clone())
                .unwrap_or_default();
            for track in group {
                library
                    .set_scrape_result(
                        &track.entry_key,
                        None,
                        &genres,
                        &[],
                        None,
                        details
                            .as_ref()
                            .and_then(|details| details.community_rating),
                        details
                            .as_ref()
                            .and_then(|details| details.community_rating_votes),
                    )
                    .await?;
            }
            report.matched += group.len() as u64;

            if let Some(first) = group.first() {
                let need_cover = force
                    || !artwork_already_present(library, roots, &first.entry_key, ArtworkKind::Cover)
                        .await;
                if need_cover {
                    if let Ok(cover) = coverart.front_cover(&release_mbid).await {
                        if let Ok(relative) = artwork::save_artwork(
                            roots,
                            &first.relative_path,
                            "album-cover.jpg",
                            &cover,
                        )
                        .await
                        {
                            for track in group {
                                let _ = library
                                    .set_artwork(&track.entry_key, ArtworkKind::Cover, &relative)
                                    .await;
                            }
                        }
                    }
                }
                let need_artist_photo = force
                    || !artwork_already_present(
                        library,
                        roots,
                        &first.entry_key,
                        ArtworkKind::ArtistPhoto,
                    )
                    .await;
                if need_artist_photo {
                    if let Some(artist_mbid) = details.as_ref().and_then(|d| d.artist_mbid.as_deref()) {
                        if let Some(bytes) = fetch_artist_photo(mb, wikimedia, artist_mbid).await {
                            if let Ok(relative) = artwork::save_artwork(
                                roots,
                                &first.relative_path,
                                "artist-photo.jpg",
                                &bytes,
                            )
                            .await
                            {
                                for track in group {
                                    let _ = library
                                        .set_artwork(
                                            &track.entry_key,
                                            ArtworkKind::ArtistPhoto,
                                            &relative,
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                }
            }

            // Emitted after every write above (metadata + best-effort
            // artwork) completes, so a listener that reacts to this event by
            // re-fetching artwork bytes actually finds something there.
            if let Some(p) = progress {
                for track in group {
                    p.matched(&track.entry_key, &track.title, None, genres.clone(), vec![]);
                }
            }
        }
    }
    Ok(None)
}

fn record_musicbrainz_failure(
    group: &[&EntryRecord],
    reason: &str,
    report: &mut BulkScrapeReport,
    progress: Option<&ScrapeProgress>,
) {
    for track in group {
        report.issues.push(ScrapeIssue {
            entry_key: track.entry_key.clone(),
            title: track.title.clone(),
            reason: reason.to_string(),
            kind: track.kind,
        });
        if let Some(p) = progress {
            p.issue(
                &track.entry_key,
                &track.title,
                ScrapeOutcome::Failed,
                reason.to_string(),
            );
        }
    }
    report.failed += group.len() as u64;
}

/// Pinpoint (single-entry) video scrape — bypasses `missing_scrape()`
/// entirely (the caller already has the specific `EntryRecord` in hand, via
/// `Library::get`), so this succeeds even on an already-scraped entry: the
/// whole point is correcting a wrong bulk match. `tmdb_override`, when
/// given, skips search and fetches that exact TMDb id/URL directly.
pub async fn scrape_one_video(
    library: &Library,
    roots: &SharedRootResolver,
    config: &ScrapeConfig,
    entry: &EntryRecord,
    tmdb_override: Option<TmdbOverride>,
) -> Result<ScrapedVideo, ScrapeOneError> {
    let api_key = config
        .tmdb_api_key
        .as_ref()
        .ok_or(ScrapeOneError::NoApiKey)?;
    let tmdb = match (&config.tmdb_api_base, &config.tmdb_image_base) {
        (Some(api_base), Some(image_base)) => {
            TmdbClient::with_base_urls(api_key.clone(), api_base, image_base)
        }
        _ => TmdbClient::new(api_key.clone()),
    };
    let introdb = config
        .introdb_api_base
        .as_deref()
        .map(IntroDbClient::with_base_url)
        .unwrap_or_else(IntroDbClient::new);
    let media_type = match entry.kind {
        MediaKind::Movie => "movie",
        MediaKind::Episode => "tv",
        MediaKind::Track => return Err(ScrapeOneError::WrongKind),
    };
    let mut scraped = match tmdb_override {
        Some(over) => {
            let id = over.resolve_id()?;
            tmdb.details_by_id(id, media_type).await?
        }
        None => match entry.kind {
            MediaKind::Movie => search_movie_for_entry(&tmdb, entry).await?,
            MediaKind::Episode => {
                let raw_query = entry
                    .show_title
                    .clone()
                    .unwrap_or_else(|| entry.title.clone());
                tmdb.search_and_fetch_tv(&search_query_for(&raw_query))
                    .await?
            }
            MediaKind::Track => unreachable!("checked above"),
        },
    };
    if entry.kind == MediaKind::Episode {
        if let Some(season_number) = entry.season {
            match tmdb.season_details(scraped.tmdb_id, season_number).await {
                Ok(season) => {
                    scraped.season_poster_url = season.poster_url.clone();
                    if let Some(episode) = match_tmdb_episode(entry, &season) {
                        scraped.episode_title = Some(episode.title.clone());
                        scraped.episode_overview = episode.overview.clone();
                        if let Some(still) = &episode.still_url {
                            scraped.backdrop_url = Some(still.clone());
                        }
                    }
                }
                // Preserve the successfully matched show for custom season
                // numbering that does not exist on TMDB.
                Err(TmdbError::NotFound) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    library
        .set_scrape_result(
            &entry.entry_key,
            Some(&scraped.title),
            &scraped.genres,
            &scraped.cast,
            scraped.certification.as_deref(),
            scraped.community_rating,
            scraped.community_rating_votes,
        )
        .await?;
    library
        .set_episode_title(&entry.entry_key, scraped.episode_title.as_deref())
        .await?;
    cache_introdb_segments(library, &introdb, entry, scraped.tmdb_id).await?;
    if let Some(overview) = scraped
        .episode_overview
        .as_deref()
        .or(scraped.overview.as_deref())
    {
        library.set_overview(&entry.entry_key, overview).await?;
    }
    if let Some(url) = &scraped.poster_url {
        save_video_artwork(
            library,
            roots,
            entry,
            ArtworkKind::Poster,
            "poster",
            url,
            true,
        )
        .await;
    }
    if let Some(url) = &scraped.season_poster_url {
        save_video_artwork(
            library,
            roots,
            entry,
            ArtworkKind::SeasonPoster,
            "season-poster",
            url,
            true,
        )
        .await;
    }
    if let Some(url) = &scraped.backdrop_url {
        save_video_artwork(
            library,
            roots,
            entry,
            ArtworkKind::Backdrop,
            "backdrop",
            url,
            true,
        )
        .await;
    }
    Ok(scraped)
}

/// Re-sync the album metadata and artwork for the whole (artist, album)
/// group `entry` belongs to. This intentionally omits per-track lyrics so
/// artist- and album-level UI actions stay focused and bounded.
pub async fn scrape_one_album(
    library: &Library,
    roots: &SharedRootResolver,
    config: &ScrapeConfig,
    entry: &EntryRecord,
) -> Result<BulkScrapeReport, ScrapeOneError> {
    let artist = entry.artist.as_deref().filter(|a| !a.is_empty());
    let album = entry.album.as_deref().filter(|a| !a.is_empty());
    let (Some(artist), Some(album)) = (artist, album) else {
        return Err(ScrapeOneError::MissingMusicMetadata);
    };
    let siblings = library.entries_by_artist_album(artist, album).await?;
    let group: Vec<&EntryRecord> = siblings.iter().collect();
    let scrapers = MusicScrapers::from_config(config);
    let mut report = BulkScrapeReport::default();
    if let Some(reason) = scrape_one_album_group(
        library,
        roots,
        &scrapers,
        artist,
        album,
        &group,
        &mut report,
        None,
        true,
    )
    .await?
    {
        tracing::warn!(
            %artist,
            %album,
            %reason,
            "musicbrainz unavailable; retrying on the next scrape run"
        );
    }
    Ok(report)
}

/// Pinpoint (single-entry) music scrape: re-syncs the whole album and then
/// refreshes lyrics for every sibling track, matching bulk behavior.
pub async fn scrape_one_track(
    library: &Library,
    roots: &SharedRootResolver,
    config: &ScrapeConfig,
    entry: &EntryRecord,
) -> Result<BulkScrapeReport, ScrapeOneError> {
    let report = scrape_one_album(library, roots, config, entry).await?;
    let artist = entry.artist.as_deref().filter(|a| !a.is_empty());
    let album = entry.album.as_deref().filter(|a| !a.is_empty());
    let (Some(artist), Some(album)) = (artist, album) else {
        return Err(ScrapeOneError::MissingMusicMetadata);
    };
    let siblings = library.entries_by_artist_album(artist, album).await?;
    let scrapers = MusicScrapers::from_config(config);
    scrape_track_lyrics(
        library,
        &scrapers.lrclib,
        &siblings,
        &AtomicBool::new(false),
    )
    .await?;
    Ok(report)
}

#[derive(Debug, thiserror::Error)]
pub enum ScrapeOneError {
    #[error("no TMDb API key configured")]
    NoApiKey,
    #[error("this entry is music, not a movie or episode")]
    WrongKind,
    #[error("this entry is not a music track")]
    NotMusic,
    #[error(transparent)]
    Tmdb(#[from] TmdbError),
    #[error(transparent)]
    Override(#[from] crate::scrape::tmdb::TmdbOverrideError),
    #[error("library error: {0}")]
    Store(#[from] sqlx::Error),
    #[error("this entry has no artist/album metadata to search MusicBrainz with")]
    MissingMusicMetadata,
    #[error(transparent)]
    MusicBrainz(#[from] MbError),
}

/// Best-effort artist photo: MusicBrainz artist -> Commons file relation ->
/// Wikimedia URL resolution -> download. Any step failing just means no
/// photo this run; it never affects match/not-found/failed accounting.
async fn fetch_artist_photo(
    mb: &MusicBrainzClient,
    wikimedia: &WikimediaClient,
    artist_mbid: &str,
) -> Option<Vec<u8>> {
    let artist = mb.artist_lookup(artist_mbid).await.ok()?;
    let commons_file = artist.commons_file?;
    let url = wikimedia.resolve_file_url(&commons_file).await.ok()?;
    wikimedia.download(&url).await.ok()
}

async fn download_bytes(url: &str) -> Result<Vec<u8>, CoverArtError> {
    let response = reqwest::get(url)
        .await
        .map_err(|e| CoverArtError::Unavailable(e.to_string()))?;
    if !response.status().is_success() {
        return Err(CoverArtError::Unavailable(format!(
            "download returned {}",
            response.status()
        )));
    }
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| CoverArtError::Unavailable(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan_root;
    use axum::extract::Query;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    fn resolver(root: &std::path::Path) -> SharedRootResolver {
        SharedRootResolver::new(crate::roots::RootResolver::single(root.to_path_buf()))
    }

    #[test]
    fn search_query_strips_release_tags_confirmed_to_break_tmdb_search() {
        // Both queries below are real filenames that returned zero TMDb
        // results with the noise attached and matched immediately once
        // stripped (verified live against the real API, not assumed).
        assert_eq!(
            search_query_for("10 Cloverfield Lane 1080p BluRay x264"),
            "10 Cloverfield Lane"
        );
        assert_eq!(
            search_query_for("28 Days Later 1080p BluRay DDP5 1 x265 10bit-GalaxyRG265"),
            "28 Days Later"
        );
        // A scene-release qualifier tag ("Proper") sitting between the title
        // and the resolution tag used to survive into the query and broke
        // an otherwise-correct match — confirmed live: TMDb returns zero
        // results for "28 Years Later Proper" but one correct result (id
        // 1100988) for "28 Years Later".
        assert_eq!(
            search_query_for("28 Years Later Proper 1080p WEB-DL DDP5 1 x265-NeoNoir"),
            "28 Years Later"
        );
        // All four are taken verbatim from the current library's failed
        // rows and were verified against TMDb's live search service.
        assert_eq!(
            search_query_for("Alien Resurrection Directors Cut 1080p BRrip x264"),
            "Alien Resurrection"
        );
        assert_eq!(
            search_query_for("Dawn of the Dead Arrow 1080p BluRay HEVC"),
            "Dawn of the Dead"
        );
        assert_eq!(
            search_query_for("Mortal.Kombat.II.2026.NORDIC.1080p.BluRay.x264"),
            "Mortal Kombat II 2026"
        );
        assert_eq!(
            search_query_for("Thirteen (Thir13en) Ghosts - Horror 2001 Eng Subs 1080p"),
            "Thirteen (Thir13en) Ghosts"
        );
    }

    #[test]
    fn search_query_strips_pixel_dimension_tags_confirmed_to_break_tmdb_search() {
        // Reported live in #233: BDRip encodes that crop letterbox bars use
        // the actual encoded frame size (e.g. "1920x812") instead of a named
        // resolution tag like "1080p", and it survived into the TMDb query
        // since it isn't in SEARCH_QUERY_NOISE_TOKENS's literal list.
        assert_eq!(
            search_query_for("Battle For The Planet Of The Apes 1920x812 BDRip x264 DTS-HD MA"),
            "Battle For The Planet Of The Apes"
        );
        assert_eq!(
            search_query_for("Dawn Of The Planet Of The Apes 1920x1038 BDRip x264 DTS-HD MA"),
            "Dawn Of The Planet Of The Apes"
        );
        assert_eq!(
            search_query_for("Planet Of The Apes 1920X816 BDRip x264 DTS-HD MA"),
            "Planet Of The Apes"
        );
    }

    #[test]
    fn search_query_is_case_insensitive_and_matches_a_whole_token_only() {
        assert_eq!(search_query_for("The Matrix WEBRip"), "The Matrix");
        // "4k" is a noise token, but "4kids" (a hypothetical real title word)
        // must not be truncated on a partial match.
        assert_eq!(search_query_for("4kids and Counting"), "4kids and Counting");
    }

    #[test]
    fn search_query_with_no_noise_tokens_is_unchanged() {
        assert_eq!(search_query_for("Heat"), "Heat");
        assert_eq!(search_query_for("The Dark Knight"), "The Dark Knight");
        assert_eq!(
            search_query_for("Mission: Impossible - Ghost Protocol"),
            "Mission: Impossible - Ghost Protocol"
        );
    }

    #[test]
    fn search_query_that_is_entirely_noise_falls_back_to_the_original_title() {
        // Never send TMDb an empty query — a title so noisy every token
        // matches should degrade to searching the raw title, not "".
        assert_eq!(search_query_for("1080p x264"), "1080p x264");
    }

    #[test]
    fn specials_match_tmdb_names_even_when_filename_punctuation_differs() {
        let season = ScrapedSeason {
            episode_details: [
                (
                    13,
                    ScrapedEpisode {
                        title: "A Sitdown with Michael C. Hall and John Lithgow".into(),
                        overview: Some("A conversation.".into()),
                        still_url: None,
                    },
                ),
                (
                    33,
                    ScrapedEpisode {
                        title: "Dissecting Dexter 01 - Dexter's Origins".into(),
                        overview: None,
                        still_url: None,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let matched = match_tmdb_episode_title(
            "A Sit-down With Michael C. Hall and John Lithgow",
            Some(0),
            None,
            &season,
        )
        .unwrap();
        assert_eq!(
            matched.title,
            "A Sitdown with Michael C. Hall and John Lithgow"
        );
        assert_eq!(
            match_tmdb_episode_title("Dissecting Dexter", Some(0), None, &season)
                .unwrap()
                .title,
            "Dissecting Dexter 01 - Dexter's Origins"
        );
        let dark_defender = ScrapedSeason {
            episode_details: [(
                21,
                ScrapedEpisode {
                    title: "The Dark Defender: Little Chino".into(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        assert_eq!(
            match_tmdb_episode_title("Little Chino", Some(0), None, &dark_defender)
                .unwrap()
                .title,
            "The Dark Defender: Little Chino"
        );
        assert!(match_tmdb_episode_title("Michael C Hall", Some(0), None, &season).is_none());
    }

    fn movie_entry(title: &str, relative_path: &str, year: Option<u32>) -> EntryRecord {
        EntryRecord {
            entry_key: "movie-key".into(),
            relative_path: relative_path.into(),
            kind: MediaKind::Movie,
            title: title.into(),
            size: 1,
            modified_time: 0,
            fingerprint: "fingerprint".into(),
            artist: None,
            album: None,
            track_number: None,
            show_title: None,
            season: None,
            episode: None,
            year,
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

    #[test]
    fn movie_search_uses_embedded_year_without_leaving_it_in_the_query() {
        let entry = movie_entry(
            "A Nightmare On Elm Street 1984 1080p BrRip x264 YIFY",
            "Batocera-movies-shows/A Nightmare On Elm Street (720p).mp4",
            None,
        );
        assert_eq!(
            movie_search_candidates(&entry),
            vec![("A Nightmare On Elm Street".into(), Some(1984))]
        );
        assert_eq!(embedded_year_hint("1984"), None);
    }

    #[test]
    fn embedded_year_hint_ignores_pixel_dimension_tags() {
        // "1920x812" used to be misread as release year 1920 (the width
        // half of the dimension tag happens to fall in the valid year
        // range), which then hard-filtered TMDb's search to zero results.
        assert_eq!(
            embedded_year_hint("Battle For The Planet Of The Apes 1920x812 BDRip"),
            None
        );
        // A real year elsewhere in the title is still found once the
        // dimension tag itself is skipped.
        assert_eq!(
            embedded_year_hint("Mortal Kombat II 2026 1920x816 BDRip"),
            Some(2026)
        );
    }

    #[test]
    fn movie_search_strips_pixel_dimension_tag_and_does_not_misread_it_as_a_year() {
        let entry = movie_entry(
            "Battle For The Planet Of The Apes 1920x812 BDRip x264 DTS-HD MA",
            "Battle.For.The.Planet.Of.The.Apes.1920x812.BDRip.x264.DTS-HD.MA.mkv",
            None,
        );
        assert_eq!(
            movie_search_candidates(&entry),
            vec![("Battle For The Planet Of The Apes".into(), None)]
        );
    }

    #[test]
    fn movie_search_falls_back_from_bad_container_tag_to_clean_filename() {
        let entry = movie_entry(
            "JAWS_t02 mkv-muxed (1)",
            "Batocera-movies-shows/Jaws.1975.1080p.BrRip.x264.bitloks.YIFY.mp4",
            Some(1975),
        );
        assert_eq!(
            movie_search_candidates(&entry),
            vec![
                ("JAWS t02 mkv-muxed (1)".into(), Some(1975)),
                ("Jaws".into(), Some(1975)),
            ]
        );
    }

    // --- search_query_for_album: real-library-confirmed album name cleanup ---
    // Real bug, confirmed live against the real MusicBrainz API: the raw
    // folder name "2004 - No Silence (AVTCD-95770)" found zero results;
    // "No Silence" found the correct release at the top score.

    #[test]
    fn album_query_strips_leading_year_and_trailing_catalog_code() {
        assert_eq!(
            search_query_for_album("ATB", "2004 - No Silence (AVTCD-95770)"),
            "No Silence"
        );
        assert_eq!(
            search_query_for_album("ATB", "2003 - Addicted To Music (BANGCD029)"),
            "Addicted To Music"
        );
    }

    #[test]
    fn album_query_strips_a_redundant_leading_artist_name() {
        // Real bug, confirmed live: the year-only-stripped form ("Cosmic
        // Gate - The Drums") still found zero MusicBrainz results; only
        // stripping the redundant artist prefix too ("The Drums") found
        // the correct release.
        assert_eq!(
            search_query_for_album(
                "Cosmic Gate",
                "1999 - Cosmic Gate - The Drums (7243 886923 2 8)"
            ),
            "The Drums"
        );
    }

    #[test]
    fn album_query_strips_multiple_trailing_bracket_groups() {
        assert_eq!(
            search_query_for_album(
                "Kyau & Albert",
                "2009 - Kyau & Albert - Best Of 2002-2009 (EUPH100CD) [CD]"
            ),
            "Best Of 2002-2009"
        );
    }

    #[test]
    fn album_query_with_no_noise_is_unchanged() {
        assert_eq!(search_query_for_album("Pink Floyd", "The Wall"), "The Wall");
    }

    #[test]
    fn album_query_that_is_entirely_noise_falls_back_to_the_original() {
        assert_eq!(
            search_query_for_album("ATB", "2004 - (CATALOG-1)"),
            "2004 - (CATALOG-1)"
        );
    }

    #[tokio::test]
    async fn local_album_cover_prefers_front_scan_and_ignores_packaging_images() {
        assert!(local_cover_score("Discipline.png").is_some());
        assert!(local_cover_score("Comeback Kid.jpg").is_some());
        assert!(local_cover_score("Album-Disc-1.png").is_none());

        let (root, db_path) = fixture_dirs("local-album-cover");
        let album = root.join("music/Slipknot/We Are Not Your Kind (2019)");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01 - Insert Coin.flac"), vec![0u8; 10]).unwrap();
        std::fs::write(
            album.join("Slipknot - We Are Not Your Kind [123].png"),
            [1u8],
        )
        .unwrap();
        std::fs::write(
            album.join("Slipknot - We Are Not Your Kind [123]-Back.png"),
            [2u8],
        )
        .unwrap();
        std::fs::write(
            album.join("Slipknot - We Are Not Your Kind [123]-Gatefold.png"),
            [3u8],
        )
        .unwrap();

        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        let entry = library.list().await.unwrap().pop().unwrap();
        let relative = import_local_album_cover(&resolver(&root), &entry)
            .await
            .expect("front cover should be imported");

        assert_eq!(
            relative,
            "music/Slipknot/We Are Not Your Kind (2019)/images/album-cover.png"
        );
        assert_eq!(std::fs::read(root.join(relative)).unwrap(), [1u8]);
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn local_album_cover_import_finds_a_unicode_normalized_album_directory() {
        let (root, db_path) = fixture_dirs("local-album-cover-unicode");
        let album = root.join("music/Cafe\u{301}");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01 - Track.flac"), vec![0u8; 10]).unwrap();
        std::fs::write(album.join("folder.jpg"), [1u8]).unwrap();

        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        let mut entry = library.list().await.unwrap().pop().unwrap();
        entry.relative_path = "music/Café/01 - Track.flac".into();

        let relative = import_local_album_cover(&resolver(&root), &entry)
            .await
            .expect("cover beside an NFD directory should be imported");

        assert_eq!(relative, "music/Café/images/album-cover.jpg");
        assert_eq!(
            std::fs::read(album.join("images/album-cover.jpg")).unwrap(),
            [1u8]
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    fn fixture_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("swarm-scrape-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        (base.join("media"), base.join("library.sqlite"))
    }

    #[tokio::test]
    async fn no_tmdb_key_skips_videos_without_touching_tracks() {
        let (root, db_path) = fixture_dirs("no-key");
        std::fs::create_dir_all(root.join("movies/Heat (1995)")).unwrap();
        std::fs::write(root.join("movies/Heat (1995)/Heat.1995.mkv"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &ScrapeConfig::default(),
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            report,
            BulkScrapeReport {
                matched: 0,
                not_found: 0,
                failed: 0,
                skipped: 1,
                issues: vec![]
            }
        );
        // Skipped entries stay unscraped so a later run (with a key) retries them.
        assert_eq!(library.missing_scrape().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn end_to_end_movie_scrape_writes_title_genres_and_poster() {
        let (root, db_path) = fixture_dirs("movie-e2e");
        std::fs::create_dir_all(root.join("movies/Heat (1995)")).unwrap();
        std::fs::write(root.join("movies/Heat (1995)/Heat.1995.mkv"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        // Bind first so the poster path can embed the mock server's own address.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/search/movie", get(|| async { Json(json!({"results": [{"id": 1}]})) }))
            .route(
                "/movie/1",
                get(|| async {
                    Json(json!({
                        "title": "Heat", "genres": [{"name": "Crime"}], "poster_path": "/p.jpg",
                        "overview": "A group of professional bank robbers start to feel the heat from police.",
                        "credits": {"cast": [{"name": "Al Pacino", "character": "Vincent Hanna"}]}
                    }))
                }),
            )
            .route("/img/w342/p.jpg", get(|| async { [9u8, 9, 9] }));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}/img")),
            introdb_api_base: Some(format!("http://{addr}")),
            ..Default::default()
        };
        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            report,
            BulkScrapeReport {
                matched: 1,
                not_found: 0,
                failed: 0,
                skipped: 0,
                issues: vec![]
            }
        );

        let entry = &library.list().await.unwrap()[0];
        assert_eq!(entry.scraped_title.as_deref(), Some("Heat"));
        assert_eq!(entry.genres, vec!["Crime"]);
        assert_eq!(entry.cast.len(), 1);
        assert_eq!(entry.cast[0].name, "Al Pacino");
        assert_eq!(entry.cast[0].character.as_deref(), Some("Vincent Hanna"));
        assert_eq!(
            entry.overview.as_deref(),
            Some("A group of professional bank robbers start to feel the heat from police.")
        );
        assert_eq!(entry.artwork_version, 1);
        let (art_path, art_version) = library
            .artwork(&entry.entry_key, ArtworkKind::Poster)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(art_version, 1);
        assert_eq!(std::fs::read(root.join(&art_path)).unwrap(), vec![9, 9, 9]);
        assert!(library.missing_scrape().await.unwrap().is_empty());
        let repair = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            repair.matched, 1,
            "a processed entry with missing metadata/artwork must be retried"
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn end_to_end_episode_scrape_writes_show_season_and_episode_artwork() {
        let (root, db_path) = fixture_dirs("episode-artwork-e2e");
        std::fs::create_dir_all(root.join("Shows/American Dad!/Season 2")).unwrap();
        std::fs::write(
            root.join("Shows/American Dad!/Season 2/American.Dad.S02E01.mkv"),
            vec![0u8; 10],
        )
        .unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route(
                "/search/tv",
                get(|| async { Json(json!({"results": [{"id": 1433, "name": "American Dad!"}]})) }),
            )
            .route(
                "/tv/1433",
                get(|| async {
                    Json(json!({
                        "name": "American Dad!",
                        "genres": [{"name": "Animation"}],
                        "poster_path": "/show.jpg",
                        "backdrop_path": "/show-backdrop.jpg"
                    }))
                }),
            )
            .route(
                "/tv/1433/season/2",
                get(|| async {
                    Json(json!({
                        "poster_path": "/season-2.jpg",
                        "episodes": [{"episode_number": 1, "still_path": "/s02e01.jpg"}]
                    }))
                }),
            )
            .route(
                "/media",
                get(|Query(query): Query<HashMap<String, String>>| async move {
                    assert_eq!(query.get("tmdb_id").map(String::as_str), Some("1433"));
                    assert_eq!(query.get("season").map(String::as_str), Some("2"));
                    assert_eq!(query.get("episode").map(String::as_str), Some("1"));
                    Json(json!({
                        "intro": [{"start_ms": 30_000, "end_ms": 90_000}],
                        "credits": [{"start_ms": 1_200_000, "end_ms": null}]
                    }))
                }),
            )
            .route("/img/w342/show.jpg", get(|| async { [1u8, 1, 1] }))
            .route(
                "/img/w1280/show-backdrop.jpg",
                get(|| async { [2u8, 2, 2] }),
            )
            .route("/img/w342/season-2.jpg", get(|| async { [3u8, 3, 3] }))
            .route("/img/w780/s02e01.jpg", get(|| async { [4u8, 4, 4] }));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}/img")),
            introdb_api_base: Some(format!("http://{addr}")),
            ..Default::default()
        };
        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.matched, 1);

        let entry = &library.list().await.unwrap()[0];
        assert_eq!(entry.scraped_title.as_deref(), Some("American Dad!"));
        assert_eq!(entry.artwork_version, 3);
        let (poster, _) = library
            .artwork(&entry.entry_key, ArtworkKind::Poster)
            .await
            .unwrap()
            .unwrap();
        let (season, _) = library
            .artwork(&entry.entry_key, ArtworkKind::SeasonPoster)
            .await
            .unwrap()
            .unwrap();
        let (still, _) = library
            .artwork(&entry.entry_key, ArtworkKind::Backdrop)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(root.join(poster)).unwrap(), vec![1, 1, 1]);
        assert_eq!(std::fs::read(root.join(season)).unwrap(), vec![3, 3, 3]);
        assert_eq!(std::fs::read(root.join(still)).unwrap(), vec![4, 4, 4]);
        let (_, catalog) = library.catalog_snapshot().await.unwrap();
        assert_eq!(catalog[0].skip_segments.len(), 2);
        assert_eq!(
            catalog[0].skip_segments[0].kind,
            swarm_core::peer::SkipSegmentKind::Intro
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn pinpoint_scrape_overwrites_an_already_matched_entry() {
        let (root, db_path) = fixture_dirs("pinpoint-overwrite");
        std::fs::create_dir_all(root.join("movies/Heat (1995)")).unwrap();
        std::fs::write(root.join("movies/Heat (1995)/Heat.1995.mkv"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async { Json(json!({"results": [{"id": 1}]})) }),
            )
            .route(
                "/movie/1",
                get(|| async { Json(json!({"title": "Wrong Match", "genres": []})) }),
            )
            .route(
                "/movie/2",
                get(|| async { Json(json!({"title": "Heat", "genres": [{"name": "Crime"}]})) }),
            );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}")),
            introdb_api_base: Some(format!("http://{addr}")),
            ..Default::default()
        };

        // First pass: bulk scrape matches the wrong title (simulating a bad match).
        run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        let entry = library.list().await.unwrap().into_iter().next().unwrap();
        assert_eq!(entry.scraped_title.as_deref(), Some("Wrong Match"));

        // Pinpoint rescrape with a manual override to the correct id must
        // succeed even though the entry is already "processed" per
        // missing_scrape, and must overwrite the previous (wrong) result.
        assert!(library.missing_scrape().await.unwrap().is_empty());
        let scraped = scrape_one_video(
            &library,
            &resolver(&root),
            &config,
            &entry,
            Some(TmdbOverride::Id(2)),
        )
        .await
        .unwrap();
        assert_eq!(scraped.title, "Heat");
        let updated = library.get(&entry.entry_key).await.unwrap().unwrap();
        assert_eq!(updated.scraped_title.as_deref(), Some("Heat"));
        assert_eq!(updated.genres, vec!["Crime"]);
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn bulk_scrape_force_rescrapes_already_processed_entries() {
        let (root, db_path) = fixture_dirs("bulk-force-rescrape");
        std::fs::create_dir_all(root.join("movies/Heat (1995)")).unwrap();
        std::fs::write(root.join("movies/Heat (1995)/Heat.1995.mkv"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async { Json(json!({"results": [{"id": 1}]})) }),
            )
            .route(
                "/movie/1",
                get(|| async {
                    Json(json!({
                        "title": "Heat",
                        "genres": [{"name": "Crime"}],
                        "overview": "A complete test record.",
                        "poster_path": "/poster.jpg",
                        "backdrop_path": "/backdrop.jpg",
                        "credits": {"cast": [{"name": "Al Pacino", "character": "Vincent Hanna"}]},
                        "release_dates": {"results": [{"iso_3166_1": "US", "release_dates": [{"certification": "R"}]}]},
                        "vote_average": 8.3,
                        "vote_count": 100
                    }))
                }),
            )
            .route("/w342/poster.jpg", get(|| async { [1u8, 2, 3] }))
            .route("/w1280/backdrop.jpg", get(|| async { [4u8, 5, 6] }));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}")),
            introdb_api_base: Some(format!("http://{addr}")),
            ..Default::default()
        };

        let first = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(first.matched, 1);
        assert!(
            library.missing_scrape().await.unwrap().is_empty(),
            "must be marked processed"
        );

        // Default (force: false) must not touch an already-processed entry.
        let second = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            second,
            BulkScrapeReport::default(),
            "nothing left to do without force"
        );

        // force: true must re-scrape it anyway, even though it's already processed.
        let third = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            third.matched, 1,
            "force must re-scrape an already-processed entry"
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn bulk_scrape_does_not_redownload_artwork_already_on_disk() {
        let (root, db_path) = fixture_dirs("bulk-skip-existing-artwork");
        std::fs::create_dir_all(root.join("movies/Heat (1995)")).unwrap();
        std::fs::write(root.join("movies/Heat (1995)/Heat.1995.mkv"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let poster_hits = std::sync::Arc::new(AtomicUsize::new(0));
        let backdrop_hits = std::sync::Arc::new(AtomicUsize::new(0));
        let poster_hits_route = poster_hits.clone();
        let backdrop_hits_route = backdrop_hits.clone();
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async { Json(json!({"results": [{"id": 1}]})) }),
            )
            .route(
                "/movie/1",
                get(|| async {
                    // Deliberately no vote_average/vote_count: community_rating
                    // stays NULL forever, so this entry keeps matching
                    // `incomplete_scrape()` across repeated non-force runs —
                    // the setup needed to prove artwork is left alone on a
                    // second pass while everything else still gets rewritten.
                    Json(json!({
                        "title": "Heat",
                        "genres": [{"name": "Crime"}],
                        "overview": "A complete test record.",
                        "poster_path": "/poster.jpg",
                        "backdrop_path": "/backdrop.jpg",
                        "credits": {"cast": [{"name": "Al Pacino", "character": "Vincent Hanna"}]},
                        "release_dates": {"results": [{"iso_3166_1": "US", "release_dates": [{"certification": "R"}]}]}
                    }))
                }),
            )
            .route(
                "/w342/poster.jpg",
                get(move || {
                    let hits = poster_hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        [1u8, 2, 3]
                    }
                }),
            )
            .route(
                "/w1280/backdrop.jpg",
                get(move || {
                    let hits = backdrop_hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        [4u8, 5, 6]
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}")),
            introdb_api_base: Some(format!("http://{addr}")),
            ..Default::default()
        };

        let first = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(first.matched, 1);
        assert_eq!(poster_hits.load(Ordering::SeqCst), 1);
        assert_eq!(backdrop_hits.load(Ordering::SeqCst), 1);
        assert!(
            !library.incomplete_scrape().await.unwrap().is_empty(),
            "community_rating is deliberately never filled, so the entry stays incomplete"
        );

        let second = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            second.matched, 1,
            "entry is still incomplete, so it gets re-scraped"
        );
        assert_eq!(
            poster_hits.load(Ordering::SeqCst),
            1,
            "poster already exists on disk, so a non-force re-scrape must not re-download it"
        );
        assert_eq!(
            backdrop_hits.load(Ordering::SeqCst),
            1,
            "backdrop already exists on disk, so a non-force re-scrape must not re-download it"
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn manual_tmdb_url_override_skips_search_entirely() {
        let (root, db_path) = fixture_dirs("pinpoint-url-override");
        std::fs::create_dir_all(root.join("movies/Foo (2020)")).unwrap();
        std::fs::write(root.join("movies/Foo (2020)/Foo.2020.mkv"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        let entry = library.list().await.unwrap().into_iter().next().unwrap();

        // No /search/* route registered at all — a request there would 404
        // and fail the test, proving the override path never calls search.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().route(
            "/movie/42",
            get(|| async { Json(json!({"title": "Direct Hit"})) }),
        );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}")),
            introdb_api_base: Some(format!("http://{addr}")),
            ..Default::default()
        };

        let scraped = scrape_one_video(
            &library,
            &resolver(&root),
            &config,
            &entry,
            Some(TmdbOverride::Url(
                "https://www.themoviedb.org/movie/42-direct-hit".to_string(),
            )),
        )
        .await
        .unwrap();
        assert_eq!(scraped.title, "Direct Hit");
    }

    #[tokio::test]
    async fn malformed_override_url_errors_cleanly_without_touching_existing_data() {
        let (root, db_path) = fixture_dirs("pinpoint-bad-url");
        std::fs::create_dir_all(root.join("movies/Foo (2020)")).unwrap();
        std::fs::write(root.join("movies/Foo (2020)/Foo.2020.mkv"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        library
            .set_scrape_result(
                &library.list().await.unwrap()[0].entry_key,
                Some("Existing Title"),
                &[],
                &[],
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let entry = library.list().await.unwrap().into_iter().next().unwrap();

        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            ..Default::default()
        };
        let result = scrape_one_video(
            &library,
            &resolver(&root),
            &config,
            &entry,
            Some(TmdbOverride::Url("https://example.com/not-tmdb".into())),
        )
        .await;
        assert!(matches!(result, Err(ScrapeOneError::Override(_))));

        // Existing data must be untouched by a failed override.
        let unchanged = library.get(&entry.entry_key).await.unwrap().unwrap();
        assert_eq!(unchanged.scraped_title.as_deref(), Some("Existing Title"));
    }

    #[tokio::test]
    async fn pinpoint_track_rescrape_resyncs_the_whole_album_group() {
        let (root, db_path) = fixture_dirs("pinpoint-track");
        std::fs::create_dir_all(root.join("music/Pink Floyd/The Wall")).unwrap();
        std::fs::write(
            root.join("music/Pink Floyd/The Wall/01 - In The Flesh.flac"),
            vec![1u8; 10],
        )
        .unwrap();
        std::fs::write(
            root.join("music/Pink Floyd/The Wall/02 - The Thin Ice.flac"),
            vec![2u8; 10],
        )
        .unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mb_base = format!("http://{addr}/mb");
        let router = Router::new()
            .route(
                "/mb/release/",
                get(|| async { Json(json!({"releases": [{"id": "rel-1"}]})) }),
            )
            .route(
                "/mb/release/rel-1",
                get(|| async { Json(json!({"genres": [{"name": "Rock"}], "artist-credit": []})) }),
            );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let entries = library.list().await.unwrap();
        let config = ScrapeConfig {
            musicbrainz_base: Some(mb_base),
            ..Default::default()
        };
        let report = scrape_one_track(&library, &resolver(&root), &config, &entries[0])
            .await
            .unwrap();
        assert_eq!(
            report,
            BulkScrapeReport {
                matched: 2,
                not_found: 0,
                failed: 0,
                skipped: 0,
                issues: vec![]
            }
        );
        for entry in library.list().await.unwrap() {
            assert_eq!(entry.genres, vec!["Rock"]);
        }
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn pinpoint_video_scrape_without_api_key_is_a_clean_error() {
        let (root, db_path) = fixture_dirs("pinpoint-no-key");
        std::fs::create_dir_all(root.join("movies/Foo (2020)")).unwrap();
        std::fs::write(root.join("movies/Foo (2020)/Foo.2020.mkv"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        let entry = library.list().await.unwrap().into_iter().next().unwrap();
        let result = scrape_one_video(
            &library,
            &resolver(&root),
            &ScrapeConfig::default(),
            &entry,
            None,
        )
        .await;
        assert!(matches!(result, Err(ScrapeOneError::NoApiKey)));
    }

    #[tokio::test]
    async fn not_found_movie_is_marked_processed_not_retried() {
        let (root, db_path) = fixture_dirs("movie-not-found");
        std::fs::create_dir_all(root.join("movies/Unknowable Film")).unwrap();
        std::fs::write(
            root.join("movies/Unknowable Film/Unknowable Film.mkv"),
            vec![0u8; 10],
        )
        .unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let router = Router::new().route(
            "/search/movie",
            get(|| async { Json(json!({"results": []})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}")),
            introdb_api_base: Some(format!("http://{addr}")),
            ..Default::default()
        };
        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.matched, 0);
        assert_eq!(report.not_found, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].title, "Unknowable Film");
        assert_eq!(report.issues[0].reason, "no match found on TMDb");
        // Processed (no match) still counts as done — must not be re-queued.
        assert!(library.missing_scrape().await.unwrap().is_empty());
        let entry = &library.list().await.unwrap()[0];
        assert_eq!(entry.scraped_title, None);
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn bulk_scrape_hits_tmdb_and_musicbrainz_concurrently_not_sequentially() {
        // Records the instant each service's *first* request lands, rather
        // than comparing total wall-clock time — MusicBrainzClient
        // self-enforces a real ~1.05s minimum interval between its own
        // requests (respecting MusicBrainz's real rate limit), which would
        // swamp any timing budget based on total duration regardless of
        // whether the two scrapes ran concurrently or not. Whether the two
        // *started* within a tight window of each other is what actually
        // distinguishes concurrent (tokio::join!) from sequential (movies
        // fully finish, including any of their own delay, before music
        // starts at all) — sequential would show a gap of at least DELAY.
        const DELAY: std::time::Duration = std::time::Duration::from_millis(250);
        let started = std::time::Instant::now();
        let tmdb_hit_at = std::sync::Arc::new(std::sync::Mutex::new(None::<std::time::Duration>));
        let mb_hit_at = std::sync::Arc::new(std::sync::Mutex::new(None::<std::time::Duration>));

        let (root, db_path) = fixture_dirs("concurrent-scrape");
        std::fs::create_dir_all(root.join("movies/Heat (1995)")).unwrap();
        std::fs::write(root.join("movies/Heat (1995)/Heat.1995.mkv"), vec![0u8; 10]).unwrap();
        std::fs::create_dir_all(root.join("music/Artist/Album")).unwrap();
        std::fs::write(
            root.join("music/Artist/Album/01 - Song.flac"),
            vec![1u8; 10],
        )
        .unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        assert_eq!(library.list().await.unwrap().len(), 2);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tmdb_recorder, mb_recorder) = (tmdb_hit_at.clone(), mb_hit_at.clone());
        let router = Router::new()
            .route(
                "/search/movie",
                get(move || {
                    let recorder = tmdb_recorder.clone();
                    async move {
                        *recorder.lock().unwrap() = Some(started.elapsed());
                        tokio::time::sleep(DELAY).await;
                        Json(json!({"results": [{"id": 1}]}))
                    }
                }),
            )
            .route(
                "/movie/1",
                get(|| async { Json(json!({"title": "Heat", "genres": []})) }),
            )
            .route(
                "/mb/release/",
                get(move || {
                    let recorder = mb_recorder.clone();
                    async move {
                        *recorder.lock().unwrap() = Some(started.elapsed());
                        Json(json!({"releases": [{"id": "rel-1"}]}))
                    }
                }),
            )
            .route(
                "/mb/release/rel-1",
                get(|| async { Json(json!({"genres": [], "artist-credit": []})) }),
            );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}")),
            introdb_api_base: Some(format!("http://{addr}")),
            musicbrainz_base: Some(format!("http://{addr}/mb")),
            ..Default::default()
        };
        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();

        assert_eq!(
            report.matched, 2,
            "both must still actually succeed: {report:?}"
        );
        let tmdb_at = tmdb_hit_at
            .lock()
            .unwrap()
            .expect("TMDb search must have been hit");
        let mb_at = mb_hit_at
            .lock()
            .unwrap()
            .expect("MusicBrainz search must have been hit");
        let gap = tmdb_at.abs_diff(mb_at);
        assert!(
            gap < DELAY,
            "TMDb hit at {tmdb_at:?}, MusicBrainz hit at {mb_at:?} — a {gap:?} gap means they did not start \
             concurrently (sequential would show a gap of at least the {DELAY:?} TMDb delay, since MusicBrainz \
             would only start once the video scrape, including its delay, fully finished)"
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn music_scrape_groups_by_album_and_shares_cover_art() {
        let (root, db_path) = fixture_dirs("music-e2e");
        std::fs::create_dir_all(root.join("music/Pink Floyd/The Wall")).unwrap();
        std::fs::write(
            root.join("music/Pink Floyd/The Wall/01 - In The Flesh.flac"),
            vec![1u8; 10],
        )
        .unwrap();
        std::fs::write(
            root.join("music/Pink Floyd/The Wall/02 - The Thin Ice.flac"),
            vec![2u8; 10],
        )
        .unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        assert_eq!(library.list().await.unwrap().len(), 2);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mb_base = format!("http://{addr}/mb");
        let ca_base = format!("http://{addr}/ca");
        let router = Router::new()
            .route(
                "/mb/release/",
                get(|| async { Json(json!({"releases": [{"id": "rel-1"}]})) }),
            )
            .route(
                "/mb/release/rel-1",
                get(|| async { Json(json!({"genres": [{"name": "Rock"}], "artist-credit": []})) }),
            )
            .route(
                "/ca/release/rel-1",
                get(move || {
                    let cover_url = format!("http://{addr}/cover.jpg");
                    async move { Json(json!({"images": [{"front": true, "image": cover_url}]})) }
                }),
            )
            .route("/cover.jpg", get(|| async { [7u8, 7, 7] }));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let config = ScrapeConfig {
            musicbrainz_base: Some(mb_base),
            coverart_base: Some(ca_base),
            ..Default::default()
        };
        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            report,
            BulkScrapeReport {
                matched: 2,
                not_found: 0,
                failed: 0,
                skipped: 0,
                issues: vec![]
            }
        );

        let entries = library.list().await.unwrap();
        for entry in &entries {
            assert_eq!(entry.genres, vec!["Rock"]);
            assert_eq!(entry.artwork_version, 1);
        }
        // Both tracks in the album point at the same physical cover file.
        let (path_a, _) = library
            .artwork(&entries[0].entry_key, ArtworkKind::Cover)
            .await
            .unwrap()
            .unwrap();
        let (path_b, _) = library
            .artwork(&entries[1].entry_key, ArtworkKind::Cover)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(path_a, path_b);
        assert_eq!(std::fs::read(root.join(&path_a)).unwrap(), vec![7, 7, 7]);
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn persistent_musicbrainz_outage_opens_circuit_for_remaining_albums() {
        let (root, db_path) = fixture_dirs("musicbrainz-circuit-breaker");
        std::fs::create_dir_all(root.join("music/Artist One/Album One")).unwrap();
        std::fs::write(
            root.join("music/Artist One/Album One/01 - Song One.flac"),
            vec![1u8; 10],
        )
        .unwrap();
        std::fs::create_dir_all(root.join("music/Artist Two/Album Two")).unwrap();
        std::fs::write(
            root.join("music/Artist Two/Album Two/01 - Song Two.flac"),
            vec![2u8; 10],
        )
        .unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let handler_hits = hits.clone();
        let router = Router::new().route(
            "/mb/release/",
            get(move || {
                let handler_hits = handler_hits.clone();
                async move {
                    handler_hits.fetch_add(1, Ordering::Relaxed);
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        [("retry-after", "0")],
                        Json(json!({"error": "temporarily unavailable"})),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = ScrapeConfig {
            musicbrainz_base: Some(format!("http://{addr}/mb")),
            ..Default::default()
        };

        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();

        assert_eq!(report.failed, 2);
        assert_eq!(report.issues.len(), 2);
        assert_eq!(
            hits.load(Ordering::Relaxed),
            3,
            "only the first album should use the bounded retry budget"
        );
        assert_eq!(
            library.incomplete_scrape().await.unwrap().len(),
            2,
            "provider failures must remain eligible for the next scrape run"
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn bulk_music_scrape_caches_lrclib_lyrics_and_does_not_retry_a_completed_lookup() {
        let (root, db_path) = fixture_dirs("music-lyrics");
        std::fs::create_dir_all(root.join("music/Test Artist/Test Album")).unwrap();
        std::fs::write(
            root.join("music/Test Artist/Test Album/01 - Test Song.flac"),
            vec![1u8; 10],
        )
        .unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        let mut entry = library.list().await.unwrap().into_iter().next().unwrap();
        entry.duration_secs = Some(213.7);
        library.upsert(&entry).await.unwrap();

        let lyric_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorder = lyric_hits.clone();
        let router = Router::new()
            .route(
                "/mb/release/",
                get(|| async { Json(json!({"releases": [{"id": "rel-1"}]})) }),
            )
            .route(
                "/mb/release/rel-1",
                get(|| async { Json(json!({"genres": [], "artist-credit": []})) }),
            )
            .route(
                "/ca/release/rel-1",
                get(|| async { Json(json!({"images": []})) }),
            )
            .route(
                "/lyrics/api/get",
                get(move || {
                    let recorder = recorder.clone();
                    async move {
                        recorder.fetch_add(1, Ordering::Relaxed);
                        Json(json!({
                            "id": 17,
                            "instrumental": false,
                            "plainLyrics": "First line\nSecond line",
                            "syncedLyrics": "[00:01.00]First line\n[00:03.50]Second line",
                            "lang": "en"
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = ScrapeConfig {
            musicbrainz_base: Some(format!("http://{addr}/mb")),
            coverart_base: Some(format!("http://{addr}/ca")),
            lrclib_base: Some(format!("http://{addr}/lyrics")),
            ..Default::default()
        };

        run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        let lyrics = library
            .track_lyrics(&entry.entry_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lyrics.provider_id, Some(17));
        assert!(lyrics
            .synced_lyrics
            .as_deref()
            .unwrap()
            .contains("Second line"));
        assert_eq!(lyric_hits.load(Ordering::Relaxed), 1);

        run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            lyric_hits.load(Ordering::Relaxed),
            1,
            "cached lyrics must not be fetched on every normal scrape"
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn tracks_without_artist_or_album_are_skipped() {
        let (root, db_path) = fixture_dirs("music-skip");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("orphan.mp3"), vec![0u8; 10]).unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();

        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &ScrapeConfig::default(),
            &AtomicBool::new(false),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            report,
            BulkScrapeReport {
                matched: 0,
                not_found: 0,
                failed: 0,
                skipped: 1,
                issues: vec![]
            }
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn progress_events_stream_one_per_entry_with_correct_data_and_running_total() {
        // 1 movie (matched) + 1 album of 2 tracks (matched) = 3 entries, so
        // exactly 3 progress events, `processed` 1..=3, `total` fixed at 3
        // throughout — this is the whole property a progress bar depends on.
        let (root, db_path) = fixture_dirs("progress-events");
        std::fs::create_dir_all(root.join("movies/Heat (1995)")).unwrap();
        std::fs::write(root.join("movies/Heat (1995)/Heat.1995.mkv"), vec![0u8; 10]).unwrap();
        std::fs::create_dir_all(root.join("music/Pink Floyd/The Wall")).unwrap();
        std::fs::write(
            root.join("music/Pink Floyd/The Wall/01 - In The Flesh.flac"),
            vec![1u8; 10],
        )
        .unwrap();
        std::fs::write(
            root.join("music/Pink Floyd/The Wall/02 - The Thin Ice.flac"),
            vec![2u8; 10],
        )
        .unwrap();
        let library = Library::open(db_path.to_str().unwrap()).await.unwrap();
        scan_root(&library, &root).await.unwrap();
        assert_eq!(library.list().await.unwrap().len(), 3);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async { Json(json!({"results": [{"id": 1}]})) }),
            )
            .route(
                "/movie/1",
                get(|| async { Json(json!({"title": "Heat", "genres": [{"name": "Crime"}]})) }),
            )
            .route(
                "/mb/release/",
                get(|| async { Json(json!({"releases": [{"id": "rel-1"}]})) }),
            )
            .route(
                "/mb/release/rel-1",
                get(|| async { Json(json!({"genres": [{"name": "Rock"}], "artist-credit": []})) }),
            )
            .route(
                "/ca/release/rel-1",
                get(|| async { Json(json!({"images": []})) }),
            );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let config = ScrapeConfig {
            tmdb_api_key: Some("key".into()),
            tmdb_api_base: Some(format!("http://{addr}")),
            tmdb_image_base: Some(format!("http://{addr}/img")),
            introdb_api_base: Some(format!("http://{addr}")),
            musicbrainz_base: Some(format!("http://{addr}/mb")),
            coverart_base: Some(format!("http://{addr}/ca")),
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let report = run_bulk_scrape(
            &library,
            &resolver(&root),
            &config,
            &AtomicBool::new(false),
            Some(tx),
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.matched, 3);

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert_eq!(
            events.len(),
            3,
            "one progress event per entry, got {events:?}"
        );
        for (i, event) in events.iter().enumerate() {
            assert_eq!(
                event.processed,
                (i + 1) as u64,
                "processed must increment 1..=total in emission order"
            );
            assert_eq!(
                event.total, 3,
                "total must stay fixed at the entry count known before the loop started"
            );
            assert_eq!(event.outcome, ScrapeOutcome::Matched);
        }
        let movie_event = events
            .iter()
            .find(|e| e.title.starts_with("Heat"))
            .expect("movie event present");
        assert_eq!(movie_event.scraped_title.as_deref(), Some("Heat"));
        assert_eq!(movie_event.genres, vec!["Crime"]);
        let track_events: Vec<_> = events
            .iter()
            .filter(|e| e.entry_key != movie_event.entry_key)
            .collect();
        assert_eq!(
            track_events.len(),
            2,
            "one event per track, not one per album group"
        );
        for track_event in track_events {
            // Tracks never get a scraped_title (see scrape_one_album_group) —
            // only genres change.
            assert_eq!(track_event.scraped_title, None);
            assert_eq!(track_event.genres, vec!["Rock"]);
        }
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }
}
