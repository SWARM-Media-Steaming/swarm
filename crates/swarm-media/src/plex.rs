//! Centralized **Plex media-organization compatibility layer**.
//!
//! The single source of truth for "what does Plex currently consider a valid
//! layout / name / local asset" — shared by the library scanner
//! ([`crate::scan`] via [`crate::classify`]), subtitle discovery
//! ([`crate::subtitles`]), the metadata scraper ([`crate::scrape`]), the
//! repair/reorganization planner (`apps/server/src/reorganize.rs`), and any
//! future importer.
//!
//! Design rules (from issue #247):
//! - **Plex wins.** Where existing SWARM behavior conflicts with a currently
//!   supported Plex convention, this module encodes the Plex behavior and the
//!   caller adopts it.
//! - **Accept every valid Plex layout.** Never reject a valid Plex structure
//!   just because it differs from SWARM's preferred canonical layout.
//! - **Deterministic.** Everything here is pure path/text logic with no
//!   filesystem or database access, so callers (including the AI helper) can
//!   consume the results as ground truth.
//!
//! Sources: Plex Support — "Naming and organizing your Movie/TV Show/Music
//! files", "Local Files for TV Show Trailers and Extras", "Adding Local
//! Subtitles to Your Media", "Movie/Show Specific Naming" (agent GUIDs and
//! editions). Current as of the January 2026 documentation.

/// A Plex "match this exact item" identifier embedded in a file or folder
/// name as `{agent-id}` — e.g. `{tmdb-27205}`, `{imdb-tt1375666}`,
/// `{tvdb-121361}`. Plex uses these to skip its fuzzy matcher entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexGuid {
    /// Lowercase agent name: `tmdb`, `imdb`, `tvdb`, `anidb`, `mbid`.
    pub agent: String,
    /// The raw id exactly as written (`27205`, `tt1375666`, `121361`).
    pub id: String,
}

impl PlexGuid {
    /// `"tmdb-27205"` — the stable token form stored on a catalog entry and
    /// handed to the scraper.
    pub fn token(&self) -> String {
        format!("{}-{}", self.agent, self.id)
    }
}

/// Agent names Plex accepts inside a `{agent-id}` token. `mbid` is
/// MusicBrainz (music libraries); the rest are video.
const GUID_AGENTS: &[&str] = &["tmdb", "imdb", "tvdb", "anidb", "mbid", "tsdb", "tvrage"];

/// Parse the first `{agent-id}` token in `name` (a file stem or folder
/// name). Case-insensitive on the agent; the id keeps its original case
/// (imdb ids are `tt`-prefixed). Returns `None` when there is no such token
/// or the agent is not one Plex recognizes (so `{edition-...}` and
/// decorative `{web-dl}`-style tags are never mistaken for an id).
pub fn parse_guid(name: &str) -> Option<PlexGuid> {
    for (start, _) in name.match_indices('{') {
        let rest = &name[start + 1..];
        let end = rest.find('}')?;
        let inner = &rest[..end];
        let (agent, id) = inner.split_once('-')?;
        let agent = agent.trim().to_lowercase();
        let id = id.trim();
        if !id.is_empty() && GUID_AGENTS.contains(&agent.as_str()) {
            return Some(PlexGuid {
                agent,
                id: id.to_string(),
            });
        }
    }
    None
}

/// The edition label from a `{edition-<label>}` token (Plex "Movie Specific
/// Naming"), e.g. `Movie (2020) {edition-Director's Cut}.mkv` →
/// `Director's Cut`. Whitespace-trimmed; `None` when absent or empty.
pub fn parse_edition(name: &str) -> Option<String> {
    for (start, _) in name.match_indices('{') {
        let rest = &name[start + 1..];
        let Some(end) = rest.find('}') else { continue };
        let inner = &rest[..end];
        if let Some(label) = inner
            .split_once('-')
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case("edition"))
            .map(|(_, v)| v.trim())
        {
            if !label.is_empty() {
                return Some(label.to_string());
            }
        }
    }
    None
}

