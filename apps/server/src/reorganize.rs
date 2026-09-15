//! "Reorganize this media folder" planner for the AI tab (issue #235).
//!
//! Every video file `swarm_media::classify` already parses gets a canonical
//! on-disk path computed with **no AI call at all** — the scanner already
//! understands these files (however messy the scene-release filename), this
//! just gives the folder layout the same clean, consistent shape for a
//! human reading it in Finder/Explorer. AI is only asked to guess a title
//! for the long tail `classify` can't parse (see `guess_with_ai`), and even
//! then the guess only ever improves one more *proposed* item in the plan a
//! person must approve — nothing here touches disk until `apply_plan` runs,
//! and `apply_plan` never deletes anything: a blocked move (destination
//! already exists, cross-device rename) is skipped and reported, never
//! forced past.
//!
//! Movies, TV episodes, and music tracks (issue #300) are all covered.
//! Tracks never go through AI or TMDb — `swarm_media::classify` already
//! recognizes and flattens the intermediate "category" grouping folders
//! (`Album`, `Compilation`, …) some libraries insert between artist and
//! album, and the rare loose track with no album folder at all falls back
//! to the file's own embedded tags (the same `swarm_media::tags::read_tags`
//! the scanner's cataloging path already uses) rather than a guess; a track
//! with no album either way is left out of the plan.

use crate::ai::AiClient;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use swarm_core::peer::MediaKind;
use swarm_media::classify::{self, Classified};
use swarm_media::plex::{self, PlexValidationIssue};
use swarm_media::scrape::tmdb::TmdbClient;
use swarm_media::subtitles::{parse_subtitle_name, subtitle_extension};

/// Cap on how many AI calls one scan will make, so a folder full of
/// ambiguous names can't turn into an unbounded (and unboundedly expensive)
/// run. Files beyond the cap still use the deterministic classifier and are
/// never silently omitted from the plan.
const MAX_AI_GUESSES: usize = 25;

/// Common artwork extensions considered for orphan detection (issue #298),
/// alongside `swarm_media::subtitles::subtitle_extension`. Deliberately a
/// small, well-known set rather than every image format — this only needs
/// to catch the loose poster/thumbnail leftovers a past rename left behind,
/// not classify every image file in a library.
const ORPHAN_ARTWORK_EXTS: &[&str] = &["jpg", "jpeg", "png"];

/// The top-level holding folder orphaned sidecars are proposed into (issue
/// #298) — never deleted, just moved out of the way so nothing else can
/// collide with it (see `scan_root`'s orphan pass doc comment).
const ORPHANED_FOLDER: &str = "_orphaned";

