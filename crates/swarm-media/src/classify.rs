//! File classification and path-derived grouping.
//!
//! Rules inherited from Batocera.Drone (documented there as scar tissue from
//! shipped bugs):
//! - **Allowlist, not denylist** — only known media extensions become catalog
//!   entries, so sidecar files (.nfo, posters, subtitles) never leak in.
//! - **Grouping keys are always path/filename-derived** — embedded tags and
//!   scraped titles are display overlay only, so a bad tag or scrape can
//!   never split or corrupt an album/show grouping.

use swarm_core::peer::MediaKind;

use crate::roots::MediaRootAssetType;

pub const AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "m4a", "wav", "wma", "aac", "aiff", "ape",
];
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "wmv", "flv", "mpg", "mpeg", "m2ts", "ts", "3gp",
];

/// Disc-subfolder names absorbed into the parent album (e.g. `CD1`, `Disc 2`).
fn is_disc_folder(name: &str) -> bool {
    let lower = name.to_lowercase();
    for prefix in ["cd", "disc", "disk"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_start_matches([' ', '-', '_']);
            if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                return true;
            }
        }
    }
    false
}

/// A release-type "category" folder some libraries insert between Artist and
/// the real album folder — `Artist/Album/<Real Album Name>/track.mp3`,
/// `Artist/Compilation/<Real Release Name>/track.mp3`. Ported from
/// batocera.drone's `music/filename_parser.py::_CATEGORY_FOLDER_NAMES`
/// (`drone-music-feature` skill), confirmed there against a real ~2,400-track
/// library where every release of one of these types collapsed into a single
/// fake bucket (e.g. every ATB album under one "ATB / Album" group) — same
/// vocabulary, since it mirrors MusicBrainz's own release-group type list.
const CATEGORY_FOLDER_NAMES: &[&str] = &[
    "album",
    "albums",
    "single",
    "singles",
    "ep",
    "eps",
    "broadcast",
    "broadcasts",
    "other",
    "others",
    "compilation",
    "compilations",
    "soundtrack",
    "soundtracks",
    "spokenword",
    "interview",
    "interviews",
    "audiobook",
    "audiobooks",
    "audio drama",
    "live",
    "live album",
    "live albums",
    "remix",
    "remixes",
    "dj-mix",
    "dj mix",
    "mixtape",
    "mixtapes",
    "street",
    "demo",
    "demos",
    "field recording",
    "field recordings",
    "bootleg",
    "bootlegs",
    "bonus",
    "bonuses",
];

fn music_name_key(name: &str) -> String {
    name.to_ascii_lowercase()
        .replace(" and ", " & ")
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}

fn is_artist_collection_folder(folder: &str, artist: &str) -> bool {
    let lower = folder.to_ascii_lowercase();
    CATEGORY_FOLDER_NAMES.iter().any(|category| {
        lower
            .strip_suffix(category)
            .map(|prefix| prefix.trim_end_matches([' ', '-', '_']))
            .is_some_and(|prefix| music_name_key(prefix) == music_name_key(artist))
    })
}

/// Real libraries often number these wrapper folders (`"3. Remixes"`,
/// `"4. Bonus"`) rather than using the bare category name — confirmed
/// against a real library where an artist's remix/bonus folders were laid
/// out exactly this way. Strips a leading `N`/`N.`/`N ` ordinal before the
/// category-name check so those still match.
fn strip_leading_ordinal(name: &str) -> &str {
    let trimmed = name.trim_start();
    let digits_end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits_end == 0 {
        return name;
    }
    let rest = trimmed[digits_end..].trim_start_matches(['.', ' ']).trim();
    if rest.is_empty() {
        name
    } else {
        rest
    }
}

fn is_category_folder(name: &str) -> bool {
    CATEGORY_FOLDER_NAMES.contains(&strip_leading_ordinal(name).to_lowercase().as_str())
}

/// Strips a trailing `" - Discography"`/`" Discography"` suffix (case-
/// insensitive) from an artist folder name. Real, live example: a folder
/// named `"Kyau & Albert - Discography"` (containing every one of that
/// artist's releases as subfolders) was being treated as if "Discography"
/// were literally part of the artist's name — both the grouping display and
/// every MusicBrainz search built from it (`artist:"Kyau & Albert -
/// Discography"`) were wrong as a result. Confirmed live against a real
/// ~5,300-track library: two artist folders (`Kyau & Albert - Discography`,
/// `Staind - Discography`) used this exact convention, together 666 tracks
/// (~12% of the library).
fn strip_discography_suffix(name: &str) -> &str {
    for suffix in [" - discography", " discography"] {
        if name.len() > suffix.len()
            && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        {
            let stripped = name[..name.len() - suffix.len()].trim_end();
            if !stripped.is_empty() {
                return stripped;
            }
        }
    }
    name
}

/// A generic top-level "this is where the music lives" folder name, only
/// ever meaningful as the very first path segment (unlike
/// [`CATEGORY_FOLDER_NAMES`], which applies one level into an artist).
/// Deliberately a short, narrow list — the same accepted trade-off as any
/// name-based folder convention (a real artist literally named "Music"
/// would be misread), kept small so it only catches the genuinely generic,
/// unambiguous wrapper names real libraries actually use.
const MEDIA_TYPE_WRAPPER_NAMES: &[&str] = &["music", "songs", "audio", "tracks"];

fn is_media_type_wrapper(name: &str) -> bool {
    MEDIA_TYPE_WRAPPER_NAMES.contains(&name.to_lowercase().as_str())
}

/// The video equivalent of [`MEDIA_TYPE_WRAPPER_NAMES`] — a top-level "this
/// is where the shows live" folder (Sonarr/Plex/Kodi's own convention: a
/// `TV Shows/<Show Name>/...` root). Confirmed live against a real library
/// (`Batocera-movies-shows/Shows/<Show Name>/...`) where content nested
/// under a show folder with neither a `SxxEyy`/`Ep. NN` marker nor a
/// `Season N` subfolder (deeply nested featurettes/deleted-scenes/bonus
/// content) fell all the way through to the movie fallback below and was
/// searched against the wrong TMDb database entirely. Only meaningful as
/// the very first path segment, same reasoning as the music wrapper.
const VIDEO_TYPE_WRAPPER_NAMES: &[&str] = &[
    "shows",
    "show",
    "tv",
    "tv shows",
    "tv series",
    "series",
    "television",
];

fn is_video_type_wrapper(name: &str) -> bool {
    VIDEO_TYPE_WRAPPER_NAMES.contains(&name.to_lowercase().as_str())
}

/// Subfolder names a movie's own bonus material is gathered into — the
/// Plex/Kodi/Jellyfin "local extras" convention (`Movie (2019)/Featurettes/
/// Making Of.mkv`). A video inside one of these, with no show/episode
/// signal of its own, is part of that movie rather than a separate film.
fn is_extras_folder(name: &str) -> bool {
    crate::plex::PlexExtraKind::from_dir_name(name).is_some()
}

#[derive(Debug)]
struct MovieExtra {
    movie_title: String,
    movie_year: Option<u32>,
    display_title: String,
    kind: crate::plex::PlexExtraKind,
    parent_dir: String,
    relative_path: String,
    category_path: Option<String>,
}

/// Resolve a file anywhere below a recognized movie-extras directory. The
/// first extras directory anchors the owning movie folder; the nearest
/// recognized directory supplies the type, so nested category overrides are
/// deterministic (`Featurettes/x/Deleted Scenes/y.mkv` is a deleted scene).
fn movie_extra_from_dirs(dirs: &[&str], file_name: &str, clip_stem: &str) -> Option<MovieExtra> {
    let extras_idx = dirs.iter().position(|dir| is_extras_folder(dir))?;
    let parent = dirs.get(extras_idx.checked_sub(1)?)?;
    // A real movie folder does not need a year; Plex recommends one but does
    // not require SWARM to reject otherwise unambiguous existing libraries.
    let (stripped, year) = extract_year_and_strip(parent);
    let movie_title = clean_title(&stripped);
    if movie_title.is_empty() {
        return None;
    }
    let kind = dirs[extras_idx..]
        .iter()
        .rev()
        .find_map(|dir| crate::plex::PlexExtraKind::from_dir_name(dir))?;
    Some(MovieExtra {
        movie_title,
        movie_year: year,
        display_title: clean_title(clip_stem),
        kind,
        parent_dir: dirs[..extras_idx].join("/"),
        relative_path: dirs[extras_idx..]
            .iter()
            .chain(std::iter::once(&file_name))
            .copied()
            .collect::<Vec<_>>()
            .join("/"),
        category_path: Some(dirs[extras_idx..].join("/")),
    })
}

#[derive(Debug)]
struct EpisodeExtra {
    kind: crate::plex::PlexExtraKind,
    title: String,
    relative_path: String,
    category_path: String,
}

/// Resolve a show's bonus content from the first recognized extras directory,
/// while the deepest recognized directory determines its type (the same rule
/// as [`movie_extra_from_dirs`]). A `Specials` season folder is not itself an
/// extras-category anchor. Unlike a movie extra, there is
/// no parent-folder title/year to resolve here — a show extra links to its
/// show purely via the `show_title` already carried by its caller, not a
/// synthetic parent entry_key, so this only reports the type/title/paths.
fn episode_extra_from_dirs(dirs: &[&str], file_name: &str, clip_stem: &str) -> Option<EpisodeExtra> {
    let extras_idx = dirs
        .iter()
        .position(|dir| is_extras_folder(dir) && !is_season_folder(dir))?;
    let kind = dirs[extras_idx..]
        .iter()
        .rev()
        .find_map(|dir| crate::plex::PlexExtraKind::from_dir_name(dir))?;
    Some(EpisodeExtra {
        kind,
        title: clean_title(clip_stem),
        relative_path: dirs[extras_idx..]
            .iter()
            .chain(std::iter::once(&file_name))
            .copied()
            .collect::<Vec<_>>()
            .join("/"),
        category_path: dirs[extras_idx..].join("/"),
    })
}

/// The show folder immediately below a recognized Shows/TV wrapper folder
/// somewhere in `dirs` ([VIDEO_TYPE_WRAPPER_NAMES]), if any. Scans the whole
/// ancestor chain rather than anchoring to index 0, same robustness as
/// [find_ancestor_season], since a real path may carry an extra leading
/// multi-root label segment ahead of the wrapper. Shared by every
/// show_title fallback chain that needs "the real show folder" rather than
/// [show_title_from_ancestors]'s naive nearest-non-season-folder walk,
/// which can land on a generic bonus-content wrapper folder name
/// (`"Featurettes"`, `"Extras"`) instead of the actual show — confirmed
/// live: a file with its own `S00E02`-style marker sitting directly inside
/// a `Featurettes` folder (no season-shaped ancestor, no stem-prefix text)
/// picked up show_title `"Featurettes"` before this existed.
fn wrapper_derived_show_name(dirs: &[&str]) -> Option<String> {
    let wrapper_idx = dirs.iter().position(|d| is_video_type_wrapper(d))?;
    // Strip Plex `{tvdb-…}` / `{imdb-…}` / `{edition-…}` tokens and a
    // trailing `(YYYY)` premiere year — Plex shows neither in the display
    // title (the year is a separate field) — but keep everything else this
    // fallback deliberately preserves (quality/edition parentheticals; see
    // the doc comment).
    let raw = crate::plex::strip_plex_tokens(dirs.get(wrapper_idx + 1)?);
    let show_title = clean_title(strip_trailing_year_paren(&raw));
    (!show_title.is_empty()).then_some(show_title)
}

/// Remove a trailing ` (YYYY)` where the parenthetical is exactly a
/// `1900..=2099` year — the Plex `Show Name (Year)` folder convention. A
/// non-year trailing parenthetical (`(1080p BluRay …)`, `(US)`) is left
/// untouched.
fn strip_trailing_year_paren(name: &str) -> &str {
    let trimmed = name.trim_end();
    let Some(inner) = trimmed.strip_suffix(')') else {
        return name;
    };
    let Some(open) = inner.rfind('(') else {
        return name;
    };
    let candidate = &inner[open + 1..];
    if candidate.len() == 4
        && candidate.bytes().all(|b| b.is_ascii_digit())
        && candidate
            .parse::<u32>()
            .is_ok_and(|y| (1900..=2099).contains(&y))
    {
        let head = inner[..open].trim_end();
        if !head.is_empty() {
            return head;
        }
    }
    name
}

/// Find an `Ep`/`Episode` marker (case-insensitive, optional trailing `.`,
/// optional space, then 1-4 digits) — a common real-world alternate to
/// `SxxEyy` for shows numbered without a season component in the filename
/// (e.g. `"CENTURIONS - Ep. 57 - Hole in the Ocean, Part 2"`). Confirmed
/// live: this exact convention was silently misclassified as a movie
/// (searched against TMDb's movie DB, so "not found" even though the show
/// exists) before this parser existed. Both boundaries must be non-
/// alphanumeric (or string start/end), same bounding discipline as
/// [parse_nxnn_marker], so this never fires inside a longer word like
/// "Deep" or "Prep". Unlike `SxxEyy`, this marker carries no season of its
/// own — the caller resolves season from an ancestor `Season N` folder,
/// defaulting to 1 when none exists. Returns the parsed episode number.
fn parse_ep_marker(stem: &str) -> Option<u32> {
    let bytes = stem.as_bytes();
    let is_boundary_before = |i: usize| i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
    for word in ["episode", "ep"] {
        let wlen = word.len();
        let mut start = 0;
        while start + wlen <= bytes.len() {
            if is_boundary_before(start)
                && bytes[start..start + wlen].eq_ignore_ascii_case(word.as_bytes())
            {
                let mut i = start + wlen;
                if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                }
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                let digit_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let digit_len = i - digit_start;
                let at_end = i == bytes.len() || !bytes[i].is_ascii_alphanumeric();
                if digit_len > 0 && digit_len <= 4 && at_end {
                    if let Ok(episode) = stem[digit_start..i].parse() {
                        return Some(episode);
                    }
                }
            }
            start += 1;
        }
    }
    None
}

