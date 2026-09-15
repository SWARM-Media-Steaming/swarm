//! TMDb client (movies + TV) — requires a user-supplied free API key, like
//! Batocera.Drone's `movies/tmdb_client.py`. Two-tier errors so one 404
//! fails one title instead of aborting a whole bulk run.

use crate::store::CastMember;
use serde::Deserialize;
use std::collections::HashMap;

const DEFAULT_API_BASE: &str = "https://api.themoviedb.org/3";
const DEFAULT_IMAGE_BASE: &str = "https://image.tmdb.org/t/p";

#[derive(Debug, Clone, thiserror::Error)]
pub enum TmdbError {
    #[error("no TMDb match")]
    NotFound,
    #[error("TMDb unavailable: {0}")]
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScrapedVideo {
    /// TMDB's stable movie/show id. TV episode scraping uses the show id to
    /// fetch the matching season exactly once, rather than trying to find
    /// season and episode artwork in the show-level response (where TMDB
    /// does not return it).
    pub tmdb_id: u64,
    pub title: String,
    pub genres: Vec<String>,
    /// Fully-qualified image URLs, ready to download.
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    /// TV-only artwork from `/tv/{id}/season/{season}`. Kept distinct from
    /// the show poster and episode still so each browse level can render the
    /// image TMDB actually supplies for it.
    pub season_poster_url: Option<String>,
    /// Top-billed cast, in TMDb's own billing order.
    pub cast: Vec<CastMember>,
    /// TMDb's synopsis — `None` when TMDb has no overview for this title
    /// (a real, if rare, gap in their own data), never an empty string.
    pub overview: Option<String>,
    /// US content rating — MPAA-style (`"PG-13"`, `"R"`, ...) for a movie,
    /// TV Parental Guidelines-style (`"TV-14"`, `"TV-MA"`, ...) for a show.
    /// `None` when TMDb has no US certification on file, which is common
    /// for less-mainstream titles — same "real, if rare, gap" status as
    /// `overview`.
    pub certification: Option<String>,
    /// TMDb user score (native 0–10 scale) and the number of votes behind
    /// it. A zero-vote placeholder is represented as `None` rather than a
    /// misleading 0/10 rating.
    pub community_rating: Option<f64>,
    pub community_rating_votes: Option<u64>,
    /// TMDb's episode/special title, when this video was matched to a TV
    /// season entry. Kept separate from `title`, which is the canonical show
    /// title used for grouping.
    pub episode_title: Option<String>,
    /// Episode-specific synopsis, when TMDb supplies one.
    pub episode_overview: Option<String>,
}

/// Artwork returned by TMDB's season-details endpoint. One response covers
/// every episode in a season, which lets the bulk scraper cache this by
/// `(show_id, season_number)` instead of making one request per episode.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScrapedSeason {
    pub poster_url: Option<String>,
    pub episode_still_urls: HashMap<u32, String>,
    /// Episode metadata keyed by TMDb episode number. Specials are included
    /// here as season 0, even when the local file has no numeric episode.
    pub episode_details: HashMap<u32, ScrapedEpisode>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScrapedEpisode {
    pub title: String,
    pub overview: Option<String>,
    pub still_url: Option<String>,
}

/// A user-supplied manual TMDb match, bypassing search entirely — the
/// pinpoint-rescrape "wrong match" escape hatch. `media_type` for the
/// eventual `details_by_id` call always comes from the entry's own
/// `MediaKind` (movie vs. episode's show), not from parsing the URL, so a
/// mismatched `.../tv/...` URL pasted for a movie entry still resolves —
/// callers that want that validated should compare `resolve_id`'s success
/// against their own expectations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmdbOverride {
    /// A bare numeric TMDb id.
    Id(u64),
    /// A pasted TMDb URL, e.g. `https://www.themoviedb.org/movie/27205-inception`
    /// or `.../tv/1396-breaking-bad`. Parsed into an id at scrape time.
    Url(String),
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum TmdbOverrideError {
    #[error("could not find a TMDb id in \"{0}\"")]
    UnparseableUrl(String),
}

impl TmdbOverride {
    /// Resolve to a bare numeric id, parsing a URL if necessary. Never
    /// panics on malformed input — returns a typed error instead.
    pub fn resolve_id(&self) -> Result<u64, TmdbOverrideError> {
        match self {
            TmdbOverride::Id(id) => Ok(*id),
            TmdbOverride::Url(url) => {
                parse_tmdb_url_id(url).ok_or_else(|| TmdbOverrideError::UnparseableUrl(url.clone()))
            }
        }
    }
}

/// Extracts the numeric id from a TMDb URL shaped like
/// `.../movie/27205-inception`, `.../tv/1396-breaking-bad`, or either form
/// with no slug (`.../movie/27205`) or a trailing slash.
fn parse_tmdb_url_id(url: &str) -> Option<u64> {
    let after_host = url.split_once("themoviedb.org")?.1;
    let mut segments = after_host.trim_matches('/').split('/');
    let media_segment = segments.next()?;
    if media_segment != "movie" && media_segment != "tv" {
        return None;
    }
    let id_segment = segments.next()?;
    let digits: String = id_segment
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// TMDb issues two different credential shapes from the same account
/// settings page, and it's an easy real-world mix-up (confirmed against a
/// real user's key): the legacy v3 "API Key" is a 32-character lowercase
/// hex string sent as an `api_key` query parameter, while the newer v4 "API
/// Read Access Token" is a JWT (three base64url segments joined by `.`,
/// e.g. `eyJhbG....<payload>....<signature>`) sent as `Authorization:
/// Bearer <token>` — passing a v4 token as `api_key` is rejected with a
/// plain 401 and no hint about which credential type was expected. Detect
/// by shape rather than making the caller know the difference: a v3 key is
/// always exactly 32 hex characters, which can never contain a `.`, so
/// "contains at least two `.` characters" cleanly separates the two — a
/// JWT always has exactly two dots (header.payload.signature) and a v3 key
/// structurally can never have any.
fn is_v4_read_access_token(key: &str) -> bool {
    key.bytes().filter(|&b| b == b'.').count() >= 2
}

pub struct TmdbClient {
    http: reqwest::Client,
    api_base: String,
    image_base: String,
    api_key: String,
    bearer_token: bool,
}

impl TmdbClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_urls(api_key, DEFAULT_API_BASE, DEFAULT_IMAGE_BASE)
    }