/// Remove every `{...}` token (Plex ids, editions, and decorative brace
/// tags alike) from `name`, collapsing the whitespace they leave behind.
/// The existing [`crate::classify`] bracket stripper already does this for
/// title derivation; this is exposed for the reorganize planner, which
/// needs a clean name without going through the full classifier.
pub fn strip_plex_tokens(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    for ch in name.chars() {
        match ch {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The eight Plex extras categories. Plex recognizes these both as
/// subdirectory names (`Behind The Scenes/`, `Deleted Scenes/`, …) and as
/// filename suffixes (`-behindthescenes`, `-deleted`, …) for movies and TV
/// shows alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlexExtraKind {
    BehindTheScenes,
    DeletedScenes,
    Featurettes,
    Interviews,
    Scenes,
    Shorts,
    Trailers,
    Other,
}

impl PlexExtraKind {
    /// Stable lower-camel slug, matching Plex's own extras `type` values.
    pub fn slug(self) -> &'static str {
        match self {
            Self::BehindTheScenes => "behindTheScenes",
            Self::DeletedScenes => "deletedScene",
            Self::Featurettes => "featurette",
            Self::Interviews => "interview",
            Self::Scenes => "scene",
            Self::Shorts => "short",
            Self::Trailers => "trailer",
            Self::Other => "other",
        }
    }

    /// The Plex canonical **directory** name for this category, used when
    /// SWARM writes its preferred layout.
    pub fn canonical_dir(self) -> &'static str {
        match self {
            Self::BehindTheScenes => "Behind The Scenes",
            Self::DeletedScenes => "Deleted Scenes",
            Self::Featurettes => "Featurettes",
            Self::Interviews => "Interviews",
            Self::Scenes => "Scenes",
            Self::Shorts => "Shorts",
            Self::Trailers => "Trailers",
            Self::Other => "Other",
        }
    }

    /// Classify a directory name (case-insensitive) as one of the Plex
    /// extras folders, or `None`. Accepts both the spaced form Plex
    /// documents (`Behind The Scenes`) and the squashed form other tools
    /// emit (`behindthescenes`).
    pub fn from_dir_name(name: &str) -> Option<Self> {
        let key: String = name
            .to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        Some(match key.as_str() {
            "behindthescenes" => Self::BehindTheScenes,
            "deleted" | "deletedscene" | "deletedscenes" => Self::DeletedScenes,
            "featurette" | "featurettes" => Self::Featurettes,
            "interview" | "interviews" => Self::Interviews,
            "scene" | "scenes" => Self::Scenes,
            "short" | "shorts" => Self::Shorts,
            "trailer" | "trailers" => Self::Trailers,
            // Plex documents `Other`; these long-standing SWARM aliases are
            // intentionally retained as tolerant spellings of that category.
            "other" | "others" | "extra" | "extras" | "special" | "specials"
            | "bonus" | "bonusfeature" | "bonusfeatures" => Self::Other,
            _ => return None,
        })
    }

    /// If `stem` ends in one of Plex's extras filename suffixes
    /// (`Movie (2020)-behindthescenes`), return the category and the base
    /// stem with the suffix removed. The suffix must be preceded by `-`
    /// with real title text before it, so an ordinary title ending in the
    /// word "trailer" is never mistaken for one.
    pub fn from_filename_suffix(stem: &str) -> Option<(Self, String)> {
        // (suffix without the leading dash, kind). Plex documents these
        // exact tokens; a couple carry a documented plural alias.
        const SUFFIXES: &[(&str, PlexExtraKind)] = &[
            ("behindthescenes", PlexExtraKind::BehindTheScenes),
            ("deleted", PlexExtraKind::DeletedScenes),
            ("deletedscene", PlexExtraKind::DeletedScenes),
            ("featurette", PlexExtraKind::Featurettes),
            ("interview", PlexExtraKind::Interviews),
            ("scene", PlexExtraKind::Scenes),
            ("short", PlexExtraKind::Shorts),
            ("trailer", PlexExtraKind::Trailers),
            ("other", PlexExtraKind::Other),
        ];
        let lower = stem.to_lowercase();
        for (suffix, kind) in SUFFIXES {
            if let Some(base) = lower.strip_suffix(suffix) {
                if let Some(base) = base.strip_suffix('-') {
                    if !base.trim().is_empty() {
                        // Slice the ORIGINAL stem so the base keeps its case.
                        let cut = stem.len() - suffix.len() - 1;
                        return Some((*kind, stem[..cut].to_string()));
                    }
                }
            }
        }
        None
    }
}