/// A bare leading episode number with no `S`/`x`/"Ep" marker at all —
/// e.g. `"101 - Simpsons Roasting on an Open Fire.avi"` sitting directly in
/// a `Season 01` folder. Real, common convention for older "Complete
/// Series" rips of very long-running shows (The Simpsons chief among
/// them): without any of the markers `parse_episode_marker`/`parse_ep_
/// marker` look for, every file like this used to fall into the season-0
/// "bonus content" bucket below with no episode number at all, so nothing
/// under the show ever looked properly identified. `season` is always the
/// real ancestor `Season N`/`SNN` folder's number, known at every call
/// site. Two real shapes, disambiguated by digit count:
/// - Absolute numbering (season folded into the number itself — the
///   dominant convention for this case): 3-4 digits whose leading portion
///   equals `season` exactly and leaves a 2-digit remainder — episode is
///   that remainder (`"101"` under `Season 01` → episode 1, `"714"` under
///   `Season 07` → episode 14).
/// - Otherwise, the number is read as a plain per-season episode number.
/// Requires a non-digit separator right after the digit run (space, `-`,
/// `.`, `_`), same boundary discipline as `split_track_number`, so a title
/// that's just a bare number with nothing after it (`"2001.mkv"`) never
/// misfires.
fn parse_leading_episode_number(stem: &str, season: u32) -> Option<u32> {
    let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let rest = &stem[digits.len()..];
    let trimmed = rest.trim_start_matches([' ', '-', '.', '_']);
    if trimmed.is_empty() || trimmed.len() == rest.len() {
        return None;
    }
    if digits.len() >= 3 {
        if let Some(episode_part) = digits.strip_prefix(season.to_string().as_str()) {
            if episode_part.len() == 2 {
                if let Ok(episode) = episode_part.parse() {
                    return Some(episode);
                }
            }
        }
    }
    digits.parse().ok()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Classified {
    pub kind: MediaKind,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<u32>,
    pub show_title: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub year: Option<u32>,
    /// Last episode of a Plex multi-episode file (`S01E01-E03` → `episode`
    /// = 1, `episode_end` = Some(3)). `None` for ordinary single-episode
    /// files. See [`crate::plex::parse_episode_range`].
    pub episode_end: Option<u32>,
    /// Plex agent id embedded in the file/folder name as `{tmdb-…}` /
    /// `{imdb-…}` / `{tvdb-…}`, stored in `agent-id` token form. Lets the
    /// scraper skip its fuzzy matcher. See [`crate::plex::parse_guid`].
    pub plex_guid: Option<String>,
    /// Plex `{edition-<label>}` token, e.g. `Director's Cut`.
    pub edition: Option<String>,
    /// When the file is a Plex "extra" (trailer / behind the scenes /
    /// deleted scene / …) rather than a feature or a numbered episode, the
    /// extras category slug (see [`crate::plex::PlexExtraKind::slug`]).
    pub extra_kind: Option<&'static str>,
    /// Clean, user-facing title of an extra, independent of the feature's
    /// grouping title (for example `Dorm Room Extended`).
    pub extra_title: Option<String>,
    /// Path-derived owning movie title used to resolve the concrete parent
    /// entry without parsing the presentation title back apart.
    pub extra_parent_title: Option<String>,
    /// Library-relative directory containing the owning movie.
    pub extra_parent_dir: Option<String>,
    /// Path from the movie directory to this extra, including its filename.
    pub extra_relative_path: Option<String>,
    /// Optional directory/category path between the movie and extra file.
    pub extra_category_path: Option<String>,
}

/// A `Classified` with every field at its neutral default, so the many
/// call sites below only have to name the fields that actually apply. Used
/// with struct-update syntax: `Classified { kind, title, ..blank() }`.
fn blank_classified() -> Classified {
    Classified {
        kind: MediaKind::Movie,
        title: String::new(),
        artist: None,
        album: None,
        track_number: None,
        show_title: None,
        season: None,
        episode: None,
        year: None,
        episode_end: None,
        plex_guid: None,
        edition: None,
        extra_kind: None,
        extra_title: None,
        extra_parent_title: None,
        extra_parent_dir: None,
        extra_relative_path: None,
        extra_category_path: None,
    }
}

pub fn media_extension(relative_path: &str) -> Option<(&'static str, bool)> {
    let ext = relative_path.rsplit('.').next()?.to_lowercase();
    if let Some(known) = AUDIO_EXTS.iter().find(|e| **e == ext) {
        return Some((known, true));
    }
    VIDEO_EXTS
        .iter()
        .find(|e| **e == ext)
        .map(|known| (*known, false))
}

/// Classify a library-relative path (forward slashes) into a catalog entry.
/// Returns None for non-media extensions.
pub fn classify(relative_path: &str) -> Option<Classified> {
    let (_, is_audio) = media_extension(relative_path)?;
    let segments: Vec<&str> = relative_path.split('/').filter(|s| !s.is_empty()).collect();
    let file_name = segments.last()?;
    let stem = file_name
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(file_name);
    // Directory chain above the file, with disc folders absorbed.
    let mut dirs: Vec<&str> = segments[..segments.len() - 1].to_vec();
    if dirs.last().is_some_and(|d| is_disc_folder(d)) {
        dirs.pop();
    }

    if is_audio {
        let (track_number, title) = split_track_number(stem);
        // Folder convention: .../Artist/Album/track — anchored from the TOP
        // (artist = the first folder under the media root, album = the
        // second), not the bottom. Ported from batocera.drone's
        // `music/filename_parser.py::classify_location` (`drone-music-
        // feature` skill) after confirming live that anchoring from the
        // bottom (previously: album = dirs.last(), artist = the folder
        // above it) silently produced garbage for any real library nested
        // deeper than exactly two levels — DJ-mix/radio-broadcast-style
        // folder structures like `Gabriel & Dresden/Organized Natures/
        // 01-29/29/track.mp3` classified as artist="01-29", album="29"
        // instead of the correct artist="Gabriel & Dresden", album=
        // "Organized Natures". Anchoring from the top and simply ignoring
        // any deeper segments fixes this without needing to special-case
        // "how deep is too deep" — confirmed against a real library where
        // 82% of tracks (4,371/5,340) were nested past two levels.
        //
        // A generic media-type wrapper folder (a single combined root with
        // Movies/Shows/Music-style top-level subfolders, rather than a
        // dedicated per-type root or label) is skipped the same way a
        // multi-root label prefix already is upstream (see scan_roots/
        // reclassify_all) — otherwise it would be misread as the artist.
        let dirs: &[&str] = if dirs.first().is_some_and(|d| is_media_type_wrapper(d)) {
            &dirs[1..]
        } else {
            &dirs
        };

        // A category/release-type wrapper folder some libraries insert
        // right after Artist (`Artist/Album/<Real Album Name>/track.mp3`,
        // `Artist/Compilation/<Real Release>/track.mp3`) is skipped so the
        // real album name underneath it is used instead of the category
        // label — see CATEGORY_FOLDER_NAMES.
        //
        // No artist folder at all (a flat single-folder library, or files
        // dropped directly under a bare MEDIA_TYPE_WRAPPER_NAMES root) means
        // there is nothing here to anchor from, so fall back to parsing the
        // same information out of the filename instead — see
        // `parse_flat_track_fields`. The scraper (`scrape_tracks` in
        // `scrape/runner.rs`) requires both artist and album before it will
        // even attempt a MusicBrainz lookup, so without this fallback every
        // track in a flat library is permanently skipped.
        let (artist, album, track_number, title) = if dirs.is_empty() {
            parse_flat_track_fields(&title, track_number)
        } else {
            let artist = dirs
                .first()
                .map(|s| clean_title(strip_discography_suffix(s)));
            let album = match dirs.get(1) {
                Some(second) if is_category_folder(second) && dirs.len() >= 3 => {
                    if dirs.len() >= 4
                        && music_name_key(dirs[2])
                            == artist.as_deref().map_or_else(String::new, music_name_key)
                    {
                        Some(clean_title(dirs[3]))
                    } else {
                        Some(clean_title(dirs[2]))
                    }
                }
                Some(second)
                    if dirs.len() >= 3
                        && artist
                            .as_deref()
                            .is_some_and(|artist| is_artist_collection_folder(second, artist)) =>
                {
                    Some(clean_title(dirs[2]))
                }
                Some(second) => Some(clean_title(second)),
                None => None,
            };
            (artist, album, track_number, title)
        };
        return Some(Classified {
            kind: MediaKind::Track,
            title,
            artist,
            album,
            track_number,
            ..blank_classified()
        });
    }

    // Bracketed release-group/resolution/codec tags (`[1080p]`, `(x264)`,
    // `{YIFY}`) are decorative and stripped from the title outright; a bare
    // 4-digit year inside one is the sole exception — meaningful signal for
    // TMDb search, kept even though its brackets are still removed. The
    // dominant real-world scene-release convention has no brackets at all
    // though (`10.Cloverfield.Lane.2016.1080p.BluRay.x264-GROUP.mkv`) — a
    // standalone dot/underscore-delimited year token is just as meaningful
    // a signal and just as wrong left sitting in the middle of a title, so
    // it's captured and stripped the same way once no bracket year was
    // found (bracket wins on the rare filename that somehow has both — a
    // deliberately bracketed year is a more deliberate signal). Movies
    // often only carry the year on the enclosing folder, not the filename
    // (`Inception (2010)/Inception.1080p.mkv`), so fall back there too.
    let (stem_clean, stem_year) = extract_year_and_strip(stem);
    let mut year = stem_year;
    if year.is_none() {
        year = dirs.last().and_then(|dir| extract_year_and_strip(dir).1);
    }

    // Plex "Movie/Show Specific Naming": an agent id and/or an edition label
    // embedded in the file stem or any ancestor folder as `{tmdb-…}` /
    // `{imdb-…}` / `{tvdb-…}` / `{edition-…}`. The existing bracket handling
    // already strips both from the derived title; this captures them so the
    // scraper can skip its fuzzy matcher and the catalog can show the
    // edition. Nearest name wins (file over folder).
    let plex_guid = crate::plex::parse_guid(stem)
        .or_else(|| dirs.iter().rev().find_map(|d| crate::plex::parse_guid(d)))
        .map(|g| g.token());
    let edition = crate::plex::parse_edition(stem)
        .or_else(|| dirs.iter().rev().find_map(|d| crate::plex::parse_edition(d)));

    if let Some((season, episode, title_prefix)) = parse_episode_marker(&stem_clean) {
        // Plex multi-episode file (`S01E01-E03`): keep the first episode as
        // the primary number and record the span end.
        let episode_end = crate::plex::parse_episode_range(&stem_clean)
            .filter(|r| r.season == season && r.first == episode && r.last > episode)
            .map(|r| r.last);
        // Show title: prefer an ancestor season folder (either shape — see
        // find_ancestor_season) over the text before the SxxEyy/NxNN
        // marker, else fall back to the marker's own stem-prefix text, else
        // the older plain-directory fallback (Show/Season 1/file).
        //
        // This used to prefer the stem-prefix text first — reversed after a
        // real, live example proved that wrong: a folder ("Law & Order
        // SVU") containing many seasons' worth of episodes sourced from
        // different release groups, where most files agree on one exact
        // filename wording but a handful vary ("Law and Order SVU",
        // "Law And Order SVU", "Law and Order Special Victims Unit" —
        // confirmed live, 7 real files split into 3 splinter groups this
        // way out of 580). A season folder the user actually organized
        // files into is a much more stable, deliberate identity signal
        // than whatever text a random uploader happened to put in a
        // filename — release-group filename wording varies far more than
        // folder structure does in practice.
        let from_stem = clean_title(title_prefix);
        let folder_derived = find_ancestor_season(&dirs)
            .map(|(name, _, _)| name)
            .filter(|name| !name.is_empty());
        let show_title = folder_derived
            .or_else(|| (!from_stem.is_empty()).then_some(from_stem))
            .or_else(|| wrapper_derived_show_name(&dirs))
            .unwrap_or_else(|| show_title_from_ancestors(&dirs));
        return Some(Classified {
            kind: MediaKind::Episode,
            title: clean_title(&stem_clean),
            show_title: (!show_title.is_empty()).then_some(show_title),
            season: Some(season),
            episode: Some(episode),
            episode_end,
            year,
            plex_guid,
            edition,
            ..blank_classified()
        });
    }

    // `Ep. NN`/`Episode NN` marker with no season encoded in the filename
    // itself (unlike SxxEyy/NxNN) — season comes from an ancestor
    // `Season N` folder when one exists, else defaults to 1 (the common
    // convention for a continuously-numbered single-season show). The show
    // name is deliberately always folder-derived here, never parsed from
    // the text before the marker — real "Ep. NN" filenames are far less
    // consistently formatted than SxxEyy ones (sometimes an abbreviation,
    // sometimes omitted entirely), so the folder ancestor is the more
    // reliable, path-derived signal (see the module doc comment's grouping
    // rule).
    if let Some(episode) = parse_ep_marker(&stem_clean) {
        let (season, folder_year) = find_ancestor_season(&dirs)
            .map(|(_, s, y)| (s, y))
            .unwrap_or((1, None));
        let show_title = find_ancestor_season(&dirs)
            .map(|(name, _, _)| name)
            .or_else(|| wrapper_derived_show_name(&dirs))
            .unwrap_or_else(|| show_title_from_ancestors(&dirs));
        if !show_title.is_empty() {
            return Some(Classified {
                kind: MediaKind::Episode,
                title: clean_title(&stem_clean),
                show_title: Some(show_title),
                season: Some(season),
                episode: Some(episode),
                year: year.or(folder_year),
                plex_guid,
                edition,
                ..blank_classified()
            });
        }
    }

    // No SxxEyy anywhere in the filename itself, but the file sits somewhere
    // under a real season folder. Two cases:
    // - A bare leading episode number (see [parse_leading_episode_number])
    //   — the real episode, just numbered without any S/x/Ep marker. Common
    //   for older "Complete Series" rips of long-running shows.
    // - Anything else is bonus/extra content (a featurette, interview,
    //   deleted scene, blooper reel, behind-the-scenes clip...). The
    //   specific containing subfolder name isn't matched against a list of
    //   known synonyms (too fragile — it varies by uploader); the
    //   structural signal alone (nested under a season folder, no episode
    //   marker of its own) is what matters. `season: Some(0)` is the
    //   real-world Plex/Kodi/TheTVDB convention for "Specials" —
    //   deliberately a single show-level bucket rather than per-season,
    //   since bonus content isn't numbered against any one season the way
    //   real episodes are.
    if let Some((show_title, ancestor_season, folder_year)) = find_ancestor_season(&dirs) {
        // Only trust a bare leading number as the real episode number when
        // the file sits directly inside the season folder itself — deeper
        // nesting (e.g. `Season 03/Featurettes/Access - Granted/11. clip
        // .mkv`) is genuine bonus content that just happens to start with a
        // number, not an absolute-numbered episode.
        let directly_in_season_folder = dirs.last().is_some_and(|dir| is_season_folder(dir));
        let leading_episode = directly_in_season_folder
            .then(|| parse_leading_episode_number(&stem_clean, ancestor_season))
            .flatten();
        let (season, episode) = match leading_episode {
            Some(episode) => (ancestor_season, Some(episode)),
            None => (0, None),
        };
        // Season-0 bonus content sitting in a recognized Plex extras folder
        // (`Featurettes/`, `Deleted Scenes/`, …) carries that category, plus
        // the same title/category/relative-path detail a movie extra gets
        // (see [episode_extra_from_dirs]) — the show links purely via
        // `show_title` above, so there's no parent entry_key to resolve.
        let episode_extra = (season == 0)
            .then(|| episode_extra_from_dirs(&dirs, file_name, &stem_clean))
            .flatten();
        return Some(Classified {
            kind: MediaKind::Episode,
            title: clean_title(&stem_clean),
            show_title: Some(show_title),
            season: Some(season),
            episode,
            year: year.or(folder_year),
            plex_guid,
            edition,
            extra_kind: episode_extra.as_ref().map(|e| e.kind.slug()),
            extra_title: episode_extra.as_ref().map(|e| e.title.clone()),
            extra_relative_path: episode_extra.as_ref().map(|e| e.relative_path.clone()),
            extra_category_path: episode_extra.as_ref().map(|e| e.category_path.clone()),
            ..blank_classified()
        });
    }

    // No episode marker anywhere, no ancestor `Season N` folder — but the
    // file is nested under a recognized Shows/TV wrapper folder somewhere
    // in its ancestor chain (see [VIDEO_TYPE_WRAPPER_NAMES]). This is
    // deeply-nested bonus content (featurettes/deleted-scenes/fake-endings
    // — arbitrarily nested, no fixed convention worth enumerating, same
    // reasoning as the season-folder bonus-content case above) sitting
    // directly under a show folder rather than a season folder. Confirmed
    // live: without this, such files fell all the way through to the movie
    // fallback below and were searched against the wrong TMDb database, and
    // never appeared under their show. The show folder is the segment
    // immediately below the wrapper. Scans the whole ancestor chain rather
    // than anchoring to index 0, same robustness as [find_ancestor_season],
    // since a real path may carry an extra leading multi-root label segment
    // ahead of the wrapper.
    if let Some(show_title) = wrapper_derived_show_name(&dirs) {
        let episode_extra = episode_extra_from_dirs(&dirs, file_name, &stem_clean);
        return Some(Classified {
            kind: MediaKind::Episode,
            title: clean_title(&stem_clean),
            show_title: Some(show_title),
            season: Some(0),
            year,
            plex_guid,
            edition,
            extra_kind: episode_extra.as_ref().map(|e| e.kind.slug()),
            extra_title: episode_extra.as_ref().map(|e| e.title.clone()),
            extra_relative_path: episode_extra.as_ref().map(|e| e.relative_path.clone()),
            extra_category_path: episode_extra.as_ref().map(|e| e.category_path.clone()),
            ..blank_classified()
        });
    }

    // A clip in a movie's own `Featurettes`/`Specials`/`Deleted Scenes`/...
    // subfolder is bonus material for that movie, not a film in its own
    // right. It is catalogued as a Movie sharing the parent movie's title
    // and year — so it groups with the feature and scrapes as the same
    // title — with the clip's own name appended for the catalog list.
    if let Some(extra) = movie_extra_from_dirs(&dirs, file_name, &stem_clean) {
        return Some(Classified {
            kind: MediaKind::Movie,
            title: if extra.display_title.is_empty()
                || extra.display_title.eq_ignore_ascii_case(&extra.movie_title)
            {
                extra.movie_title.clone()
            } else {
                format!("{} - {}", extra.movie_title, extra.display_title)
            },
            year: year.or(extra.movie_year),
            plex_guid,
            edition,
            extra_kind: Some(extra.kind.slug()),
            extra_title: Some(extra.display_title),
            extra_parent_title: Some(extra.movie_title),
            extra_parent_dir: Some(extra.parent_dir),
            extra_relative_path: Some(extra.relative_path),
            extra_category_path: extra.category_path,
            ..blank_classified()
        });
    }

    // Plex extras named by filename suffix rather than by folder:
    // `Inception (2010)-trailer.mkv`, `Big Buck Bunny (2008)-behindthescenes.mkv`.
    // The base stem before the suffix is the feature's own name, so the clip
    // groups and scrapes with the feature.
    if let Some((extra_kind, base)) = crate::plex::PlexExtraKind::from_filename_suffix(stem) {
        let (base_clean, base_year) = extract_year_and_strip(&base);
        let display_title = clean_title(&base_clean);
        if !display_title.is_empty() {
            let parent = dirs.last();
            let (parent_title, parent_year) = parent
                .map(|dir| extract_year_and_strip(dir))
                .map(|(title, year)| (clean_title(&title), year))
                .unwrap_or_else(|| (display_title.clone(), base_year));
            return Some(Classified {
                kind: MediaKind::Movie,
                title: parent_title.clone(),
                year: year.or(parent_year).or(base_year),
                plex_guid,
                edition,
                extra_kind: Some(extra_kind.slug()),
                extra_title: Some(display_title),
                extra_parent_title: Some(parent_title.clone()),
                extra_parent_dir: Some(dirs.join("/")),
                extra_relative_path: Some(file_name.to_string()),
                ..blank_classified()
            });
        }
    }

    // Some personal libraries append cast/genre/language descriptors after
    // the real title behind a spaced dash, before the release year:
    // `01 Die Hard - Bruce Willis Action 1988 Eng Subs 1080p [H264-mp4]`.
    // Drop that suffix so the title is just `Die Hard` — see
    // [strip_descriptor_dash_suffix] for how conservatively this fires.
    let movie_stem = strip_descriptor_dash_suffix(stem, stem_year);
    let (movie_stem_clean, _) = extract_year_and_strip(movie_stem);

    // Scene-style movie filenames conventionally put the release year at
    // the boundary between the real title and technical/release metadata:
    // `Title.2022.IMAX.1080p.BluRay...`.  Keeping the suffix made the TMDb
    // query overly specific and caused otherwise ordinary movies to miss.
    // Do this only in the movie fallback, after every episode path above
    // has consumed the cleaned full stem; truncating before then would hide
    // an SxxEyy marker in filenames such as `Show.2010.S01E01.mkv`.
    let title = stem_year
        .and_then(|release_year| title_before_release_year(movie_stem, release_year))
        .unwrap_or_else(|| clean_title(&movie_stem_clean));
    // A zero-padded leading ordinal some libraries prefix onto every movie
    // file (`01 Die Hard`, `03. The Matrix`) is not part of the title.
    let title = strip_leading_movie_ordinal(&title).to_string();

    Some(Classified {
        kind: MediaKind::Movie,
        title,
        year,
        plex_guid,
        edition,
        ..blank_classified()
    })
}

/// Classify a path with the configured root type as additional structural
/// context. A dedicated Shows root begins at `<Show Name>/...`, so it does
/// not carry the `Shows/<Show Name>/...` wrapper that [`classify`] can use
/// to distinguish deeply nested bonus clips from standalone movies.
///
/// Root typing is authoritative: every video in a Shows root belongs to the
/// first show directory. A recognized Plex extras directory is authoritative
/// even when a legacy filename happens to contain an episode marker; other
/// explicitly numbered files remain normal episodes, and every unnumbered
/// clip falls into Plex's `Other` category. Extras below a season folder
/// retain that season, while show-level extras use season 0 (Specials).
pub fn classify_for_asset_type(
    relative_path: &str,
    asset_type: MediaRootAssetType,
) -> Option<Classified> {
    let mut classified = classify(relative_path)?;
    match asset_type {
        MediaRootAssetType::Mixed => {}
        MediaRootAssetType::Music if classified.kind != MediaKind::Track => return None,
        MediaRootAssetType::Music => {}
        MediaRootAssetType::Movies | MediaRootAssetType::PhotosVideos => {
            if classified.kind == MediaKind::Track {
                return None;
            }
            classified.kind = MediaKind::Movie;
            classified.show_title = None;
            classified.season = None;
            classified.episode = None;
            classified.episode_end = None;
        }
        MediaRootAssetType::Shows => {
            if classified.kind == MediaKind::Track {
                return None;
            }
            let segments: Vec<&str> = relative_path
                .split('/')
                .filter(|segment| !segment.is_empty())
                .collect();
            let file_name = *segments.last()?;
            let dirs = &segments[..segments.len() - 1];
            let show_idx = usize::from(dirs.first().is_some_and(|dir| is_video_type_wrapper(dir)));
            let raw_show = crate::plex::strip_plex_tokens(dirs.get(show_idx)?);
            let show_title = clean_title(strip_trailing_year_paren(&raw_show));
            if show_title.is_empty() {
                return None;
            }

            classified.kind = MediaKind::Episode;
            classified.show_title = Some(show_title);

            let stem = file_name
                .rsplit_once('.')
                .map_or(file_name, |(stem, _)| stem);
            let clip_stem = extract_bracket_tags(stem).0;
            let recognized = episode_extra_from_dirs(dirs, file_name, &clip_stem);

            if let Some(extra) = recognized {
                let season = find_ancestor_season(dirs).map_or(0, |(_, season, _)| season);
                classified.season = Some(season);
                classified.episode = None;
                classified.episode_end = None;
                classified.extra_kind = Some(extra.kind.slug());
                classified.extra_title = Some(extra.title);
                classified.extra_relative_path = Some(extra.relative_path);
                classified.extra_category_path = Some(extra.category_path)
                    .filter(|path| !path.is_empty());
                classified.extra_parent_title = None;
                classified.extra_parent_dir = None;
            } else if classified.episode.is_none() {
                let season = find_ancestor_season(dirs).map_or(0, |(_, season, _)| season);
                let category_start = dirs
                    .iter()
                    .rposition(|dir| is_season_folder(dir))
                    .map_or(show_idx + 1, |index| index + 1);
                let fallback_category = dirs[category_start..].join("/");
                let extra_relative_path = dirs[category_start..]
                    .iter()
                    .copied()
                    .chain(std::iter::once(file_name))
                    .collect::<Vec<_>>()
                    .join("/");

                classified.season = Some(season);
                classified.episode_end = None;
                classified.extra_kind = Some(crate::plex::PlexExtraKind::Other.slug());
                classified.extra_title = Some(clean_title(&clip_stem));
                classified.extra_relative_path = Some(extra_relative_path);
                classified.extra_category_path = Some(fallback_category)
                    .filter(|path| !path.is_empty());
                classified.extra_parent_title = None;
                classified.extra_parent_dir = None;
            }
        }
    }
    Some(classified)
}

/// The pre-existing show-name fallback (kept as its own function since it's
/// now used from two places): nearest-to-furthest, the first ancestor
/// directory that isn't itself a season folder.
fn show_title_from_ancestors(dirs: &[&str]) -> String {
    dirs.iter()
        .rev()
        .map(|d| clean_title(d))
        .find(|name| !name.is_empty() && !is_season_folder(name))
        .unwrap_or_default()
}

/// A season-indicating folder, any of three shapes: a literal `"Season N"`,
/// `parse_season_suffix_folder`'s `"<name> SNN"`, or a bare `"SNN"`
/// ([parse_bare_season_folder]) — see those functions. `is_season_folder`
/// treats all three as "skip this while hunting for a plain show-name
/// ancestor".
fn is_season_folder(name: &str) -> bool {
    let lower = name.to_lowercase();
    let literal = lower
        .strip_prefix("season")
        .map(|rest| rest.trim().bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(false);
    literal
        // Plex treats a `Specials` folder as season 0, exactly like
        // `Season 00` (issue #247).
        || lower == "specials"
        || parse_season_suffix_folder(name).is_some()
        || parse_bare_season_folder(name).is_some()
}

/// A folder name ending in `" SNN"` (a space, a case-insensitive `S`, then
/// 1-3 digits, at the very end) — e.g. `"Dexter (2006) S03"`,
/// `"Lost (2004) S03"`. Returns `(season, text before the suffix)` on
/// match. Deliberately conservative (the space before `S` is required, and
/// the text before it must be non-empty) — a false positive here would
/// misclassify a real movie as show content, a strictly worse failure than
/// the bonus-content-mistaken-for-a-movie bug this exists to fix.
fn parse_season_suffix_folder(name: &str) -> Option<(u32, &str)> {
    let bytes = name.as_bytes();
    let end = bytes.len();
    let mut digit_start = end;
    while digit_start > 0 && bytes[digit_start - 1].is_ascii_digit() {
        digit_start -= 1;
    }
    if digit_start == end || end - digit_start > 3 {
        return None;
    }
    if digit_start == 0 || !bytes[digit_start - 1].eq_ignore_ascii_case(&b's') {
        return None;
    }
    let s_pos = digit_start - 1;
    if s_pos == 0 || bytes[s_pos - 1] != b' ' {
        return None;
    }
    let season: u32 = name[digit_start..end].parse().ok()?;
    let show_part = name[..s_pos].trim_end();
    (!show_part.is_empty()).then_some((season, show_part))
}

/// A folder whose entire name is just a case-insensitive `S` followed by 1-2
/// digits and nothing else (e.g. `"S06"`, `"S6"`) — an abbreviated
/// alternative to the literal `"Season N"` folder, with an identical
/// relationship to its parent: the show name lives in the *next* real
/// ancestor above it, not in this folder itself (unlike
/// [parse_season_suffix_folder], which requires non-empty show-name text
/// *before* the "S" in the same folder — the two never both match the same
/// name). Deliberately whole-string, not a prefix/suffix match, so a folder
/// like `"S06E01"` (a real convention some rips use to flatten one episode
/// directly under a combined season+episode folder name) can never
/// misfire here — the trailing `"E01"` isn't all-digits, so it fails the
/// all-digit check below.
fn parse_bare_season_folder(name: &str) -> Option<u32> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || !bytes[0].eq_ignore_ascii_case(&b's') {
        return None;
    }
    let digits = &bytes[1..];
    if digits.is_empty() || digits.len() > 2 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    name[1..].parse().ok()
}

/// Walk `dirs` nearest-to-furthest looking for the first ancestor that's a
/// season folder, any of three shapes, and derive `(show_title, season, year)`:
/// - `"<Show Name> (<year>) SNN"` is self-contained — the show name and an
///   optional year both live in the same folder (run through the existing
///   [extract_year_and_strip] + [clean_title] pipeline, same as everywhere
///   else a folder/filename gets turned into a display name).
/// - a literal `"Season N"` or a bare `"SNN"` ([parse_bare_season_folder])
///   has no show name of its own — it comes from the next real ancestor
///   above it (skipping further season/disc folders). Prefers
///   [wrapper_derived_show_name] (the segment right below a recognized
///   Shows/TV wrapper) over [show_title_from_ancestors]'s naive nearest-
///   non-season-folder walk, which — confirmed live — can land on a
///   generic bonus-content wrapper folder instead of the real show when
///   one sits directly above the season folder (e.g. a doubly-nested
///   `.../The Office (US) (2005).../Featurettes/Featurettes/Season 1/...`,
///   where the naive walk stops at the inner `"Featurettes"` instead of
///   climbing two more levels to the actual show folder).
fn find_ancestor_season(dirs: &[&str]) -> Option<(String, u32, Option<u32>)> {
    for (idx, dir) in dirs.iter().enumerate().rev() {
        if let Some((season, show_part)) = parse_season_suffix_folder(dir) {
            let (stripped, year) = extract_year_and_strip(show_part);
            let show_title = clean_title(&stripped);
            if !show_title.is_empty() {
                return Some((show_title, season, year));
            }
            continue;
        }
        let lower = dir.to_lowercase();
        let literal_season = lower
            .strip_prefix("season")
            .and_then(|rest| rest.trim().parse().ok())
            .or_else(|| (lower == "specials").then_some(0));
        let Some(season) = literal_season.or_else(|| parse_bare_season_folder(dir)) else {
            continue;
        };
        // A `wrapper_derived_show_name` hit is left exactly as-is (see its
        // own doc comment and the `bonus_content_under_a_literal_season_
        // folder_finds_the_real_show_past_a_wrapper_folder` test — that
        // folder's quality/edition tags are deliberately kept). Only the
        // naive `show_title_from_ancestors` fallback needs a year stripped:
        // real libraries commonly name a show folder with its premiere
        // year (`"The Simpsons (1989)"`, the Sonarr/TVDB-matched default
        // folder convention), and with no wrapper folder above it to take
        // the raw-text path instead, that year previously stayed baked
        // into `show_title` verbatim and was sent to TMDb as part of the
        // search query — confirmed live for exactly this show.
        let (show_title, year) = match wrapper_derived_show_name(&dirs[..idx]) {
            Some(name) => (name, None),
            None => {
                let raw = show_title_from_ancestors(&dirs[..idx]);
                let (stripped, year) = extract_year_and_strip(&raw);
                (clean_title(&stripped), year)
            }
        };
        if !show_title.is_empty() {
            return Some((show_title, season, year));
        }
    }
    None
}

/// `(season, episode)` for any episode marker in `stem` — either `SxxEyy`
/// or `NxNN`. Public so subtitle sidecar matching ([`crate::subtitles`]) can
/// pin a `Show.S02E06.en.srt` file to the right episode entry.
pub fn episode_marker(stem: &str) -> Option<(u32, u32)> {
    parse_episode_marker(stem).map(|(season, episode, _)| (season, episode))
}

/// Find an episode marker in `stem`, either shape — `SxxEyy` tried first
/// (unchanged priority/behavior for every filename that already worked),
/// `NxNN` (see [parse_nxnn_marker]) as a fallback when that finds nothing.
/// Both return (season, episode, text before the marker).
fn parse_episode_marker(stem: &str) -> Option<(u32, u32, &str)> {
    parse_sxxeyy_marker(stem).or_else(|| parse_nxnn_marker(stem))
}

/// Find an `SxxEyy` marker (case-insensitive); returns (season, episode, text
/// before the marker). Common punctuation/spacing between the two halves is
/// accepted (`S02 E01`, `S02-E01`, `S02.E01`, `S02_E01`) because real DVD
/// rips frequently format the marker as two readable tokens rather than one.
fn parse_sxxeyy_marker(stem: &str) -> Option<(u32, u32, &str)> {
    let bytes = stem.as_bytes();
    let is_boundary_before = |i: usize| i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
    let is_boundary_at = |i: usize| i == bytes.len() || !bytes[i].is_ascii_alphanumeric();
    let is_separator = |byte: u8| matches!(byte, b' ' | b'.' | b'_' | b'-');
    for start in 0..bytes.len() {
        if !bytes[start].eq_ignore_ascii_case(&b's') || !is_boundary_before(start) {
            continue;
        }
        let mut i = start + 1;
        let season_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == season_start || i - season_start > 3 {
            continue;
        }
        let season_end = i;
        while i < bytes.len() && is_separator(bytes[i]) {
            i += 1;
        }
        if i >= bytes.len() || !bytes[i].eq_ignore_ascii_case(&b'e') {
            continue;
        }
        i += 1;
        while i < bytes.len() && is_separator(bytes[i]) {
            i += 1;
        }
        let episode_start = i;
        let mut j = episode_start;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == episode_start || j - episode_start > 4 || !is_boundary_at(j) {
            continue;
        }
        let season = stem[season_start..season_end].parse().ok()?;
        let episode = stem[episode_start..j].parse().ok()?;
        return Some((season, episode, &stem[..start]));
    }
    None
}

/// Find an `NxNN` marker (e.g. `6x09` = season 6, episode 9) — a common
/// real-world alternate to `SxxEyy` in manual/scene rips. Case-insensitive
/// on the `x`. Both digit runs must be genuinely bounded — string start/end,
/// or a non-alphanumeric byte on either side — so this can never match
/// inside a longer word or a real title that happens to contain a lowercase
/// "x" adjacent to digits (same bounding discipline as
/// [extract_bare_year_token]). Season is capped at 2 digits, episode at 3,
/// matching real-world shows' actual numbering ranges and keeping this from
/// false-positiving on an unrelated longer digit run.
fn parse_nxnn_marker(stem: &str) -> Option<(u32, u32, &str)> {
    let bytes = stem.as_bytes();
    let is_boundary_before = |i: usize| i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
    let is_boundary_at = |i: usize| i == bytes.len() || !bytes[i].is_ascii_alphanumeric();
    for x_pos in 0..bytes.len() {
        if !bytes[x_pos].eq_ignore_ascii_case(&b'x') {
            continue;
        }
        let mut season_start = x_pos;
        while season_start > 0 && bytes[season_start - 1].is_ascii_digit() {
            season_start -= 1;
        }
        let season_len = x_pos - season_start;
        if season_len == 0 || season_len > 2 || !is_boundary_before(season_start) {
            continue;
        }
        let episode_start = x_pos + 1;
        let mut episode_end = episode_start;
        while episode_end < bytes.len() && bytes[episode_end].is_ascii_digit() {
            episode_end += 1;
        }
        let episode_len = episode_end - episode_start;
        if episode_len == 0 || episode_len > 3 || !is_boundary_at(episode_end) {
            continue;
        }
        let season = stem[season_start..x_pos].parse().ok()?;
        let episode = stem[episode_start..episode_end].parse().ok()?;
        return Some((season, episode, &stem[..season_start]));
    }
    None
}

/// Leading track number: `01 - Title`, `01. Title`, `01_Title`, `01 Title`.
fn split_track_number(stem: &str) -> (Option<u32>, String) {
    let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return (None, clean_title(stem));
    }
    let rest = &stem[digits.len()..];
    let trimmed = rest.trim_start_matches([' ', '-', '.', '_']);
    if trimmed.is_empty() || trimmed.len() == rest.len() {
        // No separator after the digits ("1984.flac" stays a title).
        return (None, clean_title(stem));
    }
    (digits.parse().ok(), clean_title(trimmed))
}