    pub fn with_base_urls(
        api_key: impl Into<String>,
        api_base: impl Into<String>,
        image_base: impl Into<String>,
    ) -> Self {
        let api_key = api_key.into();
        let bearer_token = is_v4_read_access_token(&api_key);
        Self {
            http: reqwest::Client::new(),
            api_base: api_base.into(),
            image_base: image_base.into(),
            api_key,
            bearer_token,
        }
    }

    /// Applies this client's detected auth style to a request: a v4 token as
    /// a Bearer header (and no `api_key` param at all — TMDb doesn't need
    /// both and a stray `api_key=<jwt>` on a v4-authed request is itself
    /// sometimes rejected), a v3 key as the traditional `api_key` param.
    fn authed(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.bearer_token {
            request.bearer_auth(&self.api_key)
        } else {
            request.query(&[("api_key", self.api_key.as_str())])
        }
    }

    /// `year`, when known (see `EntryRecord::year` / `classify::extract_bracket_tags`),
    /// disambiguates a remake/franchise search — TMDb's `/search/movie` takes
    /// a `year` filter directly. TV search has no equivalent used here.
    ///
    /// TMDb's `year` param is a hard filter, not just a ranking hint — it
    /// can genuinely exclude the correct movie when TMDb's own recorded
    /// release year disagrees with the year in the filename by one (a
    /// festival premiere vs. a wide release date, or a distributor's
    /// copyright year vs. release year — a real, known TMDb quirk, not
    /// hypothetical). So: if the year-filtered search comes back with
    /// nothing, retry once with no year filter at all before giving up —
    /// `pick_best_result`'s own exact-title-match scoring still does the
    /// real disambiguation work on the wider result set, this just stops a
    /// single off-by-one year from being an unconditional dead end.
    pub async fn search_and_fetch_movie(
        &self,
        title: &str,
        year: Option<u32>,
    ) -> Result<ScrapedVideo, TmdbError> {
        let id = match self.search(title, "movie", year).await {
            Err(TmdbError::NotFound) if year.is_some() => self.search(title, "movie", None).await?,
            other => other?,
        };
        self.details_by_id(id, "movie").await
    }

    pub async fn search_and_fetch_tv(&self, title: &str) -> Result<ScrapedVideo, TmdbError> {
        let id = self.search(title, "tv", None).await?;
        self.details_by_id(id, "tv").await
    }

    /// Looks up a bare title with no year hint and returns TMDb's release
    /// year **only** when exactly one confidently-matched result exists —
    /// used to backfill a canonical path's year when `classify` found a
    /// movie title but no year in the filename (issue #297). Unlike
    /// `search_and_fetch_movie`/`pick_best_result`, which always fall back
    /// to *some* best guess (TMDb's own top-ranked result when nothing is an
    /// exact title match), this never guesses: zero exact-title matches,
    /// more than one (a remake sharing its predecessor's bare title, e.g.
    /// two entries both literally titled "Scream"), or a lone match too
    /// obscure to trust (near-zero vote count, the same thin/likely-wrong
    /// community entry `pick_best_result`'s doc comment warns about) all
    /// return `Ok(None)` rather than an unreliable year.
    pub async fn confident_movie_year(&self, title: &str) -> Result<Option<u32>, TmdbError> {
        let url = format!("{}/search/movie", self.api_base);
        let response = self
            .authed(self.http.get(&url).query(&[("query", title)]))
            .send()
            .await
            .map_err(|e| TmdbError::Unavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(TmdbError::Unavailable(format!(
                "search returned {}",
                response.status()
            )));
        }
        let body: SearchResponse = response
            .json()
            .await
            .map_err(|e| TmdbError::Unavailable(e.to_string()))?;

        let norm_query = normalize_for_match(title);
        let mut exact_matches = body.results.iter().filter(|hit| {
            let candidate_title = hit.title.as_deref().or(hit.name.as_deref()).unwrap_or_default();
            let original_title = hit
                .original_title
                .as_deref()
                .or(hit.original_name.as_deref())
                .unwrap_or_default();
            normalize_for_match(candidate_title) == norm_query || normalize_for_match(original_title) == norm_query
        });

        let Some(hit) = exact_matches.next() else {
            return Ok(None);
        };
        if exact_matches.next().is_some() {
            return Ok(None);
        }

        const MIN_CONFIDENT_VOTE_COUNT: u64 = 20;
        if hit.vote_count.unwrap_or(0) < MIN_CONFIDENT_VOTE_COUNT {
            return Ok(None);
        }

        Ok(hit
            .release_date
            .as_deref()
            .and_then(|d| d.get(0..4))
            .and_then(|y| y.parse::<u32>().ok()))
    }