/// A resolved episode span from a multi-episode file — Plex's
/// `S01E01-E03` / `S01E01-S01E03` / `S01E01E02` / `1x01-1x03` conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpisodeRange {
    pub season: u32,
    pub first: u32,
    pub last: u32,
}

/// Parse a multi-episode marker anywhere in `stem`. Returns `None` when no
/// marker is present or it names a single episode (the caller's existing
/// single-episode parser handles that). The second episode reference may
/// repeat the season (`S01E01-S01E02`) or omit it (`S01E01-E02`,
/// `S01E01E02`); a bare `-02` tail is also accepted (`S01E01-02`).
pub fn parse_episode_range(stem: &str) -> Option<EpisodeRange> {
    let lower = stem.to_lowercase();
    let b = lower.as_bytes();

    let mut i = 0;
    while i < b.len() {
        if b[i] == b's' && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            if let Some((season, first, after_first)) = match_sxxeyy(b, i) {
                let mut k = after_first;
                let mut saw_dash = false;
                while k < b.len() {
                    match b[k] {
                        b'-' => {
                            saw_dash = true;
                            k += 1;
                        }
                        b' ' | b'.' | b'_' => k += 1,
                        // `S01E01-S01E02` — the second ref repeats the season.
                        b's' => {
                            k += 1;
                            while k < b.len() && b[k].is_ascii_digit() {
                                k += 1;
                            }
                        }
                        // `S01E01E02` (k == after_first) or `S01E01-E02`.
                        b'e' if saw_dash || k == after_first => {
                            if let Some((last, _)) = take_num(b, k + 1, 4) {
                                if last > first {
                                    return Some(EpisodeRange {
                                        season,
                                        first,
                                        last,
                                    });
                                }
                            }
                            break;
                        }
                        // `S01E01-02` — a bare second number after the dash.
                        d if d.is_ascii_digit() && saw_dash => {
                            if let Some((last, _)) = take_num(b, k, 4) {
                                if last > first {
                                    return Some(EpisodeRange {
                                        season,
                                        first,
                                        last,
                                    });
                                }
                            }
                            break;
                        }
                        _ => break,
                    }
                }
            }
        }
        i += 1;
    }

    // `1x01-1x03` / `1x01-03` form.
    parse_nxnn_range(&lower).and_then(|(season, first, last)| {
        (last > first).then_some(EpisodeRange {
            season,
            first,
            last,
        })
    })
}

/// Match `S<season>E<episode>` at byte offset `at` (which must point at the
/// `s`). Common punctuation between the two halves is tolerated. Returns
/// `(season, episode, offset just past the episode digits)`.
fn match_sxxeyy(b: &[u8], at: usize) -> Option<(u32, u32, usize)> {
    let (season, mut j) = take_num(b, at + 1, 3)?;
    while j < b.len() && matches!(b[j], b' ' | b'.' | b'_' | b'-') {
        j += 1;
    }
    if j >= b.len() || b[j] != b'e' {
        return None;
    }
    let (episode, end) = take_num(b, j + 1, 4)?;
    Some((season, episode, end))
}

fn take_num(bytes: &[u8], start: usize, max_digits: usize) -> Option<(u32, usize)> {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() && end - start < max_digits {
        end += 1;
    }
    if end == start {
        return None;
    }
    let value: u32 = std::str::from_utf8(&bytes[start..end]).ok()?.parse().ok()?;
    Some((value, end))
}