/// Recover artist/album/track-number from the filename itself when there is
/// no artist folder to anchor from (see the `classify` call site). Real flat
/// libraries — everything dropped in one folder with no per-artist/per-album
/// structure, the dominant shape produced by older ripping/download tools —
/// encode the same fields with `" - "` as the separator:
/// `Artist - Album - Track.mp3`, `Artist - Album - 03 - Track.mp3`, or just
/// `Artist - Track.mp3` for a single/loose track. `title` has already been
/// through [`clean_title`] (so `.`/`_` separators read the same as spaces)
/// and had any *leading* digit-run track number stripped by
/// [`split_track_number`]; this only looks for one placed as its own
/// segment in the middle (`Artist - Album - 03 - Track`), since a leading
/// one is already handled before this runs.
///
/// A file with no `" - "` at all carries no recoverable signal, so it's
/// returned unchanged (`artist`/`album` stay `None`, same as before this
/// fallback existed) rather than guessed at. A two-segment name is read as
/// `Artist - Track` rather than `Album - Track`, since a loose/single track
/// with no album is the far more common real-world case than a lone track
/// carrying only its album name. This is a best-effort heuristic, not a
/// guarantee — a genuine one-word title that happens to contain " - " (e.g.
/// `"Come As You Are - Live Version"`) reads as an artist/title split, but
/// the alternative is the status quo: a flat-library track never enters
/// `scrape_tracks` in `scrape/runner.rs` at all (it requires both artist and
/// album to be non-empty), so it's never scraped no matter what.
fn parse_flat_track_fields(
    title: &str,
    track_number: Option<u32>,
) -> (Option<String>, Option<String>, Option<u32>, String) {
    let mut segments: Vec<&str> = title
        .split(" - ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return (None, None, track_number, title.to_string());
    }

    let mut track_number = track_number;
    if track_number.is_none() && segments.len() > 2 {
        let interior = &segments[1..segments.len() - 1];
        if let Some(offset) = interior
            .iter()
            .position(|s| !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit()))
        {
            let index = offset + 1;
            if let Ok(number) = segments[index].parse() {
                track_number = Some(number);
                segments.remove(index);
            }
        }
    }

    if segments.len() < 2 {
        return (None, None, track_number, clean_title(&segments.join(" - ")));
    }

    let artist = Some(clean_title(segments[0]));
    let album = (segments.len() > 2).then(|| clean_title(segments[1]));
    let title_start = if segments.len() > 2 { 2 } else { 1 };
    let title = clean_title(&segments[title_start..].join(" - "));
    (artist, album, track_number, title)
}