/// The top-level holding folder confirmed content duplicates are proposed
/// into (issue #299) — mirrors `_orphaned/`: never deleted, and the original
/// relative path is preserved underneath so it can never collide with
/// anything else already there. See `resolve_destination`.
const DUPLICATES_FOLDER: &str = "_duplicates";

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReorgItem {
    /// Path relative to the scanned root, forward-slashed.
    pub from: String,
    pub to: String,
    /// `"video"`, `"track"` (issue #300: a music file proposed for a move
    /// to its canonical `Artist/Album/Track.ext` path), `"subtitle"` (a
    /// sidecar riding along with its video), `"orphan"` (issue #298: a
    /// subtitle/artwork leftover with no matching video anywhere in the
    /// root, proposed for a move into `_orphaned/` rather than a rename),
    /// or `"duplicate"` (issue #299: the file already at the proposed
    /// destination is byte-identical — this item is proposed for a move
    /// into `_duplicates/` instead, and the already-canonical file at the
    /// original destination is left alone).
    pub kind: &'static str,
    pub ai_assisted: bool,
    /// `Some("tmdb")` when `classify` found a movie title but no year in the
    /// filename and a confident TMDb lookup (see
    /// `TmdbClient::confident_movie_year`) filled it in for this canonical
    /// path — distinct from `ai_assisted`, which is about identifying an
    /// otherwise unclassifiable file rather than enriching one `classify`
    /// already placed.
    pub year_source: Option<&'static str>,
    /// `Some(reason)` when this item must not be applied (e.g. the
    /// destination already exists) — carried in the plan so the UI can show
    /// *why* an item is excluded rather than silently dropping it.
    pub conflict: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReorgPlan {
    pub root_label: String,
    pub items: Vec<ReorgItem>,
    pub ai_assisted_count: u32,
    /// Movie items whose `(Year)` came from a TMDb lookup rather than the
    /// filename itself (see `ReorgItem::year_source`).
    pub tmdb_year_count: u32,
    pub conflict_count: u32,
    /// Count of `kind == "orphan"` items — subtitle/artwork leftovers with
    /// no matching video anywhere in the root (issue #298).
    pub orphan_count: u32,
    /// Count of `kind == "duplicate"` items — confirmed byte-identical
    /// duplicates of a file already at its canonical destination, proposed
    /// for a move into `_duplicates/` rather than left as a dead-end
    /// conflict (issue #299).
    pub duplicate_count: u32,
    /// Every deterministic Plex-conformance problem found in the root
    /// (issue #247) — the complete list, unlike the bounded best-effort
    /// subset a library scan reports. Each carries the current path, the
    /// problem, the expected Plex-compatible structure, and a recommended
    /// fix, so the AI helper can offer automatic repair.
    pub validation: Vec<PlexValidationIssue>,
}

/// Walks `root` (a configured media root's real path) and proposes a
/// rename/move for every video whose canonical path differs from its
/// current one. `ai`, when given, is used only for files `classify` cannot
/// place at all — see the module doc comment. `tmdb`, when given, is used
/// only to backfill a missing movie year (issue #297) — never to identify a
/// file `classify` and `ai` both failed to place.
pub async fn scan_root(
    root_label: &str,
    root: &Path,
    ai: Option<&AiClient>,
    tmdb: Option<&TmdbClient>,
) -> std::io::Result<ReorgPlan> {
    let mut video_files = Vec::new();
    walk(root, root, &mut video_files)?;
    video_files.sort();

    let mut items = Vec::new();
    let mut ai_assisted_count = 0u32;
    let mut tmdb_year_count = 0u32;
    let mut ai_budget = MAX_AI_GUESSES;
    let mut planned_targets: HashSet<String> = HashSet::new();

    for relative in &video_files {
        let unix_relative = to_unix(relative);
        let Some((ext, is_audio)) = classify::media_extension(&unix_relative) else {
            continue;
        };

        if is_audio {
            if let Some(item) = plan_track(root, &unix_relative, ext, &mut planned_targets).await {
                items.push(item);
            }
            continue;
        }

        // `classify` deliberately has a best-effort movie fallback for every
        // recognized video extension. Keep that result even when it lacks a
        // year: dropping it here was why large flat libraries left most of
        // their files untouched once the bounded AI budget was exhausted.
        let Some(deterministic) = classify::classify(&unix_relative) else {
            continue;
        };
        let (classified, ai_assisted) = if !is_confident(&deterministic) && ai_budget > 0 {
            if let Some(client) = ai {
                ai_budget -= 1;
                match guess_with_ai(client, &unix_relative).await {
                    Some(guess) => (guess, true),
                    None => (deterministic, false),
                }
            } else {
                (deterministic, false)
            }
        } else {
            (deterministic, false)
        };

        let (classified, year_source) = fill_missing_movie_year(classified, tmdb).await;
        if year_source.is_some() {
            tmdb_year_count += 1;
        }

        let canonical = canonical_video_path(&classified, ext);
        if canonical == unix_relative {
            continue;
        }
        if ai_assisted {
            ai_assisted_count += 1;
        }

        let (to, kind, conflict) =
            resolve_destination(root, &unix_relative, &canonical, "video", &mut planned_targets).await;
        items.push(ReorgItem {
            from: unix_relative.clone(),
            to,
            kind,
            ai_assisted,
            year_source,
            conflict,
        });

        for (sub_from, sub_to) in find_sidecar_moves(root, &unix_relative, &canonical) {
            let (to, kind, conflict) =
                resolve_destination(root, &sub_from, &sub_to, "subtitle", &mut planned_targets).await;
            items.push(ReorgItem {
                from: sub_from,
                to,
                kind,
                ai_assisted,
                year_source,
                conflict,
            });
        }
    }

    for (orphan_from, orphan_to) in find_orphans(&video_files, &items) {
        let (to, kind, conflict) =
            resolve_destination(root, &orphan_from, &orphan_to, "orphan", &mut planned_targets).await;
        items.push(ReorgItem {
            from: orphan_from,
            to,
            kind,
            ai_assisted: false,
            year_source: None,
            conflict,
        });
    }

    // Deterministic Plex-conformance validation over every media file in
    // the root — movies, episodes, and tracks alike, not just the videos
    // considered for a move above.
    let mut validation = Vec::new();
    for relative in &video_files {
        let unix_relative = to_unix(relative);
        if classify::media_extension(&unix_relative).is_none() {
            continue;
        }
        let classified = classify::classify(&unix_relative);
        if let Some(issue) = plex::validate_media_file(&unix_relative, classified.as_ref()) {
            validation.push(issue);
        }
    }

    let conflict_count = items.iter().filter(|i| i.conflict.is_some()).count() as u32;
    let orphan_count = items.iter().filter(|i| i.kind == "orphan").count() as u32;
    let duplicate_count = items.iter().filter(|i| i.kind == "duplicate").count() as u32;
    Ok(ReorgPlan {
        root_label: root_label.to_string(),
        items,
        ai_assisted_count,
        tmdb_year_count,
        conflict_count,
        orphan_count,
        duplicate_count,
        validation,
    })
}

/// Second pass over the walked file list (issue #298): every subtitle or
/// common-artwork-extension file not already carried along as a sidecar by
/// `find_sidecar_moves` above, whose base stem doesn't match *any* video
/// anywhere in the root — not just the ones a rename was proposed for —
/// gets proposed for a move into the `_orphaned/` holding folder, keeping
/// its original relative path underneath so it can never collide with
/// anything else already there. This is the on-disk shape of the leftover
/// `.vtt`/`.jpg` files a past manual rename abandoned in place: the video
/// they belonged to has since moved to its canonical folder under a
/// different name, so nothing today points at them and `classify` never
/// sees them as media to validate in the first place.
///
/// Matches on stem the same way `find_sidecar_moves` does: a subtitle's
/// base stem is taken after `parse_subtitle_name` peels any trailing
/// language/modifier token, exactly like matching a sidecar to its video at
/// scan time; an artwork file's base stem is compared as-is, since none of
/// this codebase's own artwork-writing conventions (see
/// `swarm_media::scan::recovered_artwork_kind`) name a file after a movie's
/// own stem directly — those already live under an `images/` sibling
/// folder, which is skipped entirely here so real scraped/manual artwork is
/// never mistaken for an orphan.
fn find_orphans(all_files: &[PathBuf], items: &[ReorgItem]) -> Vec<(String, String)> {
    let video_stems: HashSet<String> = all_files
        .iter()
        .filter_map(|relative| {
            let unix_relative = to_unix(relative);
            let (_, is_audio) = classify::media_extension(&unix_relative)?;
            if is_audio {
                return None;
            }
            Path::new(&unix_relative)
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
        })
        .collect();

    let already_accounted: HashSet<&str> = items.iter().map(|i| i.from.as_str()).collect();

    let mut orphans = Vec::new();
    for relative in all_files {
        let unix_relative = to_unix(relative);
        if classify::media_extension(&unix_relative).is_some() {
            continue; // a video or audio file, not an orphan candidate
        }
        if in_images_dir(&unix_relative) || is_already_orphaned(&unix_relative) {
            continue;
        }
        if already_accounted.contains(unix_relative.as_str()) {
            continue;
        }
        let is_subtitle = subtitle_extension(&unix_relative).is_some();
        let is_artwork = !is_subtitle
            && unix_relative
                .rsplit('.')
                .next()
                .is_some_and(|ext| ORPHAN_ARTWORK_EXTS.contains(&ext.to_lowercase().as_str()));
        if !is_subtitle && !is_artwork {
            continue;
        }

        let Some(stem) = Path::new(&unix_relative).file_stem().map(|s| s.to_string_lossy().to_string()) else {
            continue;
        };
        let base_stem = if is_subtitle {
            parse_subtitle_name(&stem).base_stem.to_lowercase()
        } else {
            stem.to_lowercase()
        };
        if video_stems.contains(&base_stem) {
            continue; // still matches a video somewhere in the root
        }

        orphans.push((unix_relative.clone(), format!("{ORPHANED_FOLDER}/{unix_relative}")));
    }
    orphans
}

fn in_images_dir(relative: &str) -> bool {
    Path::new(relative)
        .components()
        .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("images"))
}