fn parse_nxnn_range(lower: &str) -> Option<(u32, u32, u32)> {
    let lb = lower.as_bytes();
    for i in 0..lb.len() {
        if lb[i] != b'x' {
            continue;
        }
        let mut s = i;
        while s > 0 && lb[s - 1].is_ascii_digit() {
            s -= 1;
        }
        if s == i || (s > 0 && lb[s - 1].is_ascii_alphanumeric()) {
            continue;
        }
        let season: u32 = lower[s..i].parse().ok()?;
        let (first, mut k) = take_num(lb, i + 1, 3)?;
        if k >= lb.len() || lb[k] != b'-' {
            continue;
        }
        k += 1;
        // second half may repeat "<season>x"
        if let Some(xpos) = lower[k..].find('x') {
            if lower[k..k + xpos].bytes().all(|b| b.is_ascii_digit()) && xpos > 0 {
                k += xpos + 1;
            }
        }
        let (last, _) = take_num(lb, k, 3)?;
        return Some((season, first, last));
    }
    None
}

/// Directory names Plex treats as "this holds subtitle sidecars for the
/// media beside it". Plex documents both `Subs` and `Subtitles` and treats
/// them identically; a user must never be forced to rename one to the
/// other.
pub const SUBS_DIR_NAMES: &[&str] = &["subs", "subtitles"];

/// Whether `name` is a Plex subtitle-sidecar directory (case-insensitive).
pub fn is_subs_dir(name: &str) -> bool {
    SUBS_DIR_NAMES.contains(&name.to_lowercase().as_str())
}

/// A `Season NN` / `Specials` folder name → its season number, per Plex's
/// TV conventions. `Specials` and `Season 00` both mean season 0.
pub fn parse_season_dir(name: &str) -> Option<u32> {
    let lower = name.trim().to_lowercase();
    if lower == "specials" {
        return Some(0);
    }
    let rest = lower.strip_prefix("season")?.trim();
    if rest.is_empty() {
        return None;
    }
    rest.parse().ok()
}

/// A deterministic Plex-conformance problem found for one file, in the exact
/// shape the issue asks the scanner to report: what is wrong, what Plex
/// expects instead, and the concrete fix. The AI helper consumes these
/// directly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlexValidationIssue {
    /// Library-relative path (forward slashes) of the offending file.
    pub current_path: String,
    /// What does not conform to any supported Plex structure.
    pub problem: String,
    /// The Plex-compatible structure / name that is expected.
    pub expected: String,
    /// The recommended fix a human (or the AI helper) can apply.
    pub recommended_fix: String,
}