/// Remove every top-level `[...]`, `(...)`, `{...}` span from `text`
/// (mismatched/unterminated brackets are left as literal text, and a nested
/// span is swallowed whole by its enclosing one — non-nested filenames are
/// the only case that matters in practice), returning the stripped text and
/// the first bare 4-digit `1900..=2099` year found inside any span.
fn extract_bracket_tags(text: &str) -> (String, Option<u32>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut year = None;
    let mut i = 0;
    while i < chars.len() {
        let opener = chars[i];
        let closer = match opener {
            '[' => Some(']'),
            '(' => Some(')'),
            '{' => Some('}'),
            _ => None,
        };
        if let Some(closer) = closer {
            if let Some(offset) = chars[i + 1..].iter().position(|&c| c == closer) {
                let end = i + 1 + offset;
                if year.is_none() {
                    let inner: String = chars[i + 1..end].iter().collect();
                    year = parse_bare_year(&inner);
                }
                i = end + 1;
                continue;
            }
        }
        out.push(opener);
        i += 1;
    }
    (out, year)
}

fn parse_bare_year(inner: &str) -> Option<u32> {
    let trimmed = inner.trim();
    if trimmed.len() == 4 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        let year: u32 = trimmed.parse().ok()?;
        (1900..=2099).contains(&year).then_some(year)
    } else {
        None
    }
}

/// Try a bracketed year first (the more deliberate signal), then a
/// standalone unbracketed year token — see [extract_bracket_tags] and
/// [extract_bare_year_token] respectively.
fn extract_year_and_strip(text: &str) -> (String, Option<u32>) {
    let (stripped, year) = extract_bracket_tags(text);
    if year.is_some() {
        return (stripped, year);
    }
    extract_bare_year_token(&stripped)
}