fn is_already_orphaned(relative: &str) -> bool {
    relative == ORPHANED_FOLDER || relative.starts_with(&format!("{ORPHANED_FOLDER}/"))
}

/// Backfills a movie's missing release year from TMDb before the canonical
/// path is computed (issue #297). Only ever touches a `Classified` that
/// already has a non-empty movie title and no year — TV episodes and
/// already-yeared movies pass through untouched. A `None` result from the
/// lookup (no API key configured, no match, or an ambiguous/low-confidence
/// match — see `TmdbClient::confident_movie_year`) leaves the file exactly
/// as `classify`/AI left it, so it falls back to today's bare-title
/// behavior rather than ever guessing a year.
async fn fill_missing_movie_year(
    classified: Classified,
    tmdb: Option<&TmdbClient>,
) -> (Classified, Option<&'static str>) {
    if classified.kind != MediaKind::Movie || classified.year.is_some() || classified.title.trim().is_empty() {
        return (classified, None);
    }
    let Some(client) = tmdb else {
        return (classified, None);
    };
    match client.confident_movie_year(&classified.title).await {
        Ok(Some(year)) => (
            Classified {
                year: Some(year),
                ..classified
            },
            Some("tmdb"),
        ),
        _ => (classified, None),
    }
}

/// Proposes a canonical `Artist/Album/NN - Track.ext` move for one audio
/// file (issue #300). No AI and no TMDb here: `classify` already resolves
/// artist/album from the folder chain, seeing straight through the
/// intermediate "category" grouping folders (`Album`, `Compilation`, …)
/// some libraries insert one level below the artist — the only gap left is
/// a track sitting loose directly under the artist folder with no album
/// folder at all, which `fill_missing_track_album` closes from the file's
/// own embedded tags. Returns `None` when the file is already at its
/// canonical path, or when no album can be determined either way — this
/// case is genuinely ambiguous, so the file is left out of the plan rather
/// than the album being guessed at.
async fn plan_track(
    root: &Path,
    relative: &str,
    ext: &'static str,
    planned_targets: &mut HashSet<String>,
) -> Option<ReorgItem> {
    let classified = classify::classify(relative)?;
    let classified = fill_missing_track_album(root, relative, classified).await;
    let canonical = canonical_track_path(&classified, ext)?;
    if canonical == relative {
        return None;
    }
    let (to, kind, conflict) = resolve_destination(root, relative, &canonical, "track", planned_targets).await;
    Some(ReorgItem {
        from: relative.to_string(),
        to,
        kind,
        ai_assisted: false,
        year_source: None,
        conflict,
    })
}

/// Fills in a loose track's missing album from its own embedded tags (issue
/// #300) — the same `swarm_media::tags::read_tags` the scanner's cataloging
/// path (`swarm_media::scan`) already reads for the exact same purpose, run
/// in `spawn_blocking` for the same reason every other tag/fingerprint read
/// in this codebase is: synchronous std::fs I/O that can be a slow SMB/NFS
/// round trip sharing a worker thread with request handling. Only ever
/// touches a `Classified` that has no album at all; a track that already
/// has one (from an `Artist/Album/...` folder chain) passes through
/// untouched. No usable tag, or no tags at all, leaves the album unset —
/// `canonical_track_path` then refuses to propose a move for it.
async fn fill_missing_track_album(root: &Path, relative: &str, classified: Classified) -> Classified {
    if classified.kind != MediaKind::Track || classified.album.as_deref().is_some_and(|a| !a.trim().is_empty()) {
        return classified;
    }
    let path = root.join(relative);
    let tag = tokio::task::spawn_blocking(move || swarm_media::tags::read_tags(&path))
        .await
        .unwrap_or(None);
    match tag.and_then(|t| t.album) {
        Some(album) if !album.trim().is_empty() => Classified {
            album: Some(album),
            ..classified
        },
        _ => classified,
    }
}