    /// Fetches the season poster and each episode's still image from TMDB's
    /// season endpoint. Those fields are not part of `/tv/{id}`, which is
    /// why treating show details as episode details produced the same show
    /// poster/backdrop on every season and episode.
    pub async fn season_details(
        &self,
        show_id: u64,
        season_number: u32,
    ) -> Result<ScrapedSeason, TmdbError> {
        let url = format!("{}/tv/{show_id}/season/{season_number}", self.api_base);
        let response = self
            .authed(self.http.get(&url).query(&[("language", "en-US")]))
            .send()
            .await
            .map_err(|e| TmdbError::Unavailable(e.to_string()))?;
        if response.status().as_u16() == 404 {
            return Err(TmdbError::NotFound);
        }
        if !response.status().is_success() {
            return Err(TmdbError::Unavailable(format!(
                "season details returned {}",
                response.status()
            )));
        }
        let body: SeasonDetailsResponse = response
            .json()
            .await
            .map_err(|e| TmdbError::Unavailable(e.to_string()))?;
        Ok(ScrapedSeason {
            poster_url: body
                .poster_path
                .map(|p| format!("{}/w342{p}", self.image_base)),
            episode_still_urls: body
                .episodes
                .iter()
                .filter_map(|episode| {
                    episode.still_path.clone().map(|path| {
                        (
                            episode.episode_number,
                            format!("{}/w780{path}", self.image_base),
                        )
                    })
                })
                .collect(),
            episode_details: body
                .episodes
                .into_iter()
                .map(|episode| {
                    let still_url = episode
                        .still_path
                        .map(|path| format!("{}/w780{path}", self.image_base));
                    (
                        episode.episode_number,
                        ScrapedEpisode {
                            title: episode.name,
                            overview: episode.overview.filter(|value| !value.trim().is_empty()),
                            still_url,
                        },
                    )
                })
                .collect(),
        })
    }

    async fn search(
        &self,
        query: &str,
        media_type: &str,
        year: Option<u32>,
    ) -> Result<u64, TmdbError> {
        let url = format!("{}/search/{media_type}", self.api_base);
        let year_str = year.map(|y| y.to_string());
        let mut params = vec![("query", query)];
        if media_type == "movie" {
            if let Some(year_str) = &year_str {
                params.push(("year", year_str.as_str()));
            }
        }
        let response = self
            .authed(self.http.get(&url).query(&params))
            .send()
            .await
            .map_err(|e| TmdbError::Unavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(TmdbError::Unavailable(format!(
                "search returned {}",
                response.status()
            )));
        }
        let body: SearchResponse = response
            .json()
            .await
            .map_err(|e| TmdbError::Unavailable(e.to_string()))?;
        pick_best_result(&body.results, query, year)
            .map(|hit| hit.id)
            .ok_or(TmdbError::NotFound)
    }