/// Return the semantic movie title before the filename's release-year
/// boundary. Bracketed years take the same precedence as
/// [`extract_year_and_strip`]; otherwise the first occurrence of the
/// authoritative bare year is used so duplicated years are removed along
/// with the release suffix. An empty prefix is rejected so this extra
/// release-boundary cleanup never replaces the normal result with emptiness.
fn title_before_release_year(text: &str, release_year: u32) -> Option<String> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (position, &(start, opener)) in chars.iter().enumerate() {
        let closer = match opener {
            '[' => ']',
            '(' => ')',
            '{' => '}',
            _ => continue,
        };
        let Some((end, _)) = chars[position + 1..]
            .iter()
            .find(|(_, candidate)| *candidate == closer)
        else {
            continue;
        };
        if parse_bare_year(&text[start + opener.len_utf8()..*end]) == Some(release_year) {
            return cleaned_nonempty_prefix(&text[..start]);
        }
    }

    let year_text = release_year.to_string();
    let bytes = text.as_bytes();
    let is_sep = |byte: u8| matches!(byte, b'.' | b'_' | b' ');
    for (start, _) in text.match_indices(&year_text) {
        let end = start + year_text.len();
        let bounded_before = start == 0 || is_sep(bytes[start - 1]);
        let bounded_after = end == bytes.len() || is_sep(bytes[end]);
        if bounded_before && bounded_after {
            if let Some(title) = cleaned_nonempty_prefix(&text[..start]) {
                return Some(title);
            }
        }
    }
    None
}

fn cleaned_nonempty_prefix(prefix: &str) -> Option<String> {
    let (without_bracket_tags, _) = extract_bracket_tags(prefix);
    let title = clean_title(&without_bracket_tags);
    (!title.is_empty()).then_some(title)
}

/// Whole-word tokens that describe a *release* (genre label, language,
/// subtitle/audio note, resolution/source/codec) and never appear in a real
/// movie title. Used only to recognize descriptor junk sitting between the
/// title and the release year — see [strip_descriptor_dash_suffix].
const DESCRIPTOR_TOKENS: &[&str] = &[
    // genre labels some libraries append after the title
    "action",
    "adventure",
    "animation",
    "biography",
    "comedy",
    "crime",
    "documentary",
    "drama",
    "family",
    "fantasy",
    "horror",
    "musical",
    "mystery",
    "romance",
    "thriller",
    "war",
    "western",
    "scifi",
    // language / subtitle / audio notes
    "eng",
    "english",
    "sub",
    "subs",
    "subbed",
    "subtitle",
    "subtitles",
    "dub",
    "dubbed",
    "multi",
    "dual",
    // resolution / source / codec
    "480p",
    "720p",
    "1080p",
    "2160p",
    "4k",
    "uhd",
    "hdr",
    "bluray",
    "bdrip",
    "brrip",
    "dvdrip",
    "webrip",
    "hdtv",
    "hdrip",
    "x264",
    "x265",
    "h264",
    "h265",
    "hevc",
    "xvid",
    "divx",
    "aac",
    "ac3",
    "dts",
];

/// A personal-library convention: cast/genre/language descriptors appended
/// after the real movie title behind a spaced dash — `01 Die Hard - Bruce
/// Willis Action 1988 Eng Subs 1080p [H264-mp4]`. Returns `stem` truncated
/// at that dash when the text after it carries BOTH the release year and a
/// [DESCRIPTOR_TOKENS] word *before* that year (i.e. genuine junk wedged
/// between the title and the year). Deliberately conservative, the same
/// reasoning as [`crate::scrape`]'s `search_query_for`: a real film subtitle
/// set off the same way (`Mission Impossible - Ghost Protocol`,
/// `Star Wars - Episode IV - A New Hope`) carries neither signal, so it is
/// left untouched.
fn strip_descriptor_dash_suffix(stem: &str, release_year: Option<u32>) -> &str {
    let Some(year) = release_year else {
        return stem;
    };
    let year_text = year.to_string();
    for (index, _) in stem.match_indices(" - ") {
        let suffix = &stem[index + 3..];
        let Some(year_at) = bounded_token_position(suffix, &year_text) else {
            continue;
        };
        let has_descriptor_before_year = suffix[..year_at]
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| {
                !word.is_empty() && DESCRIPTOR_TOKENS.contains(&word.to_ascii_lowercase().as_str())
            });
        if has_descriptor_before_year {
            return &stem[..index];
        }
    }
    stem
}

/// Byte offset of the first occurrence of `token` in `text` that is bounded
/// by a non-alphanumeric byte (or the string edge) on both sides, so a
/// four-digit year is never matched inside a longer number.
fn bounded_token_position(text: &str, token: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let is_boundary = |b: u8| !b.is_ascii_alphanumeric();
    for (start, _) in text.match_indices(token) {
        let end = start + token.len();
        let before_ok = start == 0 || is_boundary(bytes[start - 1]);
        let after_ok = end == bytes.len() || is_boundary(bytes[end]);
        if before_ok && after_ok {
            return Some(start);
        }
    }
    None
}

/// Strip a zero-padded leading ordinal some libraries prefix onto every
/// movie file (`01 Die Hard`, `03. The Matrix`). Only a *zero-padded*
/// leading number counts — an unpadded one is far more likely to be real
/// title text (`10 Cloverfield Lane`, `28 Days Later`, `300`, `1917`), so
/// those are left completely alone.
fn strip_leading_movie_ordinal(title: &str) -> &str {
    if title.as_bytes().first() != Some(&b'0') {
        return title;
    }
    let digits_end = title
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(title.len());
    let rest = title[digits_end..].trim_start_matches([' ', '-', '.', '_']);
    if rest.is_empty() || rest.len() == title.len() - digits_end {
        // The whole string was digits, or no separator followed them.
        return title;
    }
    rest
}