/// The canonical on-disk shape for a track — `Artist/Album/NN - Track.ext`,
/// matching the naming `crate::plex::validate_media_file` already expects
/// for `MediaKind::Track` and the `Artist/Album/Track` convention
/// `docs/PLEX_COMPATIBILITY_AUDIT.md` checks against. `None` when either
/// artist or album is still missing after `fill_missing_track_album` — see
/// `plan_track`.
fn canonical_track_path(c: &Classified, ext: &str) -> Option<String> {
    let artist = c.artist.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    let album = c.album.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    let title = sanitize(&c.title);
    let file_name = match c.track_number {
        Some(n) => format!("{n:02} - {title}.{ext}"),
        None => format!("{title}.{ext}"),
    };
    Some(format!("{}/{}/{file_name}", sanitize(artist), sanitize(album)))
}

/// Resolves what a proposed `source -> target` move should actually become
/// once what's already on disk (and what this plan has already claimed) is
/// accounted for. Returns the item's final `to`, `kind`, and `conflict` —
/// `kind` stays `default_kind` unless the destination turns out to be a
/// confirmed content duplicate (issue #299), in which case it becomes
/// `"duplicate"` and `to` is redirected into `DUPLICATES_FOLDER`.
async fn resolve_destination(
    root: &Path,
    source: &str,
    target: &str,
    default_kind: &'static str,
    planned_targets: &mut HashSet<String>,
) -> (String, &'static str, Option<String>) {
    let dest_path = root.join(target);
    if dest_path.exists() {
        if files_are_identical(root.join(source), dest_path).await {
            return duplicate_destination(source, planned_targets);
        }
        return (
            target.to_string(),
            default_kind,
            Some("a file already exists at the destination".to_string()),
        );
    }
    if !planned_targets.insert(target.to_string()) {
        return (
            target.to_string(),
            default_kind,
            Some("likely a duplicate — another item in this plan already targets this path".to_string()),
        );
    }
    (target.to_string(), default_kind, None)
}

/// Redirects a confirmed duplicate's source into `_duplicates/<original
/// relative path>` (issue #299), preserving its original scene-release name
/// so it stays traceable back to `_cleanup_leftovers/`-style source
/// folders. Still registered in `planned_targets` so two distinct sources
/// can never collide on the same `_duplicates/` path.
fn duplicate_destination(source: &str, planned_targets: &mut HashSet<String>) -> (String, &'static str, Option<String>) {
    let target = format!("{DUPLICATES_FOLDER}/{source}");
    let conflict = if !planned_targets.insert(target.clone()) {
        Some("likely a duplicate — another item in this plan already targets this path".to_string())
    } else {
        None
    };
    (target, "duplicate", conflict)
}

/// Compares two files by content fingerprint (issue #299) — the same
/// `swarm_core::fingerprint` tool already used at scale to find and safely
/// delete duplicate `library_entries` rows in this exact library: content-
/// based, path-independent, negligible collision probability. Run in
/// `spawn_blocking` like every other fingerprint read in this codebase (see
/// `swarm_media::scan`): this is synchronous std::fs I/O that can be a slow
/// SMB/NFS round trip and shares a worker thread with request handling. Any
/// I/O error (permissions, a file that vanished mid-scan) is treated as
/// "not identical" — falls back to today's plain-conflict behavior rather
/// than ever guessing.
async fn files_are_identical(a: PathBuf, b: PathBuf) -> bool {
    tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
        if std::fs::metadata(&a)?.len() != std::fs::metadata(&b)?.len() {
            return Ok(false);
        }
        let fp_a = swarm_core::fingerprint::fingerprint_file(&a)?;
        let fp_b = swarm_core::fingerprint::fingerprint_file(&b)?;
        Ok(fp_a == fp_b)
    })
    .await
    .unwrap_or(Ok(false))
    .unwrap_or(false)
}