    /// Fetch details for a known TMDb id directly, skipping search entirely
    /// — the manual-URL-override path (see [`TmdbOverride`]) as well as the
    /// tail end of the normal search-then-fetch flow both land here.
    pub async fn details_by_id(
        &self,
        id: u64,
        media_type: &str,
    ) -> Result<ScrapedVideo, TmdbError> {
        let url = format!("{}/{media_type}/{id}", self.api_base);
        // One request, not several: TMDb folds each sub-resource into the
        // main details payload under its own key when asked. The
        // certification sub-resource is named differently per media type
        // (a movie has no `content_ratings` method, a show has no
        // `release_dates` one) so it's picked based on what's being fetched
        // rather than requesting both unconditionally.
        let append = if media_type == "movie" {
            "credits,release_dates"
        } else {
            "credits,content_ratings"
        };
        let response = self
            .authed(self.http.get(&url).query(&[("append_to_response", append)]))
            .send()
            .await
            .map_err(|e| TmdbError::Unavailable(e.to_string()))?;
        if response.status().as_u16() == 404 {
            return Err(TmdbError::NotFound);
        }
        if !response.status().is_success() {
            return Err(TmdbError::Unavailable(format!(
                "details returned {}",
                response.status()
            )));
        }
        let body: DetailsResponse = response
            .json()
            .await
            .map_err(|e| TmdbError::Unavailable(e.to_string()))?;
        // TMDb already orders cast by billing; keep only the headline names.
        let cast = body
            .credits
            .map(|c| {
                c.cast
                    .into_iter()
                    .take(10)
                    .map(|m| CastMember {
                        name: m.name,
                        character: m.character,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(ScrapedVideo {
            tmdb_id: id,
            title: body.title.or(body.name).unwrap_or_default(),
            genres: body.genres.into_iter().map(|g| g.name).collect(),
            // w342, not TMDb's larger w500: posters are the one artwork kind
            // fetched constantly at small display sizes (every browse-grid
            // card, ~130dp wide on the TV client), not just the odd full-size
            // detail view — real complaint from live use, artwork "not
            // loading quickly" traced to every one of dozens of grid
            // thumbnails pulling a full w500 JPEG through the peer-QUIC/
            // loopback-proxy hop and decoding it just to shrink it back down
            // on screen. w342 is still comfortably sharp for the largest
            // place a poster renders today (this app's own ~190-200dp detail
            // view), while meaningfully lighter for every small grid card.
            poster_url: body
                .poster_path
                .map(|p| format!("{}/w342{p}", self.image_base)),
            backdrop_url: body
                .backdrop_path
                .map(|p| format!("{}/w1280{p}", self.image_base)),
            season_poster_url: None,
            cast,
            overview: body.overview.filter(|o| !o.is_empty()),
            // Only one of these two is ever populated for a given
            // `media_type` (see the `append` selection above) — chaining
            // them with `or_else` finds whichever one applies without the
            // caller needing to branch on media type again here.
            certification: body
                .release_dates
                .as_ref()
                .and_then(|w| w.results.iter().find(|c| c.iso_3166_1 == "US"))
                .and_then(|c| {
                    c.release_dates
                        .iter()
                        .map(|r| r.certification.as_str())
                        .find(|c| !c.is_empty())
                })
                .map(str::to_string)
                .or_else(|| {
                    body.content_ratings
                        .as_ref()
                        .and_then(|w| w.results.iter().find(|c| c.iso_3166_1 == "US"))
                        .map(|c| c.rating.clone())
                        .filter(|r| !r.is_empty())
                }),
            community_rating: (body.vote_count.unwrap_or(0) > 0)
                .then_some(body.vote_average)
                .flatten()
                .filter(|rating| rating.is_finite() && (0.0..=10.0).contains(rating)),
            community_rating_votes: body.vote_count.filter(|votes| *votes > 0),
            episode_title: None,
            episode_overview: None,
        })
    }
}

/// Picks the best match from a TMDb search result page instead of blindly
/// trusting `results[0]`. **The real bug this fixes**: TMDb's search
/// endpoint doesn't guarantee its top hit is a title match — for an
/// ambiguous or lightly-populated query (a sequel written as "Blade 2"
/// against a DB entry titled "Blade II", a title TMDb has multiple
/// same-named-ish entries for) the previous `results.into_iter().next()`
/// could silently return a completely unrelated, obscure, poorly-curated
/// film — confirmed against real scrape output where the returned "cast"
/// was for a different movie entirely (alphabetically-listed names, a sign
/// of a thin community-contributed entry with no real billing-order data,
/// not the film that was actually being searched for). Preference order:
/// (1) a result whose title normalizes to an exact match against the query
/// (roman-numeral/arabic-numeral equivalence included, so "Blade 2" matches
/// a "Blade II" entry), (2) among ties, a matching release/first-air year
/// when one was supplied, (3) among remaining ties, higher `popularity`
/// (TMDb's own relevance signal) — first-seen (TMDb's own ranking) wins any
/// remaining tie, so this never overrides TMDb's own ranking when nothing
/// above distinguishes two results.
fn pick_best_result<'a>(
    results: &'a [SearchHit],
    query: &str,
    year: Option<u32>,
) -> Option<&'a SearchHit> {
    let norm_query = normalize_for_match(query);
    let mut best: Option<(&SearchHit, (u8, u8, f64))> = None;
    for hit in results {
        let candidate_title = hit
            .title
            .as_deref()
            .or(hit.name.as_deref())
            .unwrap_or_default();
        let original_title = hit
            .original_title
            .as_deref()
            .or(hit.original_name.as_deref())
            .unwrap_or_default();
        let exact = (normalize_for_match(candidate_title) == norm_query
            || normalize_for_match(original_title) == norm_query) as u8;
        let candidate_year = hit
            .release_date
            .as_deref()
            .or(hit.first_air_date.as_deref())
            .and_then(|d| d.get(0..4))
            .and_then(|y| y.parse::<u32>().ok());
        let year_match = year.is_some_and(|y| candidate_year == Some(y)) as u8;
        let score = (exact, year_match, hit.popularity.unwrap_or(0.0));
        if best
            .as_ref()
            .is_none_or(|(_, best_score)| score > *best_score)
        {
            best = Some((hit, score));
        }
    }
    best.map(|(hit, _)| hit)
}

/// Lowercase, strip everything but alphanumerics (so punctuation/spacing
/// differences never block a match), and map whole-word roman numerals
/// II-X to their arabic digit — scene-release filenames overwhelmingly use
/// digits for a sequel ("Blade 2") while TMDb's canonical title often uses
/// a roman numeral ("Blade II"); without this normalization those two
/// never compare equal and the exact-match preference in
/// [pick_best_result] never kicks in for exactly the ambiguous case it
/// exists for.
fn normalize_for_match(s: &str) -> String {
    const ROMAN: &[(&str, &str)] = &[
        ("ii", "2"),
        ("iii", "3"),
        ("iv", "4"),
        ("v", "5"),
        ("vi", "6"),
        ("vii", "7"),
        ("viii", "8"),
        ("ix", "9"),
        ("x", "10"),
    ];
    let cleaned: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    cleaned
        .split_whitespace()
        .map(|word| {
            ROMAN
                .iter()
                .find(|(roman, _)| *roman == word)
                .map(|(_, digit)| *digit)
                .unwrap_or(word)
        })
        .collect::<Vec<_>>()
        .join("")
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    id: u64,
    title: Option<String>, // movies
    name: Option<String>,  // tv
    original_title: Option<String>,
    original_name: Option<String>,
    release_date: Option<String>,   // movies, "YYYY-MM-DD"
    first_air_date: Option<String>, // tv, "YYYY-MM-DD"
    popularity: Option<f64>,
    vote_count: Option<u64>,
}

#[derive(Deserialize)]
struct DetailsResponse {
    title: Option<String>, // movies
    name: Option<String>,  // tv
    #[serde(default)]
    genres: Vec<Genre>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    overview: Option<String>,
    /// Present because of `append_to_response=credits`.
    credits: Option<CreditsResponse>,
    /// Present on a movie fetch (`append_to_response=...,release_dates`);
    /// absent on a tv fetch.
    release_dates: Option<ReleaseDatesResponse>,
    /// Present on a tv fetch (`append_to_response=...,content_ratings`);
    /// absent on a movie fetch.
    content_ratings: Option<ContentRatingsResponse>,
    vote_average: Option<f64>,
    vote_count: Option<u64>,
}

#[derive(Deserialize)]
struct SeasonDetailsResponse {
    poster_path: Option<String>,
    #[serde(default)]
    episodes: Vec<SeasonEpisodeResponse>,
}

#[derive(Deserialize)]
struct SeasonEpisodeResponse {
    episode_number: u32,
    #[serde(default)]
    name: String,
    overview: Option<String>,
    still_path: Option<String>,
}

#[derive(Deserialize)]
struct Genre {
    name: String,
}

#[derive(Deserialize)]
struct CreditsResponse {
    #[serde(default)]
    cast: Vec<TmdbCastMember>,
}

#[derive(Deserialize)]
struct TmdbCastMember {
    name: String,
    character: Option<String>,
}

/// `GET /movie/{id}?append_to_response=release_dates` shape — one entry per
/// country, each with its own list of release events (a title can have
/// several — theatrical, digital, ...), any of which may carry a
/// certification.
#[derive(Deserialize)]
struct ReleaseDatesResponse {
    #[serde(default)]
    results: Vec<ReleaseDatesCountry>,
}

#[derive(Deserialize)]
struct ReleaseDatesCountry {
    iso_3166_1: String,
    #[serde(default)]
    release_dates: Vec<ReleaseDateEntry>,
}

#[derive(Deserialize)]
struct ReleaseDateEntry {
    #[serde(default)]
    certification: String,
}

/// `GET /tv/{id}?append_to_response=content_ratings` shape — one rating per
/// country, unlike movies' per-release-event list.
#[derive(Deserialize)]
struct ContentRatingsResponse {
    #[serde(default)]
    results: Vec<ContentRatingsCountry>,
}

#[derive(Deserialize)]
struct ContentRatingsCountry {
    iso_3166_1: String,
    #[serde(default)]
    rating: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    async fn spawn_mock(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn search_and_fetch_movie_success() {
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async { Json(json!({"results": [{"id": 27205}]})) }),
            )
            .route(
                "/movie/27205",
                get(|| async {
                    Json(json!({
                        "title": "Inception", "genres": [{"id": 1, "name": "Sci-Fi"}],
                        "poster_path": "/poster.jpg", "backdrop_path": "/backdrop.jpg",
                        "credits": {"cast": [
                            {"name": "Leonardo DiCaprio", "character": "Cobb"},
                            {"name": "Ellen Page", "character": "Ariadne"}
                        ]}
                    }))
                }),
            );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, "https://image.tmdb.org/t/p");
        let result = client
            .search_and_fetch_movie("Inception", None)
            .await
            .unwrap();
        assert_eq!(result.title, "Inception");
        assert_eq!(result.genres, vec!["Sci-Fi"]);
        assert_eq!(
            result.poster_url.as_deref(),
            Some("https://image.tmdb.org/t/p/w342/poster.jpg")
        );
        assert_eq!(result.cast.len(), 2);
        assert_eq!(result.cast[0].name, "Leonardo DiCaprio");
        assert_eq!(result.cast[0].character.as_deref(), Some("Cobb"));
    }