/// Find and remove a standalone `1900..=2099` year token from `text`, where
/// tokens are separated by `.`/`_`/` ` (the same separators [clean_title]
/// collapses to spaces) or sit at the string's start/end — covers both the
/// dot-separated scene-release convention (`10.Cloverfield.Lane.2016.
/// 1080p...`) and the equally common plain-space convention (`Shaun of the
/// Dead 2004 (1080p...)`). Only a digit run bounded by a separator or the
/// string's start/end counts, so a year embedded in a longer digit run (a
/// resolution/bitrate number) is never mistaken for one.
///
/// Real bug this fixes: space wasn't originally a recognized separator at
/// all, so a plain-space filename's year silently stayed baked into the
/// title text instead of being captured — confirmed live: "Shaun of the
/// Dead 2004 (1080p x265 q22 FS78 Joy).mkv" searched TMDb for the literal
/// title "Shaun of the Dead 2004" (year field left `None`) instead of title
/// "Shaun of the Dead" + year 2004, and came back unmatched.
///
/// Scans the *whole* string, collecting every valid match, and treats the
/// **last** one's value as authoritative rather than the first —
/// deliberately, so a movie whose own title is a bare number that happens
/// to fall in 1900..=2099 (a real, released film literally titled "1917",
/// or a hypothetical "2012 2009 1080p.mkv") has its real trailing year
/// preferred over misreading the title itself as the year; scene-release/
/// plain-filename convention overwhelmingly places the year as the last
/// semantic token before quality/codec noise, never the first word of the
/// title. Same accepted-heuristic trade-off as every other name-based
/// convention in this module (e.g. [MEDIA_TYPE_WRAPPER_NAMES]) — not
/// airtight against every possible title, but strictly better than the
/// alternative of never capturing a bare year at all.
///
/// Every match sharing the authoritative value is stripped, not just the
/// last one — real bug, found live: `"Interstellar.2014.2014.1080p..."`
/// (the year genuinely repeated twice back to back) only had its second
/// occurrence removed, leaving `"Interstellar 2014"` as the actual search
/// title. A repeated *identical* year is always redundant duplication, safe
/// to collapse entirely; a match with a *different* value (the "1917"
/// case above) is left untouched, since that's real, deliberate title text.
fn extract_bare_year_token(text: &str) -> (String, Option<u32>) {
    let bytes = text.as_bytes();
    let is_sep = |b: u8| b == b'.' || b == b'_' || b == b' ';
    let mut matches: Vec<(usize, usize, u32)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut end = i;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let at_start = start == 0 || is_sep(bytes[start - 1]);
            let at_end = end == bytes.len() || is_sep(bytes[end]);
            if end - start == 4 && at_start && at_end {
                if let Some(year) = parse_bare_year(&text[start..end]) {
                    matches.push((start, end, year));
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    if let Some(&(_, _, authoritative_year)) = matches.last() {
        let mut out = String::with_capacity(text.len());
        let mut last_end = 0;
        for &(start, end, year) in &matches {
            if year == authoritative_year {
                out.push_str(&text[last_end..start]);
                last_end = end;
            }
        }
        out.push_str(&text[last_end..]);
        return (out, Some(authoritative_year));
    }
    (text.to_string(), None)
}

/// Normalize separators for display: dots/underscores to spaces, collapse runs.
fn clean_title(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_space = true;
    for ch in raw.chars() {
        let mapped = if ch == '.' || ch == '_' { ' ' } else { ch };
        if mapped == ' ' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(mapped);
            last_space = false;
        }
    }
    out.trim().trim_end_matches(['-', ' ']).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_track_with_artist_album_and_number() {
        let entry = classify("Pink Floyd/The Wall/05 - Hey You.flac").unwrap();
        assert_eq!(entry.kind, MediaKind::Track);
        assert_eq!(entry.title, "Hey You");
        assert_eq!(entry.artist.as_deref(), Some("Pink Floyd"));
        assert_eq!(entry.album.as_deref(), Some("The Wall"));
        assert_eq!(entry.track_number, Some(5));
    }

    #[test]
    fn disc_folder_is_absorbed() {
        let entry = classify("Artist/Album/CD2/03. Song.mp3").unwrap();
        assert_eq!(entry.album.as_deref(), Some("Album"));
        assert_eq!(entry.artist.as_deref(), Some("Artist"));
        assert_eq!(entry.track_number, Some(3));
    }

    #[test]
    fn numeric_title_without_separator_is_not_a_track_number() {
        let entry = classify("Artist/Album/1984.flac").unwrap();
        assert_eq!(entry.track_number, None);
        assert_eq!(entry.title, "1984");
    }

    /// Confirmed live against a real library: a DJ-mix/radio-broadcast-style
    /// folder structure nested past the expected two levels used to have its
    /// artist/album read from the *bottom* of the path (whatever was
    /// nearest the file), producing garbage like artist="01-29", album="29".
    /// Anchoring from the top and ignoring anything deeper fixes it.
    #[test]
    fn audio_track_nested_deeper_than_two_levels_still_groups_by_the_top_two_folders() {
        let entry = classify("Gabriel & Dresden/Organized Natures/01-29/29/track.mp3").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Gabriel & Dresden"));
        assert_eq!(entry.album.as_deref(), Some("Organized Natures"));
    }

    #[test]
    fn artist_discography_wrapper_suffix_is_stripped() {
        // Real bug, found live: "Kyau & Albert - Discography" and "Staind
        // - Discography" (666 real tracks combined) were treated as
        // literal artist names, breaking both display and every
        // MusicBrainz search built from them.
        let entry = classify("Kyau & Albert - Discography/Worldvibe/01 Track.flac").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Kyau & Albert"));
        let entry2 = classify("Staind - Discography/14 Shades of Grey/01 Track.flac").unwrap();
        assert_eq!(entry2.artist.as_deref(), Some("Staind"));
    }

    #[test]
    fn discography_suffix_stripping_does_not_eat_a_real_artist_literally_named_discography() {
        // An artist folder that's ONLY "Discography" (no real name left
        // after stripping) must keep the literal folder name rather than
        // becoming an empty artist.
        let entry = classify("Discography/Album/01 Track.flac").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Discography"));
    }

    #[test]
    fn audio_track_category_wrapper_folder_is_skipped_for_the_real_album_name() {
        let entry = classify("ATB/Album/Distant Earth/01 Show Me.flac").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("ATB"));
        assert_eq!(entry.album.as_deref(), Some("Distant Earth"));
    }

    #[test]
    fn nested_singles_artist_wrapper_uses_the_release_not_the_repeated_artist() {
        let entry = classify(
            "Tiesto/Singles/Tiesto/2010 - Tiesto - Goldrush [Magik Muzik 886-0] WEB/02 - Goldrush (Edit).mp3",
        )
        .unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Tiesto"));
        assert_eq!(
            entry.album.as_deref(),
            Some("2010 - Tiesto - Goldrush [Magik Muzik 886-0] WEB")
        );
    }

    #[test]
    fn artist_named_singles_collection_uses_its_child_release_as_album() {
        let entry = classify(
            "Gabriel & Dresden/Gabriel and Dresden - Singles/2004 - Arcadia/01 - Arcadia.mp3",
        )
        .unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Gabriel & Dresden"));
        assert_eq!(entry.album.as_deref(), Some("2004 - Arcadia"));
    }

    /// A category-named folder is only a wrapper when there's a real album
    /// segment beneath it to skip to — `Artist/Album/track.ext` (nothing
    /// after "Album") keeps "Album" as the literal album name rather than
    /// being swallowed with nothing left to replace it.
    #[test]
    fn audio_track_category_name_with_nothing_beneath_it_is_kept_literally() {
        let entry = classify("Someone/Album/track.mp3").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Someone"));
        assert_eq!(entry.album.as_deref(), Some("Album"));
    }

    #[test]
    fn audio_track_category_wrapper_folder_with_disc_subfolder_still_resolves_correctly() {
        let entry = classify("ATB/Compilation/Rare & Remixed/CD1/03 Track.flac").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("ATB"));
        assert_eq!(entry.album.as_deref(), Some("Rare & Remixed"));
    }

    #[test]
    fn audio_track_with_no_album_folder_groups_under_artist_only() {
        let entry = classify("Artist/Song.mp3").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Artist"));
        assert_eq!(entry.album, None);
    }

    #[test]
    fn flat_library_track_recovers_artist_and_album_from_the_filename() {
        // No artist folder at all — the dominant shape produced by older
        // ripping/download tools that dump everything into one folder.
        // Without the filename fallback this permanently fails to scrape
        // (`scrape_tracks` requires both artist and album non-empty).
        let entry = classify("Pink Floyd - The Wall - Comfortably Numb.mp3").unwrap();
        assert_eq!(entry.kind, MediaKind::Track);
        assert_eq!(entry.artist.as_deref(), Some("Pink Floyd"));
        assert_eq!(entry.album.as_deref(), Some("The Wall"));
        assert_eq!(entry.title, "Comfortably Numb");
        assert_eq!(entry.track_number, None);
    }

    #[test]
    fn flat_library_track_with_a_leading_track_number_is_still_recovered() {
        let entry = classify("01 - Pink Floyd - The Wall - Comfortably Numb.flac").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Pink Floyd"));
        assert_eq!(entry.album.as_deref(), Some("The Wall"));
        assert_eq!(entry.title, "Comfortably Numb");
        assert_eq!(entry.track_number, Some(1));
    }

    #[test]
    fn flat_library_track_with_an_embedded_middle_track_number_is_recovered() {
        // Some flat-library tools place the track number as its own
        // dash-separated segment rather than as a filename prefix.
        let entry = classify("Pink Floyd - The Wall - 06 - Comfortably Numb.mp3").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Pink Floyd"));
        assert_eq!(entry.album.as_deref(), Some("The Wall"));
        assert_eq!(entry.title, "Comfortably Numb");
        assert_eq!(entry.track_number, Some(6));
    }

    #[test]
    fn flat_library_loose_track_with_no_album_reads_as_artist_and_title() {
        let entry = classify("Radiohead - Creep.mp3").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Radiohead"));
        assert_eq!(entry.album, None);
        assert_eq!(entry.title, "Creep");
    }

    #[test]
    fn flat_library_track_under_a_bare_music_wrapper_is_also_recovered() {
        let entry = classify("Music/Pink Floyd - The Wall - Comfortably Numb.mp3").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Pink Floyd"));
        assert_eq!(entry.album.as_deref(), Some("The Wall"));
    }

    #[test]
    fn flat_library_track_with_no_separator_is_left_unrecovered() {
        // No "-" at all carries no recoverable signal; stays exactly as
        // before this fallback existed rather than guessing.
        let entry = classify("Comfortably Numb.mp3").unwrap();
        assert_eq!(entry.artist, None);
        assert_eq!(entry.album, None);
        assert_eq!(entry.title, "Comfortably Numb");
    }

    #[test]
    fn folder_based_classification_is_unaffected_by_dashes_in_the_title() {
        // A real artist/album folder structure must never be overridden by
        // the flat-filename fallback, even when the title itself contains
        // " - " (e.g. a live-version suffix).
        let entry =
            classify("Nirvana/Unplugged In New York/01 - Come As You Are - Live.flac").unwrap();
        assert_eq!(entry.artist.as_deref(), Some("Nirvana"));
        assert_eq!(entry.album.as_deref(), Some("Unplugged In New York"));
        assert_eq!(entry.title, "Come As You Are - Live");
        assert_eq!(entry.track_number, Some(1));
    }

    #[test]
    fn episode_marker_in_filename() {
        let entry = classify("tv/The Expanse/Season 2/The.Expanse.S02E05.Home.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.season, Some(2));
        assert_eq!(entry.episode, Some(5));
        assert_eq!(entry.show_title.as_deref(), Some("The Expanse"));
    }

    #[test]
    fn spaced_sxx_eyy_marker_from_real_dvd_rip_is_recognized() {
        let entry = classify(
            "Shows/The REAL ADVENTURES of JONNY QUEST/Season 2/\
             The Real Adventures of Jonny Quest - S02 E01 - The Mummies of Malenque \
             (480p - DVDRip).mp4",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(
            entry.show_title.as_deref(),
            Some("The REAL ADVENTURES of JONNY QUEST")
        );
        assert_eq!(entry.season, Some(2));
        assert_eq!(entry.episode, Some(1));
    }

    #[test]
    fn common_sxx_eyy_separators_are_recognized_without_false_word_matches() {
        for marker in ["S02E01", "S02 E01", "S02-E01", "S02.E01", "S02_E01"] {
            assert_eq!(
                parse_sxxeyy_marker(&format!("Show - {marker} - Title")).map(|v| (v.0, v.1)),
                Some((2, 1))
            );
        }
        assert_eq!(parse_sxxeyy_marker("ThingS02 E01"), None);
        assert_eq!(parse_sxxeyy_marker("Show S02 E01Title"), None);
    }

    #[test]
    fn episode_show_title_falls_back_to_directory() {
        let entry = classify("tv/Severance/Season 1/s01e03.mkv").unwrap();
        assert_eq!(entry.show_title.as_deref(), Some("Severance"));
        assert_eq!(entry.season, Some(1));
        assert_eq!(entry.episode, Some(3));
    }

    #[test]
    fn movie_title_cleanup() {
        // "2010" is a standalone dot-delimited token in the filename, so
        // extract_bare_year_token now captures and strips it — same
        // treatment a bracketed year already got. See year_captured_and_
        // stripped_from_bare_unbracketed_filename_token below for the
        // dedicated year-focused assertions on this exact pattern.
        let entry = classify("movies/Inception (2010)/Inception.2010.1080p.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.year, Some(2010));
        assert_eq!(entry.title, "Inception");
    }

    #[test]
    fn bracket_year_extracted_and_stripped_from_filename() {
        let entry = classify("movies/Interstellar (2014) [1080p].mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.year, Some(2014));
        assert!(
            !entry.title.contains('('),
            "brackets must not survive into the title: {}",
            entry.title
        );
        assert!(
            !entry.title.contains('['),
            "brackets must not survive into the title: {}",
            entry.title
        );
    }

    #[test]
    fn bracket_year_falls_back_to_the_enclosing_folder() {
        // No year anywhere in the filename (bracketed or bare) — this is
        // the case that genuinely needs the folder fallback, a common
        // real-world layout.
        let entry = classify("movies/Inception (2010)/Inception.1080p.mkv").unwrap();
        assert_eq!(entry.year, Some(2010));
        assert_eq!(
            entry.title, "Inception 1080p",
            "folder-derived year must not change the filename-derived title"
        );
    }

    #[test]
    fn year_captured_and_stripped_from_bare_unbracketed_filename_token() {
        // The dominant real-world scene-release convention has no brackets
        // at all — found live on real hardware: year stayed NULL for
        // exactly this pattern before this fix.
        let cloverfield =
            classify("movies/10.Cloverfield.Lane.2016.1080p.BluRay.x264-GROUP.mkv").unwrap();
        assert_eq!(cloverfield.year, Some(2016));
        assert_eq!(cloverfield.title, "10 Cloverfield Lane");

        let days_later = classify("movies/28.Days.Later.2002.1080p.BluRay.x264-GROUP.mkv").unwrap();
        assert_eq!(days_later.year, Some(2002));
        assert_eq!(days_later.title, "28 Days Later");
    }

    #[test]
    fn reported_scene_release_movie_names_stop_at_the_release_year() {
        for (filename, expected_title, expected_year) in [
            (
                "Social Network.2010.BD.Rip.1080p.h264.Rus.Eng.mkv",
                "Social Network",
                2010,
            ),
            (
                "The.Prestige.2006.CUSTOM.MULTi.VF2.1080p.HDLight.AC3.5.1.H264-LiHDL.mkv",
                "The Prestige",
                2006,
            ),
            (
                "Top.Gun.Maverick.2022.IMAX.1080p.Bluray.Atmos.TrueHD.7.1.x264-EVO.mkv",
                "Top Gun Maverick",
                2022,
            ),
            (
                "Waterworld.1995.The.Ulysses.Cut.1080p.BluRay.HEVC.x265-RiPRG.mkv",
                "Waterworld",
                1995,
            ),
        ] {
            let entry = classify(&format!("movies/{filename}")).unwrap();
            assert_eq!(entry.kind, MediaKind::Movie, "{filename}");
            assert_eq!(entry.title, expected_title, "{filename}");
            assert_eq!(entry.year, Some(expected_year), "{filename}");
        }
    }

    #[test]
    fn bare_year_token_must_be_separator_bounded_not_embedded_in_a_longer_run() {
        // A 4-digit run that's part of a longer digit sequence (a bitrate/
        // resolution-adjacent number) must never be mistaken for a year.
        let entry = classify("movies/Movie.19004.mkv").unwrap();
        assert_eq!(entry.year, None);
        assert_eq!(entry.title, "Movie 19004");
    }

    #[test]
    fn bracket_year_takes_precedence_over_a_bare_token_year() {
        let entry = classify("movies/Movie.2016.[2010].mkv").unwrap();
        assert_eq!(
            entry.year,
            Some(2010),
            "a deliberately bracketed year is the more deliberate signal"
        );
    }

    #[test]
    fn decorative_bracket_tags_are_discarded_without_setting_a_year() {
        let entry = classify("movies/Heat [x264] (YIFY) {web-dl}.mkv").unwrap();
        assert_eq!(entry.year, None);
        assert_eq!(entry.title, "Heat");
    }

    #[test]
    fn bracket_content_without_a_year_is_still_stripped() {
        let entry =
            classify("tv/Severance/Season 1/Severance.S01E01 [Good News About Hell].mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.year, None);
        assert!(!entry.title.contains('['));
    }

    #[test]
    fn sidecars_are_rejected() {
        assert!(classify("movies/Inception (2010)/poster.jpg").is_none());
        assert!(classify("movies/Inception (2010)/Inception.nfo").is_none());
        assert!(classify("movies/readme.txt").is_none());
    }

    // --- ancestor-season-folder-aware bonus/extra-content classification ---
    // Real bug: bonus content nested under a show's season folder had no
    // SxxEyy marker of its own, so it fell all the way through to
    // MediaKind::Movie and got scraped against a totally unrelated TMDb
    // movie. Confirmed live in the user's real library: this exact path got
    // matched to "The Interview" (2014), a real but completely wrong film.

    #[test]
    fn bonus_content_under_a_name_year_season_folder_is_attributed_to_the_show() {
        let entry = classify(
            "Batocera-movies-shows/Shows/Lost (2004)/Lost (2004) S03/Featurettes/Access - Granted/11. hostiles-others.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Lost"));
        assert_eq!(
            entry.season,
            Some(0),
            "bonus content is a single show-level bucket, not per-season"
        );
        assert_eq!(entry.episode, None);
        assert_eq!(
            entry.year,
            Some(2004),
            "year falls back to the season folder's own (year)"
        );
        assert_eq!(entry.title, "11 hostiles-others");
    }

    #[test]
    fn bonus_content_multiple_subfolders_deep_still_finds_the_season_folder() {
        // Same shape, deeper nesting (Featurettes/Interviews/), and the show
        // folder appears twice (plain "Dexter", then "Dexter (2006) S03") —
        // the nearest season-shaped ancestor wins, not the plain one above it.
        let entry =
            classify("Batocera-movies-shows/Shows/Dexter/Dexter (2006) S03/Featurettes/Interviews/Michael C. Hall.mkv")
                .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Dexter"));
        assert_eq!(entry.season, Some(0));
        assert_eq!(entry.episode, None);
        assert_eq!(entry.year, Some(2006));
        assert_eq!(entry.title, "Michael C Hall");
    }

    #[test]
    fn real_numbered_episode_under_the_name_year_season_folder_shape_is_unaffected() {
        // The new folder shape must not steal season/episode numbers away
        // from a real SxxEyy filename marker — that's still the primary,
        // authoritative signal when present.
        let entry =
            classify("Shows/Dexter/Dexter (2006) S03/Dexter.S03E01.Our Father.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Dexter"));
        assert_eq!(entry.season, Some(3));
        assert_eq!(entry.episode, Some(1));
    }

    #[test]
    fn real_numbered_episode_with_no_stem_prefix_falls_back_to_the_season_folder_for_show_title() {
        // Filename has no text before the SxxEyy marker at all — show_title
        // must now also try the new folder shape, not just the old
        // plain-directory fallback.
        let entry = classify("Shows/Dexter/Dexter (2006) S03/S03E01.Our Father.mkv").unwrap();
        assert_eq!(entry.show_title.as_deref(), Some("Dexter"));
        assert_eq!(entry.season, Some(3));
        assert_eq!(entry.episode, Some(1));
    }

    // --- "Ep. NN" episode markers and Shows/TV wrapper fallback ---
    // Real bug: a show numbered "Ep. NN" (no SxxEyy, no season folder) fell
    // through to the movie fallback and was searched against the wrong
    // TMDb database, always coming back "not found" even though the show
    // exists — confirmed live against "The CENTURIONS".

    #[test]
    fn ep_dot_marker_with_no_season_folder_defaults_to_season_one() {
        let entry = classify(
            "Batocera-movies-shows/Shows/The CENTURIONS/CENTURIONS - Ep. 57 - Hole in the Ocean, Part 2 (480p - DVDRip).mp4",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("The CENTURIONS"));
        assert_eq!(entry.season, Some(1));
        assert_eq!(entry.episode, Some(57));
    }

    #[test]
    fn ep_marker_without_a_trailing_dot_is_also_recognized() {
        // Real example reported live: "CENTURIONS - Ep 20 - Terror on Ice"
        // — no "." after "Ep", unlike the earlier "Ep. 57" example.
        let entry = classify(
            "Batocera-movies-shows/Shows/The CENTURIONS/CENTURIONS - Ep 20 - Terror on Ice.mp4",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("The CENTURIONS"));
        assert_eq!(entry.season, Some(1));
        assert_eq!(entry.episode, Some(20));
    }

    #[test]
    fn episode_word_marker_is_also_recognized() {
        let entry = classify("Shows/Some Show/Some Show Episode 12.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Some Show"));
        assert_eq!(entry.episode, Some(12));
    }

    #[test]
    fn ep_marker_does_not_false_positive_inside_a_longer_word() {
        // "Deep"/"Sleep"/"Prep" all contain "ep" as a substring but not at a
        // word boundary — must not be mistaken for an episode marker.
        let entry = classify("movies/Deep Impact (1998)/Deep Impact.1998.1080p.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.episode, None);
    }

    #[test]
    fn ep_marker_uses_an_ancestor_season_folder_when_one_exists() {
        let entry =
            classify("Shows/Dexter/Dexter (2006) S03/Dexter - Ep. 5 - Our Father.mkv").unwrap();
        assert_eq!(entry.show_title.as_deref(), Some("Dexter"));
        assert_eq!(entry.season, Some(3));
        assert_eq!(entry.episode, Some(5));
        assert_eq!(entry.year, Some(2006));
    }

    #[test]
    fn bonus_content_under_a_shows_wrapper_with_no_season_folder_is_attributed_to_the_show() {
        // Real bug: arbitrarily-nested bonus content (no episode marker, no
        // Season N folder anywhere) under a "Shows/<Show>/..." tree fell
        // all the way through to the movie fallback and never appeared
        // under its show. Confirmed live against "Aqua Teen Hunger Force".
        let entry = classify(
            "Batocera-movies-shows/Shows/Aqua Teen Hunger Force/Featurettes/The Movie/Deleted Scenes/Dorm Room Extended.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Aqua Teen Hunger Force"));
        assert_eq!(entry.season, Some(0));
        assert_eq!(entry.episode, None);
    }

    #[test]
    fn typed_shows_root_attaches_deeply_nested_plex_extras_to_its_root_show() {
        let entry = classify_for_asset_type(
            "Aqua Teen Hunger Force/Featurettes/The Movie/Deleted Scenes/Dorm Room Extended.mkv",
            MediaRootAssetType::Shows,
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Aqua Teen Hunger Force"));
        assert_eq!(entry.season, Some(0));
        assert_eq!(entry.episode, None);
        assert_eq!(entry.extra_kind, Some("deletedScene"));
        assert_eq!(entry.extra_title.as_deref(), Some("Dorm Room Extended"));
        assert_eq!(
            entry.extra_category_path.as_deref(),
            Some("Featurettes/The Movie/Deleted Scenes")
        );
    }

    #[test]
    fn typed_shows_root_keeps_season_extras_with_their_season() {
        let entry = classify_for_asset_type(
            "The X-Files/Season 7/Featurettes/Deleted Scenes.mkv",
            MediaRootAssetType::Shows,
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("The X-Files"));
        assert_eq!(entry.season, Some(7));
        assert_eq!(entry.episode, None);
        assert_eq!(entry.extra_kind, Some("featurette"));
        assert_eq!(entry.extra_title.as_deref(), Some("Deleted Scenes"));
    }

    #[test]
    fn typed_shows_root_keeps_numbered_files_inside_featurettes_as_extras() {
        let entry = classify_for_asset_type(
            "The Office/Season 02/Featurettes/Featurettes - S02E04.mkv",
            MediaRootAssetType::Shows,
        )
        .unwrap();
        assert_eq!(entry.show_title.as_deref(), Some("The Office"));
        assert_eq!(entry.season, Some(2));
        assert_eq!(entry.episode, None);
        assert_eq!(entry.extra_kind, Some("featurette"));
        assert_eq!(entry.extra_title.as_deref(), Some("Featurettes - S02E04"));
    }

    #[test]
    fn space_separated_bare_year_is_captured_and_stripped() {
        // Real bug: this exact filename searched TMDb for the literal title
        // "Shaun of the Dead 2004" (year left unset) instead of title
        // "Shaun of the Dead" + year 2004, and came back unmatched.
        let entry =
            classify("Batocera-movies-shows/Shaun of the Dead 2004 (1080p x265 q22 FS78 Joy).mkv")
                .unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.title, "Shaun of the Dead");
        assert_eq!(entry.year, Some(2004));
    }

    #[test]
    fn space_separated_bare_year_prefers_the_last_match_over_a_numeral_title() {
        // A movie whose own title is a bare number that happens to fall in
        // the recognized year range ("1917") must not have that number
        // mistaken for the year when a real trailing year is also present.
        let entry = classify("movies/1917 2019 1080p BluRay x264.mkv").unwrap();
        assert_eq!(entry.year, Some(2019));
        assert_eq!(entry.title, "1917");
    }

    #[test]
    fn a_year_repeated_back_to_back_is_fully_collapsed_not_just_its_last_occurrence() {
        // Real bug, found live: "Interstellar.2014.2014.1080p..." only had
        // its *second* "2014" removed, leaving "Interstellar 2014" as the
        // literal search title — a redundant repeated year (identical
        // value both times) must be stripped entirely, unlike two
        // *different* year-shaped tokens (see the "1917" test above).
        let entry = classify("movies/Interstellar.2014.2014.1080p.BluRay.x264.YIFY.mp4").unwrap();
        assert_eq!(entry.year, Some(2014));
        assert_eq!(entry.title, "Interstellar");
    }

    #[test]
    fn bonus_content_under_a_shows_wrapper_handles_arbitrary_extra_nesting() {
        // Real bug: any additional nesting depth under a show folder (not
        // just one level, as the earlier featurettes example covered) must
        // still resolve to the show — confirmed live against a
        // "Featurettes/The Movie/Promo Material/..." tree with no episode
        // marker and no Season N folder anywhere.
        let entry = classify(
            "Batocera-movies-shows/Shows/Aqua Teen Hunger Force/Featurettes/The Movie/Promo Material/Teaser.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Aqua Teen Hunger Force"));
        assert_eq!(entry.season, Some(0));
    }

    #[test]
    fn bonus_content_with_its_own_season_zero_marker_uses_the_wrapper_show_not_the_containing_folder(
    ) {
        // Real bug, found live: a file with its own `S00E02`-style marker
        // sitting directly inside a generic bonus-content folder
        // ("Featurettes") — no season-shaped ancestor, no text before the
        // marker — used to fall back to `show_title_from_ancestors`, which
        // naively picked the nearest folder name and got "Featurettes"
        // itself instead of the real show.
        let entry = classify(
            "Batocera-movies-shows/Shows/Aqua Teen Hunger Force/Featurettes/S00E02 Boston [youtube rip].mp4",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Aqua Teen Hunger Force"));
        assert_eq!(entry.season, Some(0));
        assert_eq!(entry.episode, Some(2));
    }

    #[test]
    fn shows_wrapper_fallback_does_not_fire_for_a_flat_movie_library() {
        // Regression guard: a plain movie library with no "Shows"/"TV"
        // segment anywhere must be completely unaffected.
        let entry = classify("Batocera-movies-shows/Blade 2 (1080p).mp4").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.show_title, None);
    }

    #[test]
    fn deeply_nested_movie_with_no_season_folder_anywhere_is_not_reclassified() {
        // Regression guard: the new ancestor walk must never fire for a
        // plain movie just because it happens to be nested a few folders
        // deep with no season-folder signal anywhere in its ancestry.
        let entry = classify(
            "movies/Action/Best Of/Really Good Movie (2020)/Really Good Movie.2020.1080p.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.show_title, None);
    }

    #[test]
    fn season_suffix_folder_parsing_is_conservative_about_false_positives() {
        // No space before the "S" — must not match (a false positive here
        // would misclassify a real movie, worse than the bug being fixed).
        assert_eq!(parse_season_suffix_folder("MarsS03"), None);
        // Nothing before the "S" at all.
        assert_eq!(parse_season_suffix_folder("S03"), None);
        // Trailing digits with no "S" before them.
        assert_eq!(parse_season_suffix_folder("Volume 03"), None);
        // A real, valid match, no parenthetical year needed.
        assert_eq!(parse_season_suffix_folder("Show S03"), Some((3, "Show")));
    }

    // --- NxNN episode markers and bare SNN season folders ---
    // Real bug, reported after the ancestor-season-folder fix above landed:
    // neither an "NxNN" episode marker nor a bare "S06" season folder (just
    // the abbreviation, no show name attached — that lives in the parent
    // folder) was recognized at all, so a real, correctly-numbered episode
    // still fell through to MediaKind::Movie.

    #[test]
    fn nxnn_episode_marker_under_a_bare_season_folder_is_recognized() {
        let entry = classify(
            "Batocera-movies-shows/Shows/Law & Order SVU/S06/Law & Order Special Victims Unit - 6x09 - Weak.mp4",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.season, Some(6));
        assert_eq!(entry.episode, Some(9));
        // The bare "S06" ancestor folder resolves to a real show name (the
        // parent folder, "Law & Order SVU") — that folder-derived name now
        // wins over the filename's own stem-prefix text ("Law & Order
        // Special Victims Unit"), even though it's non-empty. See the
        // real-world reasoning on the show_title derivation above the
        // marker-branch `return` in `classify()`.
        assert_eq!(entry.show_title.as_deref(), Some("Law & Order SVU"));
    }

    #[test]
    fn folder_derived_show_name_wins_over_inconsistent_filename_wording_across_a_real_season() {
        // Real bug, found live: one folder's worth of episodes sourced from
        // different release groups had 7 filenames (out of 580) that
        // disagreed in wording with the rest ("SVU" vs "Special Victims
        // Unit", "and"/"And"/"&") — each splintering into its own
        // show_title despite being the exact same folder and show.
        for filename in [
            "Law and Order Special Victims Unit S27E02 A Waiver of Consent 1080p WEBRip 10bit DDP5 1 HEVC-d3g.mkv",
            "Law and Order SVU S27E05 1080p AMZN WEB-DL DDP5 1 H 264-FLUX.mkv",
            "Law.And.Order.SVU.S27E11.1080p.WEB.h264-ETHEL[EZTVx.to].mkv",
            "Law & Order Special Victims Unit - S27E20 - Odd Man Out.mkv",
        ] {
            let path = format!("Batocera-movies-shows/Shows/Law & Order SVU/S27/{filename}");
            let entry = classify(&path).unwrap();
            assert_eq!(entry.kind, MediaKind::Episode, "{filename}");
            assert_eq!(entry.show_title.as_deref(), Some("Law & Order SVU"), "{filename}");
            assert_eq!(entry.season, Some(27), "{filename}");
        }
    }

    #[test]
    fn nxnn_marker_show_title_falls_back_to_the_bare_season_folders_parent_when_stem_has_no_prefix()
    {
        // No text at all before the "6x09" marker — show_title must fall
        // back through find_ancestor_season, which for a bare "S06" folder
        // (no show name of its own) takes the name from the *parent* folder
        // above it, the same relationship a literal "Season N" folder has.
        let entry = classify("Shows/Law & Order SVU/S06/6x09.mp4").unwrap();
        assert_eq!(entry.season, Some(6));
        assert_eq!(entry.episode, Some(9));
        assert_eq!(entry.show_title.as_deref(), Some("Law & Order SVU"));
    }

    #[test]
    fn bonus_content_under_a_literal_season_folder_finds_the_real_show_past_a_wrapper_folder() {
        // Real bug, found live: bonus content organized as its own doubly-
        // nested "Featurettes/Featurettes/Season 1" structure (an unusual
        // but real-world layout — an outer bonus-content category folder,
        // then a per-season breakdown of that bonus content) was scraped
        // as a show called "Featurettes" instead of "The Office (US)
        // (2005)". show_title_from_ancestors's naive nearest-non-season-
        // folder walk stopped at the inner "Featurettes" (itself not a
        // season folder) without ever reaching the real show folder two
        // levels further up. find_ancestor_season must prefer the
        // wrapper-derived show name (the segment right below the "Shows"
        // wrapper) over that naive walk.
        let path = "Batocera-movies-shows/Shows/The Office (US) (2005) Season 1-9 S01-S09 (1080p BluRay x265 HEVC 10bit AAC 5.1 Silence)/Featurettes/Featurettes/Season 1/The Making of the Pilot.mkv";
        let entry = classify(path).unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        // wrapper_derived_show_name doesn't run extract_year_and_strip (only
        // find_ancestor_season's other, self-contained "<Show> (<year>) SNN"
        // branch does) — same raw, uncleaned-of-year/quality-tags shape the
        // pre-existing wrapper_derived_show_name(&dirs) fallback a few lines
        // below already produces for the sibling "bonus content directly
        // under the show folder, no season folder at all" case.
        assert_eq!(
            entry.show_title.as_deref(),
            Some("The Office (US) (2005) Season 1-9 S01-S09 (1080p BluRay x265 HEVC 10bit AAC 5 1 Silence)")
        );
        assert_eq!(entry.season, Some(0));
    }

    #[test]
    fn nxnn_marker_is_conservative_about_false_positives() {
        // No digits before the "x" at all.
        assert_eq!(parse_nxnn_marker("Show - x09 - Title"), None);
        // No digits after the "x" at all.
        assert_eq!(parse_nxnn_marker("Show - 6x - Title"), None);
        // Digits before "x" exist and are the right count, but the run
        // isn't left-bounded — "23" is preceded directly by "m" (from
        // "Item"), not a separator, so this is part of a longer
        // alphanumeric run, not a real marker — must not misfire.
        assert_eq!(parse_nxnn_marker("Item23x09"), None);
        // A real, valid match with mixed case "X".
        assert_eq!(
            parse_nxnn_marker("Show - 6X09 - Title"),
            Some((6, 9, "Show - "))
        );
    }

    #[test]
    fn bare_season_folder_parsing_is_conservative_about_false_positives() {
        // A combined season+episode folder name ("S06E01") is a different
        // real convention (one episode flattened directly under a folder
        // naming both numbers) — must not be misread as a bare season
        // folder just because it starts with "S" + digits.
        assert_eq!(parse_bare_season_folder("S06E01"), None);
        // Nothing after the "S" at all.
        assert_eq!(parse_bare_season_folder("S"), None);
        // Doesn't start with "S".
        assert_eq!(parse_bare_season_folder("06"), None);
        // Real, valid matches, both digit widths, case-insensitive.
        assert_eq!(parse_bare_season_folder("S06"), Some(6));
        assert_eq!(parse_bare_season_folder("s6"), Some(6));
    }

    // --- absolute-numbered episodes and year-bearing show folders ---
    // Real bug, reported against The Simpsons: a "Complete Series" rip
    // organized as `<Show> (<year>)/Season NN/<absolute number> - <title>`
    // with no SxxEyy/NxNN/Ep marker anywhere was falling into the season-0
    // bonus bucket with no episode number, and the show folder's own
    // "(1989)" year was leaking into the show_title used for TMDb search.

    #[test]
    fn absolute_numbered_episode_under_a_season_folder_recovers_season_and_episode() {
        let entry = classify(
            "Shows/The Simpsons/Season 01/101 - Simpsons Roasting on an Open Fire.avi",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("The Simpsons"));
        assert_eq!(entry.season, Some(1));
        assert_eq!(entry.episode, Some(1));
    }

    #[test]
    fn absolute_numbered_episode_with_two_digit_season_recovers_correctly() {
        let entry = classify(
            "Shows/The Simpsons/Season 28/2822 - Dad Behavior.mkv",
        )
        .unwrap();
        assert_eq!(entry.season, Some(28));
        assert_eq!(entry.episode, Some(22));
    }

    #[test]
    fn plain_numbered_episode_under_a_season_folder_uses_the_number_directly() {
        // No absolute-numbering signal (leading digits don't start with the
        // season number) — read as a plain per-season episode number rather
        // than treated as bonus content.
        let entry = classify("Shows/Dexter/Season 03/07 - Easy as Pie.mkv").unwrap();
        assert_eq!(entry.season, Some(3));
        assert_eq!(entry.episode, Some(7));
    }

    #[test]
    fn genuine_bonus_content_under_a_season_folder_is_still_season_zero() {
        // No leading digit run at all — must remain the pre-existing
        // season-0 "Specials" bucket, not misread as episode content.
        let entry =
            classify("Shows/The Simpsons/Season 01/Deleted Scene.mkv").unwrap();
        assert_eq!(entry.season, Some(0));
        assert_eq!(entry.episode, None);
    }

    #[test]
    fn year_in_show_folder_name_is_stripped_from_the_derived_show_title() {
        let entry = classify(
            "The Simpsons (1989)/Season 01/The Simpsons - S01E01 - Simpsons Roasting on an Open Fire.mkv",
        )
        .unwrap();
        assert_eq!(entry.show_title.as_deref(), Some("The Simpsons"));
        assert_eq!(entry.season, Some(1));
        assert_eq!(entry.episode, Some(1));
    }

    // --- movie "local extras" subfolders ---
    // Featurettes/Specials/Deleted Scenes/... under a movie folder hold
    // bonus material for that movie, not separate films.

    #[test]
    fn movie_featurette_is_attributed_to_the_parent_movie() {
        let entry = classify("movies/Inception (2010)/Featurettes/The Making Of.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.year, Some(2010));
        assert_eq!(entry.title, "Inception - The Making Of");
    }

    #[test]
    fn movie_extra_nested_deeper_still_resolves_to_the_movie() {
        let entry =
            classify("Movies/Blade Runner (1982)/Extras/Interviews/Ridley Scott.mp4").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.year, Some(1982));
        assert_eq!(entry.title, "Blade Runner - Ridley Scott");
    }

    #[test]
    fn nested_movie_extra_uses_nearest_category_and_accepts_a_yearless_movie_folder() {
        let entry = classify(
            "Movies/Aqua Teen Hunger Force/Featurettes/The Movie/Deleted Scenes/Dorm Room Extended.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.title, "Aqua Teen Hunger Force - Dorm Room Extended");
        assert_eq!(entry.year, None);
        assert_eq!(entry.extra_kind, Some("deletedScene"));
        assert_eq!(entry.extra_title.as_deref(), Some("Dorm Room Extended"));
        assert_eq!(entry.extra_parent_dir.as_deref(), Some("Movies/Aqua Teen Hunger Force"));
        assert_eq!(
            entry.extra_relative_path.as_deref(),
            Some("Featurettes/The Movie/Deleted Scenes/Dorm Room Extended.mkv")
        );
        assert_eq!(
            entry.extra_category_path.as_deref(),
            Some("Featurettes/The Movie/Deleted Scenes")
        );
    }

    #[test]
    fn nested_movie_extra_inherits_outer_category_without_a_deeper_override() {
        let entry = classify(
            "Movies/Aqua Teen Hunger Force/Featurettes/The Movie/Making Of.mkv",
        )
        .unwrap();
        assert_eq!(entry.extra_kind, Some("featurette"));
        assert_eq!(entry.extra_title.as_deref(), Some("Making Of"));
    }

    #[test]
    fn extras_folder_without_a_parent_movie_is_not_invented_as_a_movie() {
        let entry = classify("Trailers/Upcoming Thing.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.title, "Upcoming Thing");
        assert_eq!(entry.extra_kind, None);
    }

    #[test]
    fn show_bonus_content_still_wins_over_the_movie_extras_branch() {
        // A "Featurettes" folder under a Shows wrapper must still classify
        // as episode/season-zero content, not a movie extra.
        let entry = classify(
            "Batocera-movies-shows/Shows/Lost (2004)/Lost (2004) S03/Featurettes/clip.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Lost"));
        assert_eq!(entry.season, Some(0));
        assert_eq!(entry.extra_kind, Some("featurette"));
        assert_eq!(entry.extra_title.as_deref(), Some("clip"));
        assert_eq!(entry.extra_category_path.as_deref(), Some("Featurettes"));
        assert_eq!(
            entry.extra_relative_path.as_deref(),
            Some("Featurettes/clip.mkv")
        );
    }

    #[test]
    fn nxnn_marker_does_not_steal_priority_from_an_existing_sxxeyy_marker() {
        // A filename with a real SxxEyy marker must keep using it even if
        // an NxNN-shaped substring could also coincidentally be found —
        // SxxEyy is tried first, unconditionally, unchanged from before.
        let entry = classify("Shows/The Expanse/Season 2/The.Expanse.S02E05.Home.mkv").unwrap();
        assert_eq!(entry.season, Some(2));
        assert_eq!(entry.episode, Some(5));
    }

    // --- personal-library movie filenames: leading ordinal + descriptor
    //     suffix wedged between the title and the release year ---
    // Reported in #199: "01 Die Hard - Bruce Willis Action 1988 Eng Subs
    // 1080p [H264-mp4].mp4" scraped as the literal title "01 Die Hard -
    // Bruce Willis Action" instead of "Die Hard".

    #[test]
    fn reported_ordinal_and_cast_genre_suffix_movie_name_reduces_to_the_bare_title() {
        let entry =
            classify("movies/01 Die Hard - Bruce Willis Action 1988 Eng Subs 1080p [H264-mp4].mp4")
                .unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.title, "Die Hard");
        assert_eq!(entry.year, Some(1988));
    }

    #[test]
    fn zero_padded_leading_ordinal_alone_is_stripped_from_a_movie_title() {
        let entry = classify("movies/03. The Matrix 1999 1080p BluRay.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.title, "The Matrix");
        assert_eq!(entry.year, Some(1999));
    }

    #[test]
    fn unpadded_leading_number_in_a_movie_title_is_never_treated_as_an_ordinal() {
        // Regression guard for the existing "10 Cloverfield Lane"/"28 Days
        // Later" behavior and bare-numeral titles like "300".
        for (filename, expected) in [
            (
                "10.Cloverfield.Lane.2016.1080p.BluRay.x264-GROUP.mkv",
                "10 Cloverfield Lane",
            ),
            (
                "28.Days.Later.2002.1080p.BluRay.x264-GROUP.mkv",
                "28 Days Later",
            ),
            ("300 2006 1080p BluRay.mkv", "300"),
        ] {
            let entry = classify(&format!("movies/{filename}")).unwrap();
            assert_eq!(entry.title, expected, "{filename}");
        }
    }

    #[test]
    fn a_real_film_subtitle_set_off_by_a_dash_is_not_mistaken_for_a_descriptor_suffix() {
        // Neither dash suffix carries a descriptor token before the year, so
        // the whole title is kept.
        let entry =
            classify("movies/Star Wars - Episode IV - A New Hope 1977 1080p BluRay x264.mkv")
                .unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.title, "Star Wars - Episode IV - A New Hope");
        assert_eq!(entry.year, Some(1977));

        let entry =
            classify("movies/Mission Impossible - Ghost Protocol (2011) [1080p].mkv").unwrap();
        assert_eq!(entry.title, "Mission Impossible - Ghost Protocol");
        assert_eq!(entry.year, Some(2011));
    }

    // --- Plex compatibility layer (issue #247) ---

    #[test]
    fn plex_movie_id_and_edition_tokens_are_captured_and_stripped_from_the_title() {
        let entry = classify(
            "Movies/Blade Runner (1982) {edition-Final Cut} {imdb-tt0083658}/Blade Runner (1982) {edition-Final Cut} {imdb-tt0083658}.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.title, "Blade Runner");
        assert_eq!(entry.year, Some(1982));
        assert_eq!(entry.plex_guid.as_deref(), Some("imdb-tt0083658"));
        assert_eq!(entry.edition.as_deref(), Some("Final Cut"));
    }

    #[test]
    fn plex_show_tvdb_id_on_the_show_folder_is_captured_for_an_episode() {
        let entry = classify(
            "The Wire (2002) {tvdb-79126}/Season 01/The Wire (2002) - S01E01 - The Target.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("The Wire"));
        assert_eq!(entry.season, Some(1));
        assert_eq!(entry.episode, Some(1));
        assert_eq!(entry.plex_guid.as_deref(), Some("tvdb-79126"));
    }

    #[test]
    fn plex_id_token_is_stripped_from_a_wrapper_derived_show_name() {
        let entry = classify(
            "TV/The Wire (2002) {tvdb-79126}/Season 01/The Wire (2002) - S01E01 - The Target.mkv",
        )
        .unwrap();
        let show = entry.show_title.unwrap();
        assert!(show.contains("The Wire"), "{show}");
        assert!(!show.contains("tvdb"), "{show}");
        assert_eq!(entry.plex_guid.as_deref(), Some("tvdb-79126"));
    }

    #[test]
    fn plex_multi_episode_file_records_the_span_end() {
        let entry =
            classify("TV/Firefly/Season 01/Firefly - S01E01-E02 - Serenity.mkv").unwrap();
        assert_eq!(entry.season, Some(1));
        assert_eq!(entry.episode, Some(1));
        assert_eq!(entry.episode_end, Some(2));
    }

    #[test]
    fn plex_movie_extra_by_filename_suffix_groups_with_the_feature() {
        let entry = classify("Movies/Sintel (2010)/Sintel (2010)-trailer.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.title, "Sintel");
        assert_eq!(entry.year, Some(2010));
        assert_eq!(entry.extra_kind, Some("trailer"));
    }

    #[test]
    fn plex_movie_extra_in_a_canonical_extras_folder_carries_its_category() {
        let entry =
            classify("Movies/Inception (2010)/Behind The Scenes/The Making Of.mkv").unwrap();
        assert_eq!(entry.kind, MediaKind::Movie);
        assert_eq!(entry.extra_kind, Some("behindTheScenes"));
    }

    #[test]
    fn plex_tv_specials_folder_is_season_zero_with_an_extras_category_when_applicable() {
        let entry = classify(
            "TV/Doctor Who (2005)/Specials/Deleted Scenes/An Unearthly Cut.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.season, Some(0));
        assert_eq!(entry.extra_kind, Some("deletedScene"));
        assert_eq!(entry.extra_title.as_deref(), Some("An Unearthly Cut"));
        assert_eq!(
            entry.extra_category_path.as_deref(),
            Some("Deleted Scenes")
        );
        assert_eq!(
            entry.extra_relative_path.as_deref(),
            Some("Deleted Scenes/An Unearthly Cut.mkv")
        );
    }

    #[test]
    fn show_bonus_content_directly_under_a_shows_wrapper_carries_extras_detail() {
        // No `Season N` ancestor at all — the file sits directly under a
        // recognized extras folder one level below the show itself, so this
        // exercises the `wrapper_derived_show_name` branch rather than the
        // season-folder branch covered above.
        let entry = classify(
            "Shows/Aqua Teen Hunger Force/Featurettes/Making the Show.mkv",
        )
        .unwrap();
        assert_eq!(entry.kind, MediaKind::Episode);
        assert_eq!(entry.show_title.as_deref(), Some("Aqua Teen Hunger Force"));
        assert_eq!(entry.season, Some(0));
        assert_eq!(entry.extra_kind, Some("featurette"));
        assert_eq!(entry.extra_title.as_deref(), Some("Making the Show"));
        assert_eq!(entry.extra_category_path.as_deref(), Some("Featurettes"));
        assert_eq!(
            entry.extra_relative_path.as_deref(),
            Some("Featurettes/Making the Show.mkv")
        );
    }

    #[test]
    fn descriptor_dash_suffix_needs_both_a_year_and_a_descriptor_before_it() {
        // A dash suffix with a descriptor but no year (e.g. an unusual real
        // subtitle) must be left alone — only the title-then-junk-then-year
        // shape is truncated.
        let entry = classify("movies/Whatever - Action Movie.mkv").unwrap();
        assert_eq!(entry.title, "Whatever - Action Movie");
        assert_eq!(entry.year, None);
    }
}