fn is_confident(c: &Classified) -> bool {
    match c.kind {
        MediaKind::Movie => !c.title.trim().is_empty() && c.year.is_some(),
        MediaKind::Episode => {
            c.show_title.as_deref().is_some_and(|s| !s.trim().is_empty())
                && c.season.is_some()
                && c.episode.is_some()
        }
        MediaKind::Track => false,
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect::<String>()
        .trim()
        .to_string()
}

fn canonical_video_path(c: &Classified, ext: &str) -> String {
    match c.kind {
        MediaKind::Movie => {
            let title = sanitize(&c.title);
            let name = match c.year {
                Some(year) => format!("{title} ({year})"),
                None => title,
            };
            format!("{name}/{name}.{ext}")
        }
        MediaKind::Episode => {
            let show = sanitize(c.show_title.as_deref().unwrap_or("Unknown Show"));
            let season = c.season.unwrap_or(1);
            let episode = c.episode.unwrap_or(0);
            format!("{show}/Season {season:02}/{show} - S{season:02}E{episode:02}.{ext}")
        }
        MediaKind::Track => String::new(),
    }
}

#[derive(serde::Deserialize)]
struct AiClassifyGuess {
    kind: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    show_title: Option<String>,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    season: Option<u32>,
    #[serde(default)]
    episode: Option<u32>,
}

async fn guess_with_ai(client: &AiClient, relative_path: &str) -> Option<Classified> {
    let system = "You help organize a personal movie/TV library from messy filenames. \
        Reply with ONLY a JSON object, no prose, no code fences.";
    let user = format!(
        "File path: \"{relative_path}\"\n\n\
        Reply with a JSON object exactly like \
        {{\"kind\": \"movie\", \"title\": \"<canonical movie title>\", \"year\": <release year or null>}} \
        or {{\"kind\": \"episode\", \"show_title\": \"<canonical show title>\", \"season\": <season number>, \
        \"episode\": <episode number>, \"year\": <show's release year or null>}}. \
        Make a best-effort identification for every supplied video; do not omit a file merely because its year is missing. \
        Only reply {{\"kind\": null}} when neither a movie title nor a TV show/season/episode can reasonably be derived."
    );
    let reply = client.complete(system, &user).await.ok()?;
    let guess: AiClassifyGuess = crate::ai::parse_json_object(&reply)?;
    match guess.kind.as_deref() {
        Some("movie") if guess.title.as_deref().is_some_and(|t| !t.trim().is_empty()) => Some(Classified {
            kind: MediaKind::Movie,
            title: guess.title.unwrap_or_default(),
            artist: None,
            album: None,
            track_number: None,
            show_title: None,
            season: None,
            episode: None,
            year: guess.year,
            episode_end: None,
            plex_guid: None,
            edition: None,
            extra_kind: None,
            extra_title: None,
            extra_parent_title: None,
            extra_parent_dir: None,
            extra_relative_path: None,
            extra_category_path: None,
        }),
        Some("episode")
            if guess.show_title.as_deref().is_some_and(|s| !s.trim().is_empty())
                && guess.season.is_some()
                && guess.episode.is_some() =>
        {
            Some(Classified {
                kind: MediaKind::Episode,
                title: guess.show_title.clone().unwrap_or_default(),
                artist: None,
                album: None,
                track_number: None,
                show_title: guess.show_title,
                season: guess.season,
                episode: guess.episode,
                year: guess.year,
                episode_end: None,
                plex_guid: None,
                edition: None,
                extra_kind: None,
                extra_title: None,
                extra_parent_title: None,
                extra_parent_dir: None,
                extra_relative_path: None,
                extra_category_path: None,
            })
        }
        _ => None,
    }
}

/// Finds subtitle sidecars (same directory, or a `Subs`/`Subtitles`
/// subfolder of it) whose base stem — after peeling any trailing
/// language/modifier token, same rule `swarm_media::subtitles` uses to
/// match sidecars to videos at scan time — matches the video's own stem.
/// Every match found is proposed to move alongside the video, keeping its
/// language suffix, so it keeps matching after the move.
fn find_sidecar_moves(root: &Path, video_relative: &str, video_target: &str) -> Vec<(String, String)> {
    let mut moves = Vec::new();
    let video_path = Path::new(video_relative);
    let dir = video_path.parent().unwrap_or_else(|| Path::new(""));
    let Some(video_stem) = video_path.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else {
        return moves;
    };
    let target_path = Path::new(video_target);
    let target_dir = target_path.parent().unwrap_or_else(|| Path::new(""));
    let Some(target_stem) = target_path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
        return moves;
    };

    for candidate_dir in [dir.to_path_buf(), dir.join("Subs"), dir.join("Subtitles")] {
        let abs_dir = root.join(&candidate_dir);
        let Ok(read_dir) = std::fs::read_dir(&abs_dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(sub_ext) = subtitle_extension(file_name) else {
                continue;
            };
            let stem = file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(file_name);
            let parsed = parse_subtitle_name(stem);
            if parsed.base_stem.to_lowercase() != video_stem {
                continue;
            }
            let sub_relative = to_unix(&candidate_dir.join(file_name));
            let lang_suffix = parsed.language.as_deref().map(|l| format!(".{l}")).unwrap_or_default();
            let sub_target = to_unix(&target_dir.join(format!("{target_stem}{lang_suffix}.{sub_ext}")));
            moves.push((sub_relative, sub_target));
        }
    }
    moves
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            walk(root, &path, out)?;
        } else if file_type.is_file() {
            if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.to_path_buf());
            }
        }
    }
    Ok(())
}

fn to_unix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[derive(Debug, Default)]
pub struct ApplyOutcome {
    pub applied: u32,
    pub skipped: u32,
    pub errors: Vec<String>,
}