    #[tokio::test]
    async fn movie_certification_is_read_from_the_us_release_dates_entry() {
        let router = Router::new()
            .route("/search/movie", get(|| async { Json(json!({"results": [{"id": 27205}]})) }))
            .route(
                "/movie/27205",
                get(|| async {
                    Json(json!({
                        "title": "Inception",
                        "vote_average": 8.4,
                        "vote_count": 36000,
                        "release_dates": {"results": [
                            {"iso_3166_1": "FR", "release_dates": [{"certification": "12"}]},
                            {"iso_3166_1": "US", "release_dates": [{"certification": ""}, {"certification": "PG-13"}]}
                        ]}
                    }))
                }),
            );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client
            .search_and_fetch_movie("Inception", None)
            .await
            .unwrap();
        assert_eq!(result.certification.as_deref(), Some("PG-13"));
        assert_eq!(result.community_rating, Some(8.4));
        assert_eq!(result.community_rating_votes, Some(36_000));
    }

    #[tokio::test]
    async fn tv_certification_is_read_from_the_us_content_ratings_entry() {
        let router = Router::new()
            .route(
                "/search/tv",
                get(|| async { Json(json!({"results": [{"id": 1396}]})) }),
            )
            .route(
                "/tv/1396",
                get(|| async {
                    Json(json!({
                        "name": "Breaking Bad",
                        "content_ratings": {"results": [
                            {"iso_3166_1": "DE", "rating": "16"},
                            {"iso_3166_1": "US", "rating": "TV-MA"}
                        ]}
                    }))
                }),
            );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client.search_and_fetch_tv("Breaking Bad").await.unwrap();
        assert_eq!(result.certification.as_deref(), Some("TV-MA"));
    }