/// Deterministically check one media file against Plex's supported
/// structures. `classified` is [`crate::classify::classify`]'s result for
/// the same path (or `None` when it could not be classified at all).
///
/// Conservative by design: it only reports a problem Plex itself would also
/// fail on, never a mere deviation from SWARM's preferred layout. A loose
/// movie in the library root, a movie without a year, `Subs` vs
/// `Subtitles`, an extras suffix vs an extras folder — all valid Plex, all
/// silent here.
pub fn validate_media_file(
    relative_path: &str,
    classified: Option<&crate::classify::Classified>,
) -> Option<PlexValidationIssue> {
    use swarm_core::peer::MediaKind;

    let file_name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    let stem = file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(file_name);
    let segments: Vec<&str> = relative_path.split('/').filter(|s| !s.is_empty()).collect();
    let dirs = &segments[..segments.len().saturating_sub(1)];

    let Some(c) = classified else {
        return Some(PlexValidationIssue {
            current_path: relative_path.to_string(),
            problem: "the file does not match any Plex-supported movie, TV, or music layout, \
                      so neither Plex nor SWARM can identify it"
                .to_string(),
            expected: "a Plex-recognized name such as \"Movie Title (Year).ext\", \
                       \"Show Name (Year)/Season 01/Show Name - S01E01.ext\", or \
                       \"Artist/Album/01 - Track.ext\""
                .to_string(),
            recommended_fix: "rename the file (and, for movies/shows, place it in a titled folder) \
                              so it carries a title and — for episodes — an SxxEyy marker"
                .to_string(),
        });
    };

    match c.kind {
        MediaKind::Episode => {
            let in_season_dir = dirs.iter().any(|d| parse_season_dir(d).is_some());
            let is_extra = c.extra_kind.is_some() || c.season == Some(0);
            if c.episode.is_none() && !is_extra {
                return Some(PlexValidationIssue {
                    current_path: relative_path.to_string(),
                    problem: "the file sits under a TV show but has no episode number Plex can read \
                              (no SxxEyy / NxNN / date marker)"
                        .to_string(),
                    expected: format!(
                        "\"{} - S{:02}E01 - Episode Title.ext\" inside a \"Season {:02}\" folder",
                        c.show_title.as_deref().unwrap_or("Show Name"),
                        c.season.unwrap_or(1),
                        c.season.unwrap_or(1),
                    ),
                    recommended_fix: "add an SxxEyy marker to the filename, or move it into a \
                                      \"Specials\" / \"Behind The Scenes\" folder if it is an extra"
                        .to_string(),
                });
            }
            if c.episode.is_some() && !in_season_dir && c.season != Some(0) {
                return Some(PlexValidationIssue {
                    current_path: relative_path.to_string(),
                    problem: "the episode is not inside a \"Season NN\" (or \"Specials\") folder, \
                              which Plex requires to group episodes under a season"
                        .to_string(),
                    expected: format!(
                        "{}/Season {:02}/{}",
                        c.show_title.as_deref().unwrap_or("Show Name (Year)"),
                        c.season.unwrap_or(1),
                        file_name,
                    ),
                    recommended_fix: format!(
                        "move the file into a \"Season {:02}\" folder under the show folder",
                        c.season.unwrap_or(1)
                    ),
                });
            }
            None
        }
        MediaKind::Movie => {
            // Plex allows loose movies and movies-in-folders alike, so the
            // only real problem is an empty title.
            if c.title.trim().is_empty() {
                return Some(PlexValidationIssue {
                    current_path: relative_path.to_string(),
                    problem: "no movie title could be derived from the file or its folder"
                        .to_string(),
                    expected: "Movie Title (Year).ext".to_string(),
                    recommended_fix: format!(
                        "rename to \"<Movie Title> ({}).ext\"",
                        c.year
                            .map(|y| y.to_string())
                            .unwrap_or_else(|| "Year".to_string())
                    ),
                });
            }
            None
        }
        MediaKind::Track => {
            if c.artist.as_deref().unwrap_or("").trim().is_empty()
                && c.album.as_deref().unwrap_or("").trim().is_empty()
                && stem.split(" - ").count() < 2
            {
                return Some(PlexValidationIssue {
                    current_path: relative_path.to_string(),
                    problem: "the track has no Artist/Album folders and no \
                              \"Artist - Album - Track\" filename, so Plex cannot place it"
                        .to_string(),
                    expected: "Artist/Album/01 - Track Name.ext".to_string(),
                    recommended_fix: "move the track into \"<Artist>/<Album>/\" folders".to_string(),
                });
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_guid_agents() {
        assert_eq!(
            parse_guid("Interstellar (2014) {tmdb-157336}"),
            Some(PlexGuid {
                agent: "tmdb".into(),
                id: "157336".into()
            })
        );
        assert_eq!(
            parse_guid("Interstellar (2014) {imdb-tt0816692}.mkv").map(|g| g.token()),
            Some("imdb-tt0816692".to_string())
        );
        assert_eq!(
            parse_guid("The Wire (2002) {tvdb-79126}").map(|g| g.agent),
            Some("tvdb".to_string())
        );
    }

    #[test]
    fn guid_parser_ignores_editions_and_decorative_tags() {
        assert_eq!(parse_guid("Movie (2020) {edition-Director's Cut}"), None);
        assert_eq!(parse_guid("Movie [1080p] {web-dl}"), None);
    }

    #[test]
    fn parses_edition_label() {
        assert_eq!(
            parse_edition("Blade Runner (1982) {edition-Final Cut}.mkv").as_deref(),
            Some("Final Cut")
        );
        assert_eq!(
            parse_edition("Dune (2021) {edition-IMAX Enhanced} {tmdb-438631}").as_deref(),
            Some("IMAX Enhanced")
        );
        assert_eq!(parse_edition("Dune (2021).mkv"), None);
    }

    #[test]
    fn strips_every_brace_token() {
        assert_eq!(
            strip_plex_tokens("Movie (2020) {edition-Director's Cut} {tmdb-1}"),
            "Movie (2020)"
        );
    }

    #[test]
    fn extras_dir_names_both_spellings() {
        assert_eq!(
            PlexExtraKind::from_dir_name("Behind The Scenes"),
            Some(PlexExtraKind::BehindTheScenes)
        );
        assert_eq!(
            PlexExtraKind::from_dir_name("behindthescenes"),
            Some(PlexExtraKind::BehindTheScenes)
        );
        assert_eq!(
            PlexExtraKind::from_dir_name("Deleted Scenes"),
            Some(PlexExtraKind::DeletedScenes)
        );
        assert_eq!(PlexExtraKind::from_dir_name("Season 01"), None);
        assert_eq!(PlexExtraKind::from_dir_name("featurette"), Some(PlexExtraKind::Featurettes));
        assert_eq!(PlexExtraKind::from_dir_name("Bonus Features"), Some(PlexExtraKind::Other));
    }

    #[test]
    fn extras_filename_suffixes() {
        assert_eq!(
            PlexExtraKind::from_filename_suffix("Big Buck Bunny (2008)-trailer"),
            Some((PlexExtraKind::Trailers, "Big Buck Bunny (2008)".to_string()))
        );
        assert_eq!(
            PlexExtraKind::from_filename_suffix("Inception (2010)-behindthescenes"),
            Some((PlexExtraKind::BehindTheScenes, "Inception (2010)".to_string()))
        );
        assert_eq!(
            PlexExtraKind::from_filename_suffix("The Making of the Trailer"),
            None,
            "no dash boundary → ordinary title"
        );
    }

    #[test]
    fn multi_episode_ranges() {
        for (name, want) in [
            ("Show - S01E01-E03 - Title", (1u32, 1u32, 3u32)),
            ("Show - S01E01-S01E02", (1, 1, 2)),
            ("Show.S02E05E06.mkv", (2, 5, 6)),
            ("Show - S01E01-02", (1, 1, 2)),
            ("Show - 1x01-1x03", (1, 1, 3)),
            ("Show - 3x08-10", (3, 8, 10)),
        ] {
            let r = parse_episode_range(name).unwrap_or_else(|| panic!("no range in {name}"));
            assert_eq!((r.season, r.first, r.last), want, "{name}");
        }
    }

    #[test]
    fn single_episode_is_not_a_range() {
        assert_eq!(parse_episode_range("Show - S01E01 - Title"), None);
        assert_eq!(parse_episode_range("Show.S02E05.mkv"), None);
    }

    #[test]
    fn season_dir_parsing() {
        assert_eq!(parse_season_dir("Season 01"), Some(1));
        assert_eq!(parse_season_dir("Season 00"), Some(0));
        assert_eq!(parse_season_dir("Specials"), Some(0));
        assert_eq!(parse_season_dir("season 12"), Some(12));
        assert_eq!(parse_season_dir("Extras"), None);
    }

    #[test]
    fn subs_dirs_are_equivalent() {
        assert!(is_subs_dir("Subs"));
        assert!(is_subs_dir("Subtitles"));
        assert!(is_subs_dir("SUBS"));
        assert!(!is_subs_dir("Season 01"));
    }

    #[test]
    fn validate_flags_an_unrecognized_file() {
        let issue = validate_media_file("library/asdf1234.mkv", None).unwrap();
        assert!(issue.problem.contains("does not match any Plex-supported"));
    }

    #[test]
    fn validate_is_silent_for_a_valid_plex_movie() {
        let c = crate::classify::classify("Movies/Inception (2010)/Inception (2010).mkv").unwrap();
        assert_eq!(
            validate_media_file("Movies/Inception (2010)/Inception (2010).mkv", Some(&c)),
            None
        );
    }

    #[test]
    fn validate_flags_an_episode_with_no_season_folder() {
        let c = crate::classify::classify("TV/The Wire/The.Wire.S01E01.mkv").unwrap();
        let issue = validate_media_file("TV/The Wire/The.Wire.S01E01.mkv", Some(&c)).unwrap();
        assert!(issue.problem.contains("Season"));
    }
}