/// Applies every non-conflicting item by `fs::rename` — never a copy+delete
/// fallback, so a failed or partial move can never cost the source file.
/// Re-checks existence right before each move (the plan may be stale by the
/// time a user approves it) rather than trusting the scan-time snapshot.
pub fn apply_plan(root: &Path, items: &[ReorgItem]) -> ApplyOutcome {
    let mut outcome = ApplyOutcome::default();
    for item in items {
        if let Some(reason) = &item.conflict {
            outcome.skipped += 1;
            outcome.errors.push(format!("{}: skipped ({reason})", item.from));
            continue;
        }
        let from = root.join(&item.from);
        let to = root.join(&item.to);
        if !from.exists() {
            outcome.skipped += 1;
            outcome.errors.push(format!("{}: source no longer exists, skipped", item.from));
            continue;
        }
        if to.exists() {
            outcome.skipped += 1;
            outcome.errors.push(format!("{}: destination now exists, skipped", item.to));
            continue;
        }
        if let Some(parent) = to.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                outcome.skipped += 1;
                outcome
                    .errors
                    .push(format!("{}: could not create destination folder ({error}), skipped", item.to));
                continue;
            }
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => outcome.applied += 1,
            Err(error) => {
                outcome.skipped += 1;
                outcome.errors.push(format!("{}: move failed ({error}), left in place", item.from));
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Writes the smallest valid FLAC lofty will parse — a `STREAMINFO`
    /// block (mandatory, fixed 34 bytes) followed by a `VORBIS_COMMENT`
    /// block carrying `artist`/`album` — with no audio frames at all, so
    /// tests can exercise `swarm_media::tags::read_tags` without a real
    /// audio encoder. See the FLAC format spec's metadata block layout
    /// (`https://xiph.org/flac/format.html`).
    fn write_flac_with_tags(root: &Path, relative: &str, artist: &str, album: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let mut data = Vec::new();
        data.extend_from_slice(b"fLaC");

        let mut stream_info = Vec::new();
        stream_info.extend_from_slice(&4096u16.to_be_bytes()); // min block size
        stream_info.extend_from_slice(&4096u16.to_be_bytes()); // max block size
        stream_info.extend_from_slice(&[0, 0, 0]); // min frame size
        stream_info.extend_from_slice(&[0, 0, 0]); // max frame size
        let sample_rate: u32 = 44100;
        let channels_minus_one: u32 = 1;
        let bits_per_sample_minus_one: u32 = 15;
        let info = (sample_rate << 12) | (channels_minus_one << 9) | (bits_per_sample_minus_one << 4);
        stream_info.extend_from_slice(&info.to_be_bytes()); // sample rate/channels/bits/high total-samples bits
        stream_info.extend_from_slice(&0u32.to_be_bytes()); // remaining total-samples bits
        stream_info.extend_from_slice(&[0u8; 16]); // MD5 signature
        assert_eq!(stream_info.len(), 34);
        data.push(0x00); // block type 0 (STREAMINFO), not the last metadata block
        data.extend_from_slice(&(stream_info.len() as u32).to_be_bytes()[1..]);
        data.extend_from_slice(&stream_info);

        let vendor = b"swarm-test";
        let fields = [format!("ARTIST={artist}"), format!("ALBUM={album}")];
        let mut comments = Vec::new();
        comments.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        comments.extend_from_slice(vendor);
        comments.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        for field in &fields {
            let bytes = field.as_bytes();
            comments.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            comments.extend_from_slice(bytes);
        }
        data.push(0x84); // block type 4 (VORBIS_COMMENT), last metadata block
        data.extend_from_slice(&(comments.len() as u32).to_be_bytes()[1..]);
        data.extend_from_slice(&comments);

        fs::write(path, data).unwrap();
    }

    #[tokio::test]
    async fn proposes_a_canonical_movie_folder_for_a_scene_release_name() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "10.Cloverfield.Lane.2016.1080p.BluRay.x264-GROUP.mkv", "x");
        let plan = scan_root("local", dir.path(), None, None).await.unwrap();
        assert_eq!(plan.items.len(), 1);
        let item = &plan.items[0];
        assert_eq!(item.to, "10 Cloverfield Lane (2016)/10 Cloverfield Lane (2016).mkv");
        assert!(item.conflict.is_none());
        assert!(!item.ai_assisted);
    }

    // --- TMDb year backfill (issue #297) ---

    async fn spawn_mock_tmdb(router: axum::Router) -> TmdbClient {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let base = format!("http://{addr}");
        TmdbClient::with_base_urls("key", &base, &base)
    }

    #[tokio::test]
    async fn a_confident_tmdb_match_fills_in_a_missing_movie_year() {
        use axum::routing::get;
        use axum::Json;
        use serde_json::json;

        let router = axum::Router::new().route(
            "/search/movie",
            get(|| async {
                Json(json!({"results": [
                    {"id": 348, "title": "Alien", "release_date": "1979-05-25", "popularity": 40.0, "vote_count": 12000}
                ]}))
            }),
        );
        let tmdb = spawn_mock_tmdb(router).await;

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Alien.mkv", "x");
        let plan = scan_root("local", dir.path(), None, Some(&tmdb)).await.unwrap();

        assert_eq!(plan.items.len(), 1);
        let item = &plan.items[0];
        assert_eq!(item.to, "Alien (1979)/Alien (1979).mkv");
        assert_eq!(item.year_source, Some("tmdb"));
        assert_eq!(plan.tmdb_year_count, 1);
    }

    #[tokio::test]
    async fn an_ambiguous_tmdb_match_leaves_the_year_unfilled() {
        use axum::routing::get;
        use axum::Json;
        use serde_json::json;

        let router = axum::Router::new().route(
            "/search/movie",
            get(|| async {
                Json(json!({"results": [
                    {"id": 1, "title": "Scream", "release_date": "1996-12-20", "popularity": 30.0, "vote_count": 8000},
                    {"id": 2, "title": "Scream", "release_date": "2022-01-14", "popularity": 25.0, "vote_count": 4000}
                ]}))
            }),
        );
        let tmdb = spawn_mock_tmdb(router).await;

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Scream.mkv", "x");
        let plan = scan_root("local", dir.path(), None, Some(&tmdb)).await.unwrap();

        assert_eq!(plan.items.len(), 1);
        let item = &plan.items[0];
        assert_eq!(item.to, "Scream/Scream.mkv");
        assert_eq!(item.year_source, None);
        assert_eq!(plan.tmdb_year_count, 0);
    }

    #[tokio::test]
    async fn brings_a_matching_subtitle_sidecar_along() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mkv", "x");
        write(dir.path(), "Heat.1995.en.srt", "x");
        write(dir.path(), "Heat.1995.es.vtt", "x");
        let plan = scan_root("local", dir.path(), None, None).await.unwrap();
        let subtitles: Vec<_> = plan.items.iter().filter(|i| i.kind == "subtitle").collect();
        assert_eq!(subtitles.len(), 2);
        assert!(subtitles.iter().any(|item| item.to == "Heat (1995)/Heat (1995).en.srt"));
        assert!(subtitles.iter().any(|item| item.to == "Heat (1995)/Heat (1995).es.vtt"));
        assert!(subtitles.iter().all(|item| item.conflict.is_none()));
        assert!(plan.items.iter().all(|item| item.kind != "orphan"));
        assert_eq!(plan.orphan_count, 0);
    }

    // --- Orphaned sidecar detection (issue #298) ---

    #[tokio::test]
    async fn flags_a_leftover_subtitle_and_artwork_file_with_no_matching_video_as_orphaned() {
        let dir = tempfile::tempdir().unwrap();
        // The movie has already been reorganized into its canonical folder...
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "x");
        // ...but a subtitle and poster left over from the old
        // "Heat.1995.1080p" naming scheme are still loose at the root,
        // pointing at nothing.
        write(dir.path(), "Heat.1995.1080p.vtt", "x");
        write(dir.path(), "Heat.1995.1080p.jpg", "x");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        let orphans: Vec<_> = plan.items.iter().filter(|i| i.kind == "orphan").collect();
        assert_eq!(orphans.len(), 2);
        assert!(orphans.iter().any(|i| i.from == "Heat.1995.1080p.vtt" && i.to == "_orphaned/Heat.1995.1080p.vtt"));
        assert!(orphans.iter().any(|i| i.from == "Heat.1995.1080p.jpg" && i.to == "_orphaned/Heat.1995.1080p.jpg"));
        assert!(orphans.iter().all(|i| i.conflict.is_none()));
        assert_eq!(plan.orphan_count, 2);
    }

    #[tokio::test]
    async fn does_not_flag_a_sidecar_that_still_matches_a_video_as_orphaned() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mkv", "x");
        write(dir.path(), "Heat.1995.en.srt", "x");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        assert!(plan.items.iter().any(|i| i.kind == "subtitle" && i.from == "Heat.1995.en.srt"));
        assert!(plan.items.iter().all(|i| i.kind != "orphan"));
        assert_eq!(plan.orphan_count, 0);
    }

    #[tokio::test]
    async fn does_not_treat_scraped_artwork_under_an_images_folder_as_orphaned() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "x");
        write(dir.path(), "Heat (1995)/images/heat-1995-tmdb-poster.jpg", "x");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        assert!(plan.items.iter().all(|i| i.kind != "orphan"));
        assert_eq!(plan.orphan_count, 0);
    }

    #[tokio::test]
    async fn flags_a_conflict_when_the_destination_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mkv", "x");
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "already here");
        let plan = scan_root("local", dir.path(), None, None).await.unwrap();
        let video = plan.items.iter().find(|i| i.kind == "video").expect("video item");
        assert!(video.conflict.is_some());
        assert_eq!(video.kind, "video");
        assert_eq!(plan.conflict_count, 1);
        assert_eq!(plan.duplicate_count, 0);
    }

    // --- True duplicate detection by content fingerprint (issue #299) ---

    #[tokio::test]
    async fn a_byte_identical_conflict_is_proposed_as_a_duplicate_move_and_never_touches_the_canonical_file() {
        let dir = tempfile::tempdir().unwrap();
        // Already organized correctly...
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "same bytes");
        // ...and a scene-release-named leftover with byte-identical content,
        // the shape of `Movies/_cleanup_leftovers/` from the Plex
        // compatibility audit.
        write(dir.path(), "Heat.1995.BDRip.x264-GROUP.mkv", "same bytes");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        let duplicate = plan.items.iter().find(|i| i.kind == "duplicate").expect("duplicate item");
        assert_eq!(duplicate.from, "Heat.1995.BDRip.x264-GROUP.mkv");
        assert_eq!(duplicate.to, "_duplicates/Heat.1995.BDRip.x264-GROUP.mkv");
        assert!(duplicate.conflict.is_none());
        assert_eq!(plan.duplicate_count, 1);
        assert_eq!(plan.conflict_count, 0);
        assert_eq!(
            fs::read_to_string(dir.path().join("Heat (1995)/Heat (1995).mkv")).unwrap(),
            "same bytes"
        );
    }

    #[tokio::test]
    async fn a_conflict_with_different_content_is_left_as_a_plain_conflict_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "the real thing");
        write(dir.path(), "Heat.1995.BDRip.x264-GROUP.mkv", "an unrelated remux");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        let video = plan.items.iter().find(|i| i.kind == "video").expect("video item");
        assert_eq!(video.from, "Heat.1995.BDRip.x264-GROUP.mkv");
        assert_eq!(
            video.conflict.as_deref(),
            Some("a file already exists at the destination")
        );
        assert!(plan.items.iter().all(|i| i.kind != "duplicate"));
        assert_eq!(plan.duplicate_count, 0);
        assert_eq!(plan.conflict_count, 1);
    }

    #[tokio::test]
    async fn leaves_an_already_canonical_file_out_of_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "x");
        let plan = scan_root("local", dir.path(), None, None).await.unwrap();
        assert!(plan.items.is_empty());
    }

    #[tokio::test]
    async fn proposes_every_video_even_when_metadata_is_incomplete_and_ai_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "asdf1234.mkv", "x");
        let plan = scan_root("local", dir.path(), None, None).await.unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].to, "asdf1234/asdf1234.mkv");
        assert_eq!(plan.ai_assisted_count, 0);
    }

    #[tokio::test]
    async fn identifies_two_sources_with_the_same_target_as_likely_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mp4", "x");
        write(dir.path(), "Heat (1995).mp4", "x");
        let plan = scan_root("local", dir.path(), None, None).await.unwrap();
        let duplicate = plan
            .items
            .iter()
            .find(|item| item.conflict.as_deref().is_some_and(|reason| reason.contains("another item")))
            .expect("one move should be marked as a duplicate");
        assert!(duplicate.conflict.as_deref().unwrap().contains("likely a duplicate"));
    }

    #[test]
    fn apply_plan_renames_files_and_never_touches_a_conflicting_item() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mkv", "original content");
        let items = vec![
            ReorgItem {
                from: "Heat.1995.mkv".to_string(),
                to: "Heat (1995)/Heat (1995).mkv".to_string(),
                kind: "video",
                ai_assisted: false,
                year_source: None,
                conflict: None,
            },
            ReorgItem {
                from: "does-not-exist.srt".to_string(),
                to: "Heat (1995)/Heat (1995).srt".to_string(),
                kind: "subtitle",
                ai_assisted: false,
                year_source: None,
                conflict: Some("a file already exists at the destination".to_string()),
            },
        ];
        let outcome = apply_plan(dir.path(), &items);
        assert_eq!(outcome.applied, 1);
        assert_eq!(outcome.skipped, 1);
        assert!(!dir.path().join("Heat.1995.mkv").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("Heat (1995)/Heat (1995).mkv")).unwrap(),
            "original content"
        );
    }

    #[test]
    fn apply_plan_never_overwrites_a_destination_that_appeared_after_scan() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mkv", "source content");
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "unrelated existing file");
        let items = vec![ReorgItem {
            from: "Heat.1995.mkv".to_string(),
            to: "Heat (1995)/Heat (1995).mkv".to_string(),
            kind: "video",
            ai_assisted: false,
            year_source: None,
            conflict: None,
        }];
        let outcome = apply_plan(dir.path(), &items);
        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(fs::read_to_string(dir.path().join("Heat.1995.mkv")).unwrap(), "source content");
        assert_eq!(
            fs::read_to_string(dir.path().join("Heat (1995)/Heat (1995).mkv")).unwrap(),
            "unrelated existing file"
        );
    }

    // --- Music library reorganization (issue #300) ---

    #[tokio::test]
    async fn flattens_an_intermediate_grouping_folder_between_artist_and_album() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Artist/Album/2003 - Some Album/01 - Track.mp3", "x");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        let track = plan.items.iter().find(|i| i.kind == "track").expect("track item");
        assert_eq!(track.from, "Artist/Album/2003 - Some Album/01 - Track.mp3");
        assert_eq!(track.to, "Artist/2003 - Some Album/01 - Track.mp3");
        assert!(track.conflict.is_none());
        assert!(!track.ai_assisted);
    }

    #[tokio::test]
    async fn a_loose_track_with_a_readable_album_tag_moves_under_that_album() {
        let dir = tempfile::tempdir().unwrap();
        write_flac_with_tags(
            dir.path(),
            "Armin van Buuren/01 - Blank State.flac",
            "Armin van Buuren",
            "A State Of Trance",
        );

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        let track = plan.items.iter().find(|i| i.kind == "track").expect("track item");
        assert_eq!(track.from, "Armin van Buuren/01 - Blank State.flac");
        assert_eq!(track.to, "Armin van Buuren/A State Of Trance/01 - Blank State.flac");
        assert!(track.conflict.is_none());
    }

    #[tokio::test]
    async fn a_loose_track_with_no_usable_album_tag_is_left_out_of_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        // Not a real, tag-readable audio file, so `read_tags` finds nothing —
        // same as a genuinely untagged loose track.
        write(dir.path(), "Paul Oakenfold/Essential Mix 2001-03-04.mp3", "x");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        assert!(plan.items.iter().all(|i| i.from != "Paul Oakenfold/Essential Mix 2001-03-04.mp3"));
    }

    #[tokio::test]
    async fn leaves_an_already_canonical_track_out_of_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Pink Floyd/The Wall/05 - Hey You.flac", "x");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        assert!(plan.items.iter().all(|i| i.kind != "track"));
    }
}