    #[tokio::test]
    async fn season_details_returns_distinct_season_poster_and_episode_stills() {
        let router = Router::new().route(
            "/tv/1433/season/2",
            get(|| async {
                Json(json!({
                    "name": "Season 2",
                    "season_number": 2,
                    "poster_path": "/american-dad-season-2.jpg",
                    "episodes": [
                        {"episode_number": 1, "name": "The One", "overview": "First.", "still_path": "/episode-1.jpg"},
                        {"episode_number": 2, "name": "The Two", "still_path": "/episode-2.jpg"},
                        {"episode_number": 3, "name": "The Three", "still_path": null}
                    ]
                }))
            }),
        );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, "https://image.tmdb.org/t/p");
        let season = client.season_details(1433, 2).await.unwrap();

        assert_eq!(
            season.poster_url.as_deref(),
            Some("https://image.tmdb.org/t/p/w342/american-dad-season-2.jpg")
        );
        assert_eq!(
            season.episode_still_urls.get(&1).map(String::as_str),
            Some("https://image.tmdb.org/t/p/w780/episode-1.jpg")
        );
        assert_eq!(
            season.episode_still_urls.get(&2).map(String::as_str),
            Some("https://image.tmdb.org/t/p/w780/episode-2.jpg")
        );
        assert!(!season.episode_still_urls.contains_key(&3));
        assert_eq!(season.episode_details.get(&1).unwrap().title, "The One");
        assert_eq!(season.episode_details.get(&1).unwrap().overview.as_deref(), Some("First."));
        assert_eq!(season.episode_details.get(&3).unwrap().title, "The Three");
    }

    #[tokio::test]
    async fn missing_certification_data_is_none_not_an_error() {
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async { Json(json!({"results": [{"id": 1}]})) }),
            )
            .route(
                "/movie/1",
                get(|| async { Json(json!({"title": "No Ratings Data"})) }),
            );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client
            .search_and_fetch_movie("No Ratings Data", None)
            .await
            .unwrap();
        assert_eq!(result.certification, None);
    }

    #[tokio::test]
    async fn missing_credits_block_is_an_empty_cast_not_an_error() {
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async { Json(json!({"results": [{"id": 1}]})) }),
            )
            .route(
                "/movie/1",
                get(|| async { Json(json!({"title": "No Credits"})) }),
            );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client
            .search_and_fetch_movie("No Credits", None)
            .await
            .unwrap();
        assert!(result.cast.is_empty());
    }

    #[tokio::test]
    async fn empty_search_results_is_not_found() {
        let router = Router::new().route(
            "/search/movie",
            get(|| async { Json(json!({"results": []})) }),
        );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        assert!(matches!(
            client.search_and_fetch_movie("Nope", None).await,
            Err(TmdbError::NotFound)
        ));
    }

    #[tokio::test]
    async fn server_error_is_unavailable_not_not_found() {
        let router = Router::new().route(
            "/search/movie",
            get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        assert!(matches!(
            client.search_and_fetch_movie("X", None).await,
            Err(TmdbError::Unavailable(_))
        ));
    }

    // --- pick_best_result: the real "wrong movie matched" bug ---
    // Root cause confirmed by inspection: search() used to take
    // `results.into_iter().next()` unconditionally, so an ambiguous or
    // lightly-populated query could silently land on a completely
    // unrelated film ranked first by TMDb's own relevance scoring but not
    // an actual title match. These tests put the *wrong* movie first in
    // the mocked results list and confirm the *right* one now wins.

    #[tokio::test]
    async fn exact_title_match_is_preferred_over_a_higher_ranked_decoy() {
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async {
                    Json(json!({"results": [
                        {"id": 1, "title": "Some Unrelated Obscure Film", "popularity": 50.0},
                        {"id": 2, "title": "Heat", "popularity": 1.0}
                    ]}))
                }),
            )
            .route("/movie/2", get(|| async { Json(json!({"title": "Heat"})) }));
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client.search_and_fetch_movie("Heat", None).await.unwrap();
        assert_eq!(result.title, "Heat");
    }

    #[tokio::test]
    async fn roman_numeral_title_matches_an_arabic_numeral_query() {
        // The real "Blade 2" bug: the scraped query uses a digit, TMDb's
        // canonical title uses a roman numeral.
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async {
                    Json(json!({"results": [
                        {"id": 1, "title": "Some Decoy Blade Movie", "popularity": 99.0},
                        {"id": 2, "title": "Blade II", "popularity": 5.0}
                    ]}))
                }),
            )
            .route(
                "/movie/2",
                get(|| async { Json(json!({"title": "Blade II"})) }),
            );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client
            .search_and_fetch_movie("Blade 2", None)
            .await
            .unwrap();
        assert_eq!(result.title, "Blade II");
    }

    #[tokio::test]
    async fn year_disambiguates_between_two_same_named_results() {
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async {
                    Json(json!({"results": [
                        {"id": 1, "title": "Total Recall", "release_date": "1990-06-01", "popularity": 20.0},
                        {"id": 2, "title": "Total Recall", "release_date": "2012-08-01", "popularity": 5.0}
                    ]}))
                }),
            )
            .route("/movie/2", get(|| async { Json(json!({"title": "Total Recall (2012)"})) }));
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client
            .search_and_fetch_movie("Total Recall", Some(2012))
            .await
            .unwrap();
        assert_eq!(result.title, "Total Recall (2012)");
    }

    #[tokio::test]
    async fn a_year_filtered_search_with_no_results_retries_without_the_year() {
        // TMDb's `year` param is a hard filter — if it excludes the movie
        // entirely (a real, known off-by-one-year TMDb quirk), a second,
        // unfiltered attempt must still find it rather than giving up.
        let router = Router::new()
            .route(
                "/search/movie",
                get(
                    |axum::extract::Query(params): axum::extract::Query<
                        std::collections::HashMap<String, String>,
                    >| async move {
                        if params.contains_key("year") {
                            Json(json!({"results": []}))
                        } else {
                            Json(json!({"results": [{"id": 1, "title": "Shaun of the Dead"}]}))
                        }
                    },
                ),
            )
            .route(
                "/movie/1",
                get(|| async { Json(json!({"title": "Shaun of the Dead"})) }),
            );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client
            .search_and_fetch_movie("Shaun of the Dead", Some(2003))
            .await
            .unwrap();
        assert_eq!(result.title, "Shaun of the Dead");
    }

    #[tokio::test]
    async fn no_exact_match_falls_back_to_tmdbs_own_top_result() {
        // Nothing in the result set is an exact title match — TMDb's own
        // ranking (first result) is trusted as the last resort, same as the
        // pre-existing behavior for this case.
        let router = Router::new()
            .route(
                "/search/movie",
                get(|| async {
                    Json(json!({"results": [{"id": 1, "title": "Loosely Similar Title"}, {"id": 2, "title": "Another One"}]}))
                }),
            )
            .route("/movie/1", get(|| async { Json(json!({"title": "Loosely Similar Title"})) }));
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client
            .search_and_fetch_movie("Something Else Entirely", None)
            .await
            .unwrap();
        assert_eq!(result.title, "Loosely Similar Title");
    }

    #[tokio::test]
    async fn year_hint_is_forwarded_to_movie_search() {
        let router = Router::new()
            .route(
                "/search/movie",
                get(
                    |axum::extract::Query(params): axum::extract::Query<
                        std::collections::HashMap<String, String>,
                    >| async move {
                        assert_eq!(params.get("year").map(String::as_str), Some("1995"));
                        Json(json!({"results": [{"id": 949}]}))
                    },
                ),
            )
            .route(
                "/movie/949",
                get(|| async { Json(json!({"title": "Heat"})) }),
            );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let result = client
            .search_and_fetch_movie("Heat", Some(1995))
            .await
            .unwrap();
        assert_eq!(result.title, "Heat");
    }

    #[tokio::test]
    async fn v3_hex_key_is_sent_as_an_api_key_query_param() {
        let router = Router::new()
            .route(
                "/search/movie",
                get(
                    |axum::extract::Query(params): axum::extract::Query<
                        std::collections::HashMap<String, String>,
                    >,
                     headers: axum::http::HeaderMap| async move {
                        assert_eq!(
                            params.get("api_key").map(String::as_str),
                            Some("0123456789abcdef0123456789abcdef")
                        );
                        assert!(!headers.contains_key(axum::http::header::AUTHORIZATION));
                        Json(json!({"results": [{"id": 1}]}))
                    },
                ),
            )
            .route("/movie/1", get(|| async { Json(json!({"title": "V3"})) }));
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("0123456789abcdef0123456789abcdef", &base, &base);
        assert_eq!(
            client
                .search_and_fetch_movie("V3", None)
                .await
                .unwrap()
                .title,
            "V3"
        );
    }

    #[tokio::test]
    async fn v4_jwt_token_is_sent_as_a_bearer_header_not_a_query_param() {
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.signature";
        let router = Router::new()
            .route(
                "/search/movie",
                get(
                    |axum::extract::Query(params): axum::extract::Query<
                        std::collections::HashMap<String, String>,
                    >,
                     headers: axum::http::HeaderMap| async move {
                        assert!(
                            !params.contains_key("api_key"),
                            "a v4 token must not also be sent as api_key"
                        );
                        assert_eq!(
                            headers
                                .get(axum::http::header::AUTHORIZATION)
                                .and_then(|v| v.to_str().ok()),
                            Some("Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.signature")
                        );
                        Json(json!({"results": [{"id": 2}]}))
                    },
                ),
            )
            .route("/movie/2", get(|| async { Json(json!({"title": "V4"})) }));
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls(token, &base, &base);
        assert_eq!(
            client
                .search_and_fetch_movie("V4", None)
                .await
                .unwrap()
                .title,
            "V4"
        );
    }

    // --- confident_movie_year (issue #297: backfill a missing year) ---

    #[tokio::test]
    async fn confident_movie_year_returns_the_year_for_a_single_well_known_exact_match() {
        let router = Router::new().route(
            "/search/movie",
            get(|| async {
                Json(json!({"results": [
                    {"id": 348, "title": "Alien", "release_date": "1979-05-25", "popularity": 40.0, "vote_count": 12000}
                ]}))
            }),
        );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let year = client.confident_movie_year("Alien").await.unwrap();
        assert_eq!(year, Some(1979));
    }

    #[tokio::test]
    async fn confident_movie_year_is_none_for_multiple_exact_title_matches() {
        // Real franchise ambiguity: a remake sharing its predecessor's bare
        // title. Nothing in a filename-derived title alone disambiguates
        // which release the file actually is, so this must not guess.
        let router = Router::new().route(
            "/search/movie",
            get(|| async {
                Json(json!({"results": [
                    {"id": 1, "title": "Scream", "release_date": "1996-12-20", "popularity": 30.0, "vote_count": 8000},
                    {"id": 2, "title": "Scream", "release_date": "2022-01-14", "popularity": 25.0, "vote_count": 4000}
                ]}))
            }),
        );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let year = client.confident_movie_year("Scream").await.unwrap();
        assert_eq!(year, None);
    }

    #[tokio::test]
    async fn confident_movie_year_is_none_when_no_result_is_an_exact_title_match() {
        let router = Router::new().route(
            "/search/movie",
            get(|| async {
                Json(json!({"results": [
                    {"id": 1, "title": "Loosely Similar Title", "release_date": "2001-01-01", "popularity": 10.0, "vote_count": 500}
                ]}))
            }),
        );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let year = client.confident_movie_year("Something Else Entirely").await.unwrap();
        assert_eq!(year, None);
    }

    #[tokio::test]
    async fn confident_movie_year_is_none_for_an_obscure_low_vote_match() {
        let router = Router::new().route(
            "/search/movie",
            get(|| async {
                Json(json!({"results": [
                    {"id": 1, "title": "Obscure Thing", "release_date": "2001-01-01", "popularity": 0.6, "vote_count": 1}
                ]}))
            }),
        );
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let year = client.confident_movie_year("Obscure Thing").await.unwrap();
        assert_eq!(year, None);
    }

    #[tokio::test]
    async fn confident_movie_year_is_none_for_empty_search_results() {
        let router = Router::new().route("/search/movie", get(|| async { Json(json!({"results": []})) }));
        let base = spawn_mock(router).await;
        let client = TmdbClient::with_base_urls("key", &base, &base);
        let year = client.confident_movie_year("Nope").await.unwrap();
        assert_eq!(year, None);
    }

    #[test]
    fn resolves_a_bare_id_override_without_parsing() {
        assert_eq!(TmdbOverride::Id(27205).resolve_id(), Ok(27205));
    }

    #[test]
    fn parses_id_from_a_full_movie_url_with_slug() {
        assert_eq!(
            TmdbOverride::Url("https://www.themoviedb.org/movie/27205-inception".into())
                .resolve_id(),
            Ok(27205)
        );
    }

    #[test]
    fn parses_id_from_a_tv_url_with_no_slug_and_trailing_slash() {
        assert_eq!(
            TmdbOverride::Url("https://www.themoviedb.org/tv/1396/".into()).resolve_id(),
            Ok(1396)
        );
    }

    #[test]
    fn malformed_override_url_is_a_clean_typed_error_not_a_panic() {
        assert_eq!(
            TmdbOverride::Url("https://example.com/not-tmdb".into()).resolve_id(),
            Err(TmdbOverrideError::UnparseableUrl(
                "https://example.com/not-tmdb".into()
            ))
        );
        assert!(
            TmdbOverride::Url("https://www.themoviedb.org/movie/not-a-number".into())
                .resolve_id()
                .is_err()
        );
        assert!(TmdbOverride::Url("garbage".into()).resolve_id().is_err());
    }
}
