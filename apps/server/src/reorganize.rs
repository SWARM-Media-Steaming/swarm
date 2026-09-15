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
use futures_util::{stream, StreamExt};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use swarm_core::peer::MediaKind;
use swarm_media::classify::{self, Classified};
use swarm_media::plex::{self, PlexValidationIssue};
use swarm_media::roots::MediaRootAssetType;
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
    /// Another configured root that owns `to`. `None` means the scanned
    /// root. This keeps cross-library corrections explicit in review while
    /// allowing the same apply journal and Undo path to handle them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_root_label: Option<String>,
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

/// One currently-configured media root's label and the [`MediaKind`] it's
/// expected to hold, derived by the caller from `Settings::media_roots`'
/// `RootAssetType` (`Movies` → `Some(MediaKind::Movie)`, `Shows` →
/// `Some(MediaKind::Episode)`, `Music` → `Some(MediaKind::Track)`, `Mixed`
/// and `PhotosVideos` → `None`, since neither imposes a classifiable
/// expectation). Kept as this crate's own lightweight shape — `settings.rs`
/// lives only in the gui binary crate, not this library crate — rather than
/// importing `RootAssetType` directly (issue #301).
#[derive(Debug, Clone)]
pub struct RootExpectation {
    pub label: String,
    pub expected_kind: Option<MediaKind>,
}

/// One file whose classified kind doesn't match the asset type of the root
/// it's currently sitting under — e.g. a feature film inside a Shows root
/// (issue #301, see `docs/PLEX_COMPATIBILITY_AUDIT.md`). Kept distinct from
/// [`ReorgItem`] so detection remains read-only; [`plan_misplaced_moves`]
/// performs the explicit, reviewed conversion when exactly one configured
/// root owns the detected kind.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MisplacedItem {
    /// Path relative to the scanned root, forward-slashed.
    pub path: String,
    /// `"movie"`, `"episode"`, or `"track"` — the kind `classify` assigned.
    pub kind: &'static str,
    /// The label of the root this file is currently sitting under.
    pub current_root_label: String,
    /// The label of the one currently-configured root whose asset type
    /// actually matches this file's classified kind.
    pub correct_root_label: String,
}

/// Walks `root` and flags every media file whose classified kind doesn't
/// match `root_label`'s expected kind in `all_roots`, but only when exactly
/// one *other* currently-configured root's expected kind matches — with
/// zero or more than one candidate the correct destination is ambiguous, so
/// the file is left out of the report entirely rather than guessed at (see
/// the issue's non-negotiable constraint). A root with no expectation
/// (`Mixed`/`PhotosVideos`, or a label `all_roots` doesn't recognize) never
/// reports anything, since nothing about it is "wrong".
pub fn find_misplaced_content(
    root_label: &str,
    root: &Path,
    all_roots: &[RootExpectation],
) -> std::io::Result<Vec<MisplacedItem>> {
    find_misplaced_content_impl(root_label, root, all_roots, None)
}

/// Root-aware misplaced detection. In a Shows root, recognized extras stay
/// episodes while year-bearing feature films remain movies, eliminating the
/// old false report that every Aqua Teen featurette belonged in Movies.
pub fn find_misplaced_content_for_asset_type(
    root_label: &str,
    root: &Path,
    all_roots: &[RootExpectation],
    asset_type: MediaRootAssetType,
) -> std::io::Result<Vec<MisplacedItem>> {
    find_misplaced_content_impl(root_label, root, all_roots, Some(asset_type))
}

fn find_misplaced_content_impl(
    root_label: &str,
    root: &Path,
    all_roots: &[RootExpectation],
    asset_type: Option<MediaRootAssetType>,
) -> std::io::Result<Vec<MisplacedItem>> {
    let Some(current) = all_roots.iter().find(|r| r.label == root_label) else {
        return Ok(Vec::new());
    };
    let Some(current_expected) = current.expected_kind else {
        return Ok(Vec::new());
    };

    let mut all_files = Vec::new();
    walk(root, root, &mut all_files)?;
    let known_shows = asset_type
        .filter(|kind| *kind == MediaRootAssetType::Shows)
        .map_or_else(Vec::new, |_| known_show_roots(&all_files));
    let no_damaged_owners = HashMap::new();

    let mut misplaced = Vec::new();
    for relative in &all_files {
        let unix_relative = to_unix(relative);
        let Some((_, is_audio)) = classify::media_extension(&unix_relative) else {
            continue;
        };
        // Video extras bundled with an album are valid music-library
        // companions, not feature films merely because their extension is
        // recognized by the generic video classifier.
        if asset_type == Some(MediaRootAssetType::Music) && !is_audio {
            continue;
        }
        let classified = match asset_type {
            Some(MediaRootAssetType::Shows) => classify_for_reorganization(
                &unix_relative,
                MediaRootAssetType::Shows,
                &known_shows,
                &no_damaged_owners,
            ),
            _ => classify::classify(&unix_relative),
        };
        let Some(classified) = classified else {
            continue;
        };
        if classified.kind == current_expected {
            continue;
        }
        let mut candidates = all_roots
            .iter()
            .filter(|r| r.label != root_label && r.expected_kind == Some(classified.kind));
        let Some(correct) = candidates.next() else {
            continue; // no configured root of the right kind — nowhere to point at
        };
        if candidates.next().is_some() {
            continue; // ambiguous — more than one root of the right kind
        }
        misplaced.push(MisplacedItem {
            path: unix_relative,
            kind: media_kind_label(classified.kind),
            current_root_label: root_label.to_string(),
            correct_root_label: correct.label.clone(),
        });
    }
    Ok(misplaced)
}

/// Builds reviewed moves from one root into the single configured root that
/// owns each misplaced item. Cross-root moves use the same rename-only,
/// no-overwrite semantics as ordinary reorganization and bring subtitle
/// sidecars along with their video.
pub async fn plan_misplaced_moves(
    source_root: &Path,
    destination_root: &Path,
    destination_root_label: &str,
    misplaced: &[MisplacedItem],
) -> Vec<ReorgItem> {
    let mut items = Vec::new();
    let mut planned_targets = HashSet::new();
    for finding in misplaced
        .iter()
        .filter(|item| item.correct_root_label == destination_root_label)
    {
        let Some((ext, is_audio)) = classify::media_extension(&finding.path) else {
            continue;
        };
        if is_audio {
            continue;
        }
        let Some(classified) = classify::classify(&finding.path) else {
            continue;
        };
        let canonical = canonical_video_path(&classified, ext);
        let (to, kind, target_root_label, conflict) = resolve_cross_root_destination(
            source_root,
            destination_root,
            destination_root_label,
            &finding.path,
            &canonical,
            "video",
            &mut planned_targets,
        )
        .await;
        items.push(ReorgItem {
            from: finding.path.clone(),
            to: to.clone(),
            kind,
            destination_root_label: target_root_label.clone(),
            ai_assisted: false,
            year_source: None,
            conflict,
        });

        let sidecar_target = to;
        for (sub_from, sub_to) in find_sidecar_moves(source_root, &finding.path, &sidecar_target) {
            let (to, kind, sidecar_root_label, conflict) = if target_root_label.is_some() {
                resolve_cross_root_destination(
                    source_root,
                    destination_root,
                    destination_root_label,
                    &sub_from,
                    &sub_to,
                    "subtitle",
                    &mut planned_targets,
                )
                .await
            } else {
                let (to, kind, conflict) = resolve_destination(
                    source_root,
                    &sub_from,
                    &sub_to,
                    "subtitle",
                    &mut planned_targets,
                )
                .await;
                (to, kind, None, conflict)
            };
            items.push(ReorgItem {
                from: sub_from,
                to,
                kind,
                destination_root_label: sidecar_root_label,
                ai_assisted: false,
                year_source: None,
                conflict,
            });
        }
    }
    items
}

fn media_kind_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movie",
        MediaKind::Episode => "episode",
        MediaKind::Track => "track",
    }
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
    scan_root_for_asset_type(root_label, root, MediaRootAssetType::Mixed, ai, tmdb).await
}

/// Root-aware form of [`scan_root`]. A configured Shows root must not use
/// the classifier's generic, last-resort movie interpretation for unnumbered
/// bonus clips: doing that produced one top-level `Title/Title.ext` folder
/// per trailer, interview, and featurette. The declared root type supplies
/// the structural context the normal library scan already uses.
pub async fn scan_root_for_asset_type(
    root_label: &str,
    root: &Path,
    asset_type: MediaRootAssetType,
    ai: Option<&AiClient>,
    tmdb: Option<&TmdbClient>,
) -> std::io::Result<ReorgPlan> {
    let mut video_files = Vec::new();
    walk(root, root, &mut video_files)?;
    video_files.sort();
    let known_show_roots = if asset_type == MediaRootAssetType::Shows {
        known_show_roots(&video_files)
    } else {
        Vec::new()
    };
    let damaged_path_owners = if asset_type == MediaRootAssetType::Shows {
        infer_damaged_path_owners(root, &video_files, &known_show_roots).await
    } else {
        HashMap::new()
    };

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
        if asset_type == MediaRootAssetType::Music {
            continue;
        }
        if asset_type == MediaRootAssetType::Movies
            && classify::classify(&unix_relative).is_some_and(|item| item.kind != MediaKind::Movie)
        {
            continue;
        }

        // `classify` deliberately has a best-effort movie fallback for every
        // recognized video extension. Keep that result even when it lacks a
        // year: dropping it here was why large flat libraries left most of
        // their files untouched once the bounded AI budget was exhausted.
        let Some(deterministic) = classify_for_reorganization(
            &unix_relative,
            asset_type,
            &known_show_roots,
            &damaged_path_owners,
        ) else {
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
            to: to.clone(),
            kind,
            destination_root_label: None,
            ai_assisted,
            year_source,
            conflict,
        });

        for (sub_from, sub_to) in find_sidecar_moves(root, &unix_relative, &to) {
            let (to, kind, conflict) =
                resolve_destination(root, &sub_from, &sub_to, "subtitle", &mut planned_targets).await;
            items.push(ReorgItem {
                from: sub_from,
                to,
                kind,
                destination_root_label: None,
                ai_assisted,
                year_source,
                conflict,
            });
        }
    }

    if asset_type == MediaRootAssetType::Shows {
        let already_planned: HashSet<String> = items.iter().map(|item| item.from.clone()).collect();
        for (alias_from, alias_to, kind) in artwork_only_show_alias_moves(
            &video_files,
            &known_show_roots,
            &already_planned,
        ) {
            let (to, kind, conflict) = resolve_destination(
                root,
                &alias_from,
                &alias_to,
                kind,
                &mut planned_targets,
            )
            .await;
            items.push(ReorgItem {
                from: alias_from,
                to,
                kind,
                destination_root_label: None,
                ai_assisted: false,
                year_source: None,
                conflict,
            });
        }
    }

    if asset_type == MediaRootAssetType::Movies {
        for (sidecar_from, sidecar_to) in find_relocated_movie_sidecars(&video_files, &items) {
            let (to, kind, conflict) = resolve_destination(
                root,
                &sidecar_from,
                &sidecar_to,
                "subtitle",
                &mut planned_targets,
            )
            .await;
            items.push(ReorgItem {
                from: sidecar_from,
                to,
                kind,
                destination_root_label: None,
                ai_assisted: false,
                year_source: None,
                conflict,
            });
        }
    }

    // Album art and disc-bundled assets have conventions that are not
    // movie/TV sidecars. Never sweep them into `_orphaned` in a Music root.
    if asset_type != MediaRootAssetType::Music {
        for (orphan_from, orphan_to) in find_orphans(&video_files, &items) {
            let (to, kind, conflict) = resolve_destination(
                root,
                &orphan_from,
                &orphan_to,
                "orphan",
                &mut planned_targets,
            )
            .await;
            items.push(ReorgItem {
                from: orphan_from,
                to,
                kind,
                destination_root_label: None,
                ai_assisted: false,
                year_source: None,
                conflict,
            });
        }
    }

    // Deterministic Plex-conformance validation over every media file in
    // the root — movies, episodes, and tracks alike, not just the videos
    // considered for a move above.
    let mut validation = Vec::new();
    for relative in &video_files {
        let unix_relative = to_unix(relative);
        let Some((_, is_audio)) = classify::media_extension(&unix_relative) else {
            continue;
        };
        if asset_type == MediaRootAssetType::Music && !is_audio {
            continue;
        }
        let classified = classify_for_reorganization(
            &unix_relative,
            asset_type,
            &known_show_roots,
            &damaged_path_owners,
        );
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

fn artwork_only_show_alias_moves(
    files: &[PathBuf],
    known_shows: &[String],
    already_planned: &HashSet<String>,
) -> Vec<(String, String, &'static str)> {
    let mut top_has_video = HashSet::new();
    for relative in files {
        let path = to_unix(relative);
        let Some(top) = path.split('/').next() else { continue };
        if classify::media_extension(&path).is_some_and(|(_, is_audio)| !is_audio) {
            top_has_video.insert(top.to_ascii_lowercase());
        }
    }

    let mut moves = Vec::new();
    for relative in files {
        let path = to_unix(relative);
        if already_planned.contains(&path) {
            continue;
        }
        let parts: Vec<&str> = path.split('/').collect();
        let Some(alias) = parts.first().copied() else { continue };
        if parts.len() < 2 || top_has_video.contains(&alias.to_ascii_lowercase()) {
            continue;
        }
        let alias_key = normalized_show_key(alias);
        let matching: Vec<&String> = known_shows
            .iter()
            .filter(|show| {
                !show.eq_ignore_ascii_case(alias) && normalized_show_key(show) == alias_key
            })
            .collect();
        if matching.len() != 1 {
            continue;
        }
        let file_name = parts.last().copied().unwrap_or_default();
        let kind = if subtitle_extension(file_name).is_some() {
            "subtitle"
        } else if file_name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ORPHAN_ARTWORK_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
        {
            "artwork"
        } else {
            continue;
        };
        let mut remainder: Vec<String> = parts[1..].iter().map(|part| (*part).to_string()).collect();
        if let Some(first) = remainder.first_mut() {
            if let Some(season) = season_number_from_folder(first) {
                *first = format!("Season {season:02}");
            }
        }
        moves.push((
            path,
            format!("{}/{}", matching[0], remainder.join("/")),
            kind,
        ));
    }
    moves
}

fn normalized_show_key(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn season_number_from_folder(folder: &str) -> Option<u32> {
    let lower = folder.to_ascii_lowercase();
    let rest = lower.strip_prefix("season")?.trim_start();
    let digits: String = rest.chars().take_while(|character| character.is_ascii_digit()).collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

/// Top-level directories already proven to be real shows by at least one
/// numbered episode. These anchors let a second reorganization run repair
/// the bad singleton folders created by the old algorithm, e.g.
/// `Friends - Gag Reel/Friends - Gag Reel.mkv` back under `Friends/`.
fn known_show_roots(files: &[PathBuf]) -> Vec<String> {
    let mut roots = Vec::new();
    for relative in files {
        let path = to_unix(relative);
        let Some(classified) = classify::classify(&path) else {
            continue;
        };
        if classified.kind != MediaKind::Episode || classified.episode.is_none() {
            continue;
        }
        let Some(top) = path.split('/').next().filter(|part| !part.is_empty()) else {
            continue;
        };
        if is_reserved_top_level(top) {
            continue;
        }
        if !roots.iter().any(|known: &String| known.eq_ignore_ascii_case(top)) {
            roots.push(top.to_string());
        }
    }
    roots.sort_by_key(|name| std::cmp::Reverse(name.len()));
    roots
}

fn is_reserved_top_level(name: &str) -> bool {
    name.starts_with('_')
        || plex::PlexExtraKind::from_dir_name(name).is_some()
        || matches!(name.to_ascii_lowercase().as_str(), "extras" | "images" | "sample" | "samples")
}

/// Recovers owners inside a damaged top-level category only when ffprobe
/// metadata names exactly one already proven show. A category can contain
/// material from multiple shows, so ownership is recorded per video. An
/// untagged video inherits its season/category group's owner only when every
/// tagged video in that group agrees; mixed and wholly untagged groups stay
/// untouched for review.
async fn infer_damaged_path_owners(
    root: &Path,
    files: &[PathBuf],
    known_shows: &[String],
) -> HashMap<String, String> {
    let mut candidates = Vec::new();
    for relative in files {
        let parts: Vec<String> = relative
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();
        let Some(top) = parts.first() else { continue };
        if !is_reserved_top_level(top)
            || !classify::media_extension(&to_unix(relative))
                .is_some_and(|(_, is_audio)| !is_audio)
        {
            continue;
        }
        candidates.push(relative.clone());
    }

    let probed = stream::iter(candidates.iter().cloned())
        .map(|relative| async move {
            let title = swarm_media::probe::container_title(&root.join(&relative)).await;
            (relative, title)
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;

    let mut owners = HashMap::new();
    let mut group_owners: HashMap<(String, String), HashSet<String>> = HashMap::new();
    for (relative, title) in probed {
        let Some(title) = title else {
            continue;
        };
        let matches: Vec<&String> = known_shows
            .iter()
            .filter(|show| metadata_title_names_show(&title, show))
            .collect();
        if matches.len() != 1 {
            continue;
        }
        let path = to_unix(&relative);
        let parts: Vec<&str> = path.split('/').collect();
        let group = (
            parts.first().unwrap_or(&"").to_ascii_lowercase(),
            parts.get(1).unwrap_or(&"").to_ascii_lowercase(),
        );
        owners.insert(path.to_ascii_lowercase(), matches[0].clone());
        group_owners.entry(group).or_default().insert(matches[0].clone());
    }
    for relative in candidates {
        let path = to_unix(&relative);
        if owners.contains_key(&path.to_ascii_lowercase()) {
            continue;
        }
        let parts: Vec<&str> = path.split('/').collect();
        let group = (
            parts.first().unwrap_or(&"").to_ascii_lowercase(),
            parts.get(1).unwrap_or(&"").to_ascii_lowercase(),
        );
        let Some(group_matches) = group_owners.get(&group) else {
            continue;
        };
        if group_matches.len() == 1 {
            owners.insert(
                path.to_ascii_lowercase(),
                group_matches.iter().next().expect("one owner").clone(),
            );
        }
    }
    owners
}

fn metadata_title_names_show(metadata_title: &str, show: &str) -> bool {
    let title = metadata_title.trim();
    title.len() >= show.len()
        && title
            .get(..show.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(show))
        && title
            .get(show.len()..)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with([' ', ':', '-', '(']))
}

fn classify_for_reorganization(
    relative_path: &str,
    asset_type: MediaRootAssetType,
    known_shows: &[String],
    damaged_path_owners: &HashMap<String, String>,
) -> Option<Classified> {
    let generic = classify::classify(relative_path)?;
    if asset_type != MediaRootAssetType::Shows {
        return classify::classify_for_asset_type(relative_path, asset_type).or(Some(generic));
    }

    let top = relative_path.split('/').next()?;
    if let Some(owner) = damaged_path_owners.get(&relative_path.to_ascii_lowercase()) {
        let mut repaired = generic;
        let path = Path::new(relative_path);
        let stem = path.file_stem()?.to_string_lossy().trim().to_string();
        repaired.kind = MediaKind::Episode;
        repaired.title = stem.clone();
        repaired.show_title = Some(owner.clone());
        repaired.season = season_from_relative_path(relative_path).or(Some(0));
        repaired.episode = None;
        repaired.episode_end = None;
        repaired.extra_kind = Some(
            plex::PlexExtraKind::from_dir_name(top)
                .unwrap_or(plex::PlexExtraKind::Other)
                .slug(),
        );
        repaired.extra_title = Some(stem);
        repaired.extra_parent_title = None;
        repaired.extra_parent_dir = None;
        return Some(repaired);
    }

    let top_is_known_show = known_shows.iter().any(|show| show.eq_ignore_ascii_case(top));
    if top_is_known_show {
        return classify::classify_for_asset_type(relative_path, asset_type).or(Some(generic));
    }

    // A year-bearing folder is an actual movie misplaced in the Shows root,
    // not a TV extra. Likewise the common Dragon Ball `M01` movie notation.
    // Leave these as movies so the existing cross-root detector points them
    // at the configured Movies library instead of hiding them under a show.
    if contains_explicit_release_year(top) || looks_like_numbered_movie(top) {
        return Some(generic);
    }

    // A reserved Plex category is never itself a show. If metadata did not
    // identify a safe owner above, leave the item in place for review rather
    // than canonically preserving the broken `Featurettes/Season ...` root.
    if is_reserved_top_level(top) {
        return None;
    }

    let parent = known_shows
        .iter()
        .find_map(|show| split_known_show_extra(top, show).map(|suffix| (show, suffix)));
    if let Some((parent, suffix)) = parent {
        let mut repaired = classify::classify_for_asset_type(relative_path, asset_type)?;
        repaired.show_title = Some(parent.clone());
        repaired.season = season_from_relative_path(relative_path).or(Some(0));
        repaired.episode = None;
        repaired.episode_end = None;
        let extra_kind = infer_extra_kind(suffix);
        repaired.extra_kind = Some(extra_kind.slug());
        repaired.extra_title = Some(clean_bug_split_extra_title(suffix));
        repaired.extra_parent_title = None;
        repaired.extra_parent_dir = None;
        return Some(repaired);
    }

    if generic.kind == MediaKind::Episode {
        return classify::classify_for_asset_type(relative_path, asset_type).or(Some(generic));
    }
    None
}

fn split_known_show_extra<'a>(folder: &'a str, show: &str) -> Option<&'a str> {
    let prefix = folder.get(..show.len())?;
    if !prefix.eq_ignore_ascii_case(show) {
        return None;
    }
    let suffix = folder.get(show.len()..)?.trim();
    if let Some(suffix) = suffix.strip_prefix('-') {
        return Some(suffix.trim());
    }
    let lower = suffix.to_ascii_lowercase();
    [
        "side story",
        "ova",
        "special",
        "featurette",
        "behind the scenes",
    ]
        .iter()
        .any(|marker| lower.starts_with(marker))
        .then_some(suffix)
        .or_else(|| lower.contains("picture drama").then_some(suffix))
}

fn season_from_relative_path(relative_path: &str) -> Option<u32> {
    relative_path.split('/').find_map(|part| {
        let rest = part.trim().to_ascii_lowercase();
        let digits = rest.strip_prefix("season")?.trim_start();
        let digits: String = digits.chars().take_while(|c| c.is_ascii_digit()).collect();
        (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
    })
}

fn contains_explicit_release_year(folder: &str) -> bool {
    folder
        .char_indices()
        .filter_map(|(index, _)| folder.get(index..index + 4))
        .any(|candidate| {
            candidate
                .parse::<u32>()
                .is_ok_and(|year| (1900..=2099).contains(&year))
        })
}

fn looks_like_numbered_movie(folder: &str) -> bool {
    folder.split(" - ").any(|part| {
        let upper = part.trim().to_ascii_uppercase();
        upper.strip_prefix('M')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn infer_extra_kind(title: &str) -> plex::PlexExtraKind {
    let lower = title.to_ascii_lowercase();
    if ["trailer", "promo", "tv spot", "sizzle reel"]
        .iter()
        .any(|token| lower.contains(token))
    {
        plex::PlexExtraKind::Trailers
    } else if ["interview", "answers your questions", "man-on-the-street", "voices of"]
        .iter()
        .any(|token| lower.contains(token))
    {
        plex::PlexExtraKind::Interviews
    } else if ["deleted", "alternate scene", "fake ending", "producer's cut", "extended"]
        .iter()
        .any(|token| lower.contains(token))
    {
        plex::PlexExtraKind::DeletedScenes
    } else if [
        "behind the scene", "making of", "gag reel", "music video", "live performance",
        "theme song", "clean opening", "clean ending", "nced", "ncop",
    ]
    .iter()
    .any(|token| lower.contains(token))
    {
        plex::PlexExtraKind::Featurettes
    } else {
        plex::PlexExtraKind::Other
    }
}

fn clean_bug_split_extra_title(suffix: &str) -> String {
    suffix
        .trim()
        .strip_suffix(" new")
        .unwrap_or(suffix.trim())
        .trim()
        .to_string()
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

/// Finds loose movie subtitles whose old pre-reorganization filename no
/// longer exactly matches the canonical video stem. A normalized title is
/// used only when it identifies exactly one video in the entire root; that
/// uniqueness requirement prevents remakes and similarly named films from
/// being guessed. The original language/Whisper suffix is preserved.
fn find_relocated_movie_sidecars(
    all_files: &[PathBuf],
    items: &[ReorgItem],
) -> Vec<(String, String)> {
    let mut videos_by_title: HashMap<String, Vec<String>> = HashMap::new();
    for relative in all_files {
        let path = to_unix(relative);
        let Some((_, is_audio)) = classify::media_extension(&path) else {
            continue;
        };
        if is_audio {
            continue;
        }
        let Some(classified) = classify::classify(&path) else {
            continue;
        };
        let key = normalized_title_key(&classified.title);
        if !key.is_empty() {
            videos_by_title.entry(key).or_default().push(path);
        }
    }

    let already_accounted: HashSet<&str> = items.iter().map(|item| item.from.as_str()).collect();
    let mut moves = Vec::new();
    for relative in all_files {
        let from = to_unix(relative);
        if already_accounted.contains(from.as_str()) || is_already_orphaned(&from) {
            continue;
        }
        let Some(extension) = subtitle_extension(&from) else {
            continue;
        };
        let Some(stem) = Path::new(&from).file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let parsed = parse_subtitle_name(stem);
        let synthetic_video = format!("{}.mkv", parsed.base_stem);
        let Some(sidecar_title) = classify::classify(&synthetic_video) else {
            continue;
        };
        let Some(matches) = videos_by_title.get(&normalized_title_key(&sidecar_title.title)) else {
            continue;
        };
        if matches.len() != 1 {
            continue;
        }
        let video = Path::new(&matches[0]);
        let Some(video_stem) = video.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let suffix = stem.strip_prefix(&parsed.base_stem).unwrap_or("");
        let target = video
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(format!("{video_stem}{suffix}.{extension}"));
        let target = to_unix(&target);
        if target != from {
            moves.push((from, target));
        }
    }
    moves
}

fn normalized_title_key(title: &str) -> String {
    title
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
        destination_root_label: None,
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
/// `"duplicate"` and `to` is redirected into `DUPLICATES_FOLDER`. A
/// different-content collision is preserved as an alternate version beside
/// the canonical file; it is never overwritten and no obsolete season
/// folder has to remain solely because two encodes share an episode number.
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
        return alternate_destination(root, source, target, default_kind, planned_targets);
    }
    if !planned_targets.insert(target.to_string()) {
        return alternate_destination(root, source, target, default_kind, planned_targets);
    }
    (target.to_string(), default_kind, None)
}

async fn resolve_cross_root_destination(
    source_root: &Path,
    destination_root: &Path,
    destination_root_label: &str,
    source: &str,
    target: &str,
    default_kind: &'static str,
    planned_targets: &mut HashSet<String>,
) -> (String, &'static str, Option<String>, Option<String>) {
    let dest_path = destination_root.join(target);
    if dest_path.exists() {
        if files_are_identical(source_root.join(source), dest_path).await {
            let (to, kind, conflict) = duplicate_destination(source, planned_targets);
            return (to, kind, None, conflict);
        }
        let (to, kind, conflict) = alternate_destination(
            destination_root,
            source,
            target,
            default_kind,
            planned_targets,
        );
        return (to, kind, Some(destination_root_label.to_string()), conflict);
    }
    if !planned_targets.insert(target.to_string()) {
        let (to, kind, conflict) = alternate_destination(
            destination_root,
            source,
            target,
            default_kind,
            planned_targets,
        );
        return (to, kind, Some(destination_root_label.to_string()), conflict);
    }
    (
        target.to_string(),
        default_kind,
        Some(destination_root_label.to_string()),
        None,
    )
}

fn alternate_destination(
    root: &Path,
    source: &str,
    target: &str,
    kind: &'static str,
    planned_targets: &mut HashSet<String>,
) -> (String, &'static str, Option<String>) {
    let target_path = Path::new(target);
    let parent = target_path.parent().unwrap_or_else(|| Path::new(""));
    let canonical_stem = target_path.file_stem().map_or_else(
        || "alternate".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let source_stem = Path::new(source)
        .file_stem()
        .map(|stem| sanitize(&stem.to_string_lossy()))
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "alternate".to_string());
    let extension = target_path
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    for suffix in 1..=10_000 {
        let label = if suffix == 1 {
            source_stem.clone()
        } else {
            format!("{source_stem} {suffix}")
        };
        let candidate = to_unix(&parent.join(format!("{canonical_stem} - {label}{extension}")));
        if !root.join(&candidate).exists() && planned_targets.insert(candidate.clone()) {
            return (candidate, kind, None);
        }
    }
    (
        target.to_string(),
        kind,
        Some("could not choose a unique alternate destination without overwriting".to_string()),
    )
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
                && (c.episode.is_some()
                    || (c.extra_kind.is_some()
                        && c.extra_title.as_deref().is_some_and(|title| !title.trim().is_empty())))
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
            if let (Some(kind), Some(title)) = (c.extra_kind, c.extra_title.as_deref()) {
                let category = extra_directory(kind);
                let title = sanitize(title);
                return if c.season.unwrap_or(0) > 0 {
                    let season = c.season.unwrap_or(1);
                    format!("{show}/Season {season:02}/{category}/{title}.{ext}")
                } else {
                    format!("{show}/{category}/{title}.{ext}")
                };
            }
            let season = c.season.unwrap_or(1);
            let episode = c.episode.unwrap_or(0);
            format!("{show}/Season {season:02}/{show} - S{season:02}E{episode:02}.{ext}")
        }
        MediaKind::Track => String::new(),
    }
}

fn extra_directory(slug: &str) -> &'static str {
    match slug {
        "behindTheScenes" => "Behind The Scenes",
        "deletedScene" => "Deleted Scenes",
        "featurette" => "Featurettes",
        "interview" => "Interviews",
        "scene" => "Scenes",
        "short" => "Shorts",
        "trailer" => "Trailers",
        _ => "Other",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMove {
    pub from: String,
    pub to: String,
    pub destination_root_label: Option<String>,
}

#[derive(Debug, Default)]
pub struct ApplyOutcome {
    pub applied: u32,
    pub skipped: u32,
    pub errors: Vec<String>,
    /// Exact journal of moves that succeeded. A plan can be partially
    /// applied, so undo must never infer this list from the original plan.
    pub applied_moves: Vec<AppliedMove>,
}

/// Applies every non-conflicting item by `fs::rename` — never a copy+delete
/// fallback, so a failed or partial move can never cost the source file.
/// Empty source directories are removed after their last file moves, but no
/// file is deleted. Re-checks existence right before each move (the plan may
/// be stale by the time a user approves it) rather than trusting the
/// scan-time snapshot.
pub fn apply_plan(root: &Path, items: &[ReorgItem]) -> ApplyOutcome {
    apply_plan_with_roots(root, &HashMap::new(), items)
}

pub fn apply_plan_with_roots(
    root: &Path,
    destination_roots: &HashMap<String, PathBuf>,
    items: &[ReorgItem],
) -> ApplyOutcome {
    let mut outcome = ApplyOutcome::default();
    for item in items {
        if let Some(reason) = &item.conflict {
            outcome.skipped += 1;
            outcome.errors.push(format!("{}: skipped ({reason})", item.from));
            continue;
        }
        let from = root.join(&item.from);
        let destination_root = match item.destination_root_label.as_deref() {
            Some(label) => match destination_roots.get(label) {
                Some(path) => path,
                None => {
                    outcome.skipped += 1;
                    outcome.errors.push(format!(
                        "{}: destination root \"{label}\" is no longer configured, skipped",
                        item.from
                    ));
                    continue;
                }
            },
            None => root,
        };
        let to = destination_root.join(&item.to);
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
            Ok(()) => {
                outcome.applied += 1;
                outcome.applied_moves.push(AppliedMove {
                    from: item.from.clone(),
                    to: item.to.clone(),
                    destination_root_label: item.destination_root_label.clone(),
                });
                remove_empty_ancestors(root, from.parent());
            }
            Err(error) => {
                outcome.skipped += 1;
                outcome.errors.push(format!("{}: move failed ({error}), left in place", item.from));
            }
        }
    }
    outcome
}

/// Reverses only the moves that actually succeeded, in reverse order.
/// Existing original paths are never overwritten; when one has reappeared,
/// the moved file remains at its reorganized path and is reported as skipped.
pub fn undo_plan(root: &Path, applied_moves: &[AppliedMove]) -> ApplyOutcome {
    undo_plan_with_roots(root, &HashMap::new(), applied_moves)
}

pub fn undo_plan_with_roots(
    root: &Path,
    destination_roots: &HashMap<String, PathBuf>,
    applied_moves: &[AppliedMove],
) -> ApplyOutcome {
    let mut outcome = ApplyOutcome::default();
    for applied in applied_moves.iter().rev() {
        let destination_root = match applied.destination_root_label.as_deref() {
            Some(label) => match destination_roots.get(label) {
                Some(path) => path,
                None => {
                    outcome.skipped += 1;
                    outcome.errors.push(format!(
                        "{}: destination root \"{label}\" is no longer configured, skipped",
                        applied.to
                    ));
                    continue;
                }
            },
            None => root,
        };
        let from = destination_root.join(&applied.to);
        let to = root.join(&applied.from);
        if !from.exists() {
            outcome.skipped += 1;
            outcome.errors.push(format!(
                "{}: reorganized file no longer exists, skipped",
                applied.to
            ));
            continue;
        }
        if to.exists() {
            outcome.skipped += 1;
            outcome.errors.push(format!(
                "{}: original path now exists, skipped to avoid overwrite",
                applied.from
            ));
            continue;
        }
        if let Some(parent) = to.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                outcome.skipped += 1;
                outcome.errors.push(format!(
                    "{}: could not recreate original folder ({error}), skipped",
                    applied.from
                ));
                continue;
            }
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => {
                outcome.applied += 1;
                outcome.applied_moves.push(AppliedMove {
                    from: applied.to.clone(),
                    to: applied.from.clone(),
                    destination_root_label: applied.destination_root_label.clone(),
                });
                remove_empty_ancestors(destination_root, from.parent());
            }
            Err(error) => {
                outcome.skipped += 1;
                outcome.errors.push(format!(
                    "{}: undo failed ({error}), left at reorganized path",
                    applied.to
                ));
            }
        }
    }
    outcome
}

fn remove_empty_ancestors(root: &Path, start: Option<&Path>) {
    let mut current = start.map(Path::to_path_buf);
    while let Some(dir) = current {
        if dir == root || !dir.starts_with(root) {
            break;
        }
        let parent = dir.parent().map(Path::to_path_buf);
        if std::fs::remove_dir(&dir).is_err() {
            break;
        }
        current = parent;
    }
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

    #[tokio::test]
    async fn shows_root_keeps_deep_extras_under_their_one_show_folder() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Aqua Teen Hunger Force/Season 01/Aqua Teen Hunger Force - S01E01.mkv",
            "episode",
        );
        write(
            dir.path(),
            "Aqua Teen Hunger Force/Featurettes/The Movie/Deleted Scenes/Dorm Room Extended.mkv",
            "extra",
        );

        let plan = scan_root_for_asset_type(
            "Shows",
            dir.path(),
            MediaRootAssetType::Shows,
            None,
            None,
        )
        .await
        .unwrap();

        let extra = plan
            .items
            .iter()
            .find(|item| item.from.ends_with("Dorm Room Extended.mkv"))
            .expect("deep extra should be normalized");
        assert_eq!(
            extra.to,
            "Aqua Teen Hunger Force/Deleted Scenes/Dorm Room Extended.mkv"
        );
        assert!(!extra.to.starts_with("Dorm Room Extended/"));
    }

    #[tokio::test]
    async fn shows_root_repairs_singleton_extra_folders_created_by_the_old_bug() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Friends/Season 01/Friends - S01E01.mkv",
            "episode",
        );
        write(
            dir.path(),
            "Friends - Gag Reel -The One with Never-Before-Seen Gags new/Friends - Gag Reel -The One with Never-Before-Seen Gags new.mkv",
            "extra",
        );
        write(
            dir.path(),
            "Friends - Gag Reel -The One with Never-Before-Seen Gags new/Friends - Gag Reel -The One with Never-Before-Seen Gags new.en.vtt",
            "subtitle",
        );

        let plan = scan_root_for_asset_type(
            "Shows",
            dir.path(),
            MediaRootAssetType::Shows,
            None,
            None,
        )
        .await
        .unwrap();

        let video = plan.items.iter().find(|item| item.kind == "video").unwrap();
        assert_eq!(
            video.to,
            "Friends/Featurettes/Gag Reel -The One with Never-Before-Seen Gags.mkv"
        );
        assert!(plan.items.iter().any(|item| {
            item.kind == "subtitle"
                && item.to
                    == "Friends/Featurettes/Gag Reel -The One with Never-Before-Seen Gags.en.vtt"
        }));
    }

    #[test]
    fn reserved_featurettes_root_is_recovered_under_the_metadata_identified_show() {
        let known = vec!["The Office".to_string()];
        let owners = HashMap::from([(
            "featurettes/season 09/featurettes - s09e23.mkv".to_string(),
            "The Office".to_string(),
        )]);
        let classified = classify_for_reorganization(
            "Featurettes/Season 09/Featurettes - S09E23.mkv",
            MediaRootAssetType::Shows,
            &known,
            &owners,
        )
        .expect("damaged category item should classify");

        assert_eq!(classified.kind, MediaKind::Episode);
        assert_eq!(classified.show_title.as_deref(), Some("The Office"));
        assert_eq!(classified.season, Some(9));
        assert_eq!(classified.episode, None);
        assert_eq!(classified.extra_kind, Some("featurette"));
        assert_eq!(
            canonical_video_path(&classified, "mkv"),
            "The Office/Season 09/Featurettes/Featurettes - S09E23.mkv"
        );
    }

    #[test]
    fn unowned_reserved_category_is_not_treated_as_a_show() {
        assert!(classify_for_reorganization(
            "Featurettes/Season 02/Featurettes - S02E01.mkv",
            MediaRootAssetType::Shows,
            &["Friends".to_string()],
            &HashMap::new(),
        )
        .is_none());
    }

    #[test]
    fn embedded_disc_title_matches_only_its_real_show_prefix() {
        assert!(metadata_title_names_show(
            "The Office: Season 9 (Disc 4)",
            "The Office"
        ));
        assert!(!metadata_title_names_show(
            "The Office: Season 9 (Disc 4)",
            "Office"
        ));
        assert!(!metadata_title_names_show("Super Friends", "Friends"));
    }

    #[test]
    fn old_ova_singleton_is_repaired_before_generic_episode_fallback() {
        let known = vec!["Dragon Ball Z".to_string()];
        let classified = classify_for_reorganization(
            "Dragon Ball Z Side Story - OVA1 - Plan to Eradicate the Saiyans - Part 1/Dragon Ball Z Side Story - OVA1 - Plan to Eradicate the Saiyans - Part 1.mkv",
            MediaRootAssetType::Shows,
            &known,
            &HashMap::new(),
        )
        .expect("OVA should classify as a show extra");

        assert_eq!(classified.show_title.as_deref(), Some("Dragon Ball Z"));
        assert_eq!(classified.episode, None);
        assert!(canonical_video_path(&classified, "mkv").starts_with("Dragon Ball Z/Other/"));
    }

    #[test]
    fn picture_drama_singleton_nests_under_its_known_series() {
        let known = vec!["Mobile Suit Gundam Wing".to_string()];
        let classified = classify_for_reorganization(
            "Mobile Suit Gundam Wing Frozen Teardrop Picture Drama/Mobile Suit Gundam Wing Frozen Teardrop Picture Drama.mkv",
            MediaRootAssetType::Shows,
            &known,
            &HashMap::new(),
        )
        .expect("picture drama should classify as a show extra");

        assert_eq!(classified.show_title.as_deref(), Some("Mobile Suit Gundam Wing"));
        assert!(canonical_video_path(&classified, "mkv")
            .starts_with("Mobile Suit Gundam Wing/Other/"));
    }

    #[tokio::test]
    async fn artwork_only_spelling_alias_merges_into_the_video_show_root() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Dragon Ball SUPER/Season 05/Dragon Ball SUPER - S05E55.mkv",
            "episode",
        );
        write(
            dir.path(),
            "Dragonball Super/Season 5 (2017-18)/images/episode-poster.jpg",
            "artwork",
        );

        let plan = scan_root_for_asset_type(
            "shows",
            dir.path(),
            MediaRootAssetType::Shows,
            None,
            None,
        )
        .await
        .unwrap();

        let artwork = plan
            .items
            .iter()
            .find(|item| item.from.ends_with("episode-poster.jpg"))
            .expect("artwork alias should be merged");
        assert_eq!(
            artwork.to,
            "Dragon Ball SUPER/Season 05/images/episode-poster.jpg"
        );
        assert_eq!(artwork.kind, "artwork");
    }

    #[tokio::test]
    async fn shows_root_keeps_feature_films_classified_as_movies() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Dragon Ball Z/Season 01/Dragon Ball Z - S01E01.mkv",
            "episode",
        );
        write(
            dir.path(),
            "Dragon Ball Z - M01 - Dead Zone/Dragon Ball Z - M01 - Dead Zone.mkv",
            "movie",
        );
        write(
            dir.path(),
            "Dragon Ball Super - BROLY (2018)/Dragon Ball Super - BROLY (2018).mkv",
            "movie",
        );

        let plan = scan_root_for_asset_type(
            "Shows",
            dir.path(),
            MediaRootAssetType::Shows,
            None,
            None,
        )
        .await
        .unwrap();

        assert!(plan.items.iter().all(|item| {
            !item.from.contains("M01 - Dead Zone") && !item.from.contains("BROLY (2018)")
        }));
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
    async fn movies_root_repairs_a_uniquely_matching_old_whisper_subtitle() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Black Widow (2021)/Black Widow (2021).mkv",
            "movie",
        );
        write(
            dir.path(),
            "Black Widow (1080p)-whisper-english-subtitles.vtt",
            "subtitle",
        );

        let plan = scan_root_for_asset_type(
            "movies",
            dir.path(),
            MediaRootAssetType::Movies,
            None,
            None,
        )
        .await
        .unwrap();

        let subtitle = plan
            .items
            .iter()
            .find(|item| item.kind == "subtitle")
            .expect("old subtitle should follow its uniquely matching movie");
        assert_eq!(
            subtitle.to,
            "Black Widow (2021)/Black Widow (2021)-whisper-english-subtitles.vtt"
        );
        assert!(plan.items.iter().all(|item| item.kind != "orphan"));
    }

    #[tokio::test]
    async fn music_root_leaves_album_art_and_bundled_video_extras_untouched() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Artist/Album/01 - Song.flac", "track");
        write(dir.path(), "Artist/Album/cover.jpg", "artwork");
        write(dir.path(), "Artist/Album/Extras/music-video.mpg", "video");

        let plan = scan_root_for_asset_type(
            "music",
            dir.path(),
            MediaRootAssetType::Music,
            None,
            None,
        )
        .await
        .unwrap();

        assert!(plan.items.is_empty());
        assert_eq!(plan.orphan_count, 0);
        let roots = vec![
            RootExpectation {
                label: "music".into(),
                expected_kind: Some(MediaKind::Track),
            },
            RootExpectation {
                label: "movies".into(),
                expected_kind: Some(MediaKind::Movie),
            },
        ];
        assert!(find_misplaced_content_for_asset_type(
            "music",
            dir.path(),
            &roots,
            MediaRootAssetType::Music,
        )
        .unwrap()
        .is_empty());
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
    async fn preserves_a_different_existing_destination_as_an_alternate_version() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mkv", "x");
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "already here");
        let plan = scan_root("local", dir.path(), None, None).await.unwrap();
        let video = plan.items.iter().find(|i| i.kind == "video").expect("video item");
        assert!(video.conflict.is_none());
        assert_eq!(video.kind, "video");
        assert!(video.to.starts_with("Heat (1995)/Heat (1995) - Heat.1995"));
        assert_eq!(plan.conflict_count, 0);
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
    async fn a_different_content_collision_is_preserved_as_an_alternate_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "the real thing");
        write(dir.path(), "Heat.1995.BDRip.x264-GROUP.mkv", "an unrelated remux");

        let plan = scan_root("local", dir.path(), None, None).await.unwrap();

        let video = plan.items.iter().find(|i| i.kind == "video").expect("video item");
        assert_eq!(video.from, "Heat.1995.BDRip.x264-GROUP.mkv");
        assert!(video.conflict.is_none());
        assert_eq!(video.to, "Heat (1995)/Heat (1995) - Heat.1995.BDRip.x264-GROUP.mkv");
        assert!(plan.items.iter().all(|i| i.kind != "duplicate"));
        assert_eq!(plan.duplicate_count, 0);
        assert_eq!(plan.conflict_count, 0);
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
    async fn gives_two_sources_with_the_same_target_unique_destinations() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mp4", "x");
        write(dir.path(), "Heat (1995).mp4", "x");
        let plan = scan_root("local", dir.path(), None, None).await.unwrap();
        assert_eq!(plan.items.len(), 2);
        assert!(plan.items.iter().all(|item| item.conflict.is_none()));
        assert_ne!(plan.items[0].to, plan.items[1].to);
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
                destination_root_label: None,
                ai_assisted: false,
                year_source: None,
                conflict: None,
            },
            ReorgItem {
                from: "does-not-exist.srt".to_string(),
                to: "Heat (1995)/Heat (1995).srt".to_string(),
                kind: "subtitle",
                destination_root_label: None,
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
            destination_root_label: None,
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

    #[test]
    fn undo_plan_restores_only_successfully_applied_moves() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Aqua Teen Hunger Force - Deleted Scene.mkv", "video");
        write(dir.path(), "Aqua Teen Hunger Force - Deleted Scene.en.srt", "subtitle");
        let items = vec![
            ReorgItem {
                from: "Aqua Teen Hunger Force - Deleted Scene.mkv".to_string(),
                to: "Aqua Teen Hunger Force/Deleted Scenes/Deleted Scene.mkv".to_string(),
                kind: "video",
                destination_root_label: None,
                ai_assisted: false,
                year_source: None,
                conflict: None,
            },
            ReorgItem {
                from: "Aqua Teen Hunger Force - Deleted Scene.en.srt".to_string(),
                to: "Aqua Teen Hunger Force/Deleted Scenes/Deleted Scene.en.srt".to_string(),
                kind: "subtitle",
                destination_root_label: None,
                ai_assisted: false,
                year_source: None,
                conflict: None,
            },
            ReorgItem {
                from: "missing.jpg".to_string(),
                to: "Aqua Teen Hunger Force/poster.jpg".to_string(),
                kind: "artwork",
                destination_root_label: None,
                ai_assisted: false,
                year_source: None,
                conflict: None,
            },
        ];

        let applied = apply_plan(dir.path(), &items);
        assert_eq!(applied.applied, 2);
        assert_eq!(applied.skipped, 1);
        assert_eq!(applied.applied_moves.len(), 2);

        let undone = undo_plan(dir.path(), &applied.applied_moves);
        assert_eq!(undone.applied, 2);
        assert_eq!(undone.skipped, 0);
        assert_eq!(
            fs::read_to_string(dir.path().join("Aqua Teen Hunger Force - Deleted Scene.mkv")).unwrap(),
            "video"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("Aqua Teen Hunger Force - Deleted Scene.en.srt")).unwrap(),
            "subtitle"
        );
        assert!(!dir.path().join("Aqua Teen Hunger Force").exists());
    }

    #[test]
    fn undo_plan_never_overwrites_an_original_path_that_reappeared() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Friends - Gag Reel.mkv", "original");
        let items = vec![ReorgItem {
            from: "Friends - Gag Reel.mkv".to_string(),
            to: "Friends/Featurettes/Gag Reel.mkv".to_string(),
            kind: "video",
            destination_root_label: None,
            ai_assisted: false,
            year_source: None,
            conflict: None,
        }];
        let applied = apply_plan(dir.path(), &items);
        write(dir.path(), "Friends - Gag Reel.mkv", "new file");

        let undone = undo_plan(dir.path(), &applied.applied_moves);

        assert_eq!(undone.applied, 0);
        assert_eq!(undone.skipped, 1);
        assert_eq!(fs::read_to_string(dir.path().join("Friends - Gag Reel.mkv")).unwrap(), "new file");
        assert_eq!(
            fs::read_to_string(dir.path().join("Friends/Featurettes/Gag Reel.mkv")).unwrap(),
            "original"
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

    // --- Wrong-media-root detection and reviewed correction (issue #301) ---

    #[tokio::test]
    async fn flags_an_episode_shaped_file_under_a_movies_root_and_names_the_shows_root() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Dragon Ball Super/Dragon.Ball.Super.S01E01.mkv", "x");
        let roots = vec![
            RootExpectation {
                label: "Movies".to_string(),
                expected_kind: Some(MediaKind::Movie),
            },
            RootExpectation {
                label: "Shows".to_string(),
                expected_kind: Some(MediaKind::Episode),
            },
        ];

        let misplaced = find_misplaced_content("Movies", dir.path(), &roots).unwrap();

        assert_eq!(misplaced.len(), 1);
        assert_eq!(misplaced[0].path, "Dragon Ball Super/Dragon.Ball.Super.S01E01.mkv");
        assert_eq!(misplaced[0].kind, "episode");
        assert_eq!(misplaced[0].current_root_label, "Movies");
        assert_eq!(misplaced[0].correct_root_label, "Shows");
    }

    #[tokio::test]
    async fn does_not_flag_content_that_matches_its_own_roots_expected_kind() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "10.Cloverfield.Lane.2016.1080p.BluRay.x264-GROUP.mkv", "x");
        let roots = vec![RootExpectation {
            label: "Movies".to_string(),
            expected_kind: Some(MediaKind::Movie),
        }];

        let misplaced = find_misplaced_content("Movies", dir.path(), &roots).unwrap();

        assert!(misplaced.is_empty());
    }

    #[test]
    fn typed_shows_detection_keeps_extras_but_reports_a_feature_film() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Aqua Teen Hunger Force/Season 01/Aqua Teen Hunger Force - S01E01.mkv",
            "episode",
        );
        write(
            dir.path(),
            "Aqua Teen Hunger Force/Deleted Scenes/Dorm Room Extended.mkv",
            "extra",
        );
        write(
            dir.path(),
            "Aqua Teen Hunger Force Colon Movie Film for Theaters (2007)/Aqua Teen Hunger Force Colon Movie Film for Theaters (2007).mkv",
            "movie",
        );
        let roots = vec![
            RootExpectation {
                label: "shows".to_string(),
                expected_kind: Some(MediaKind::Episode),
            },
            RootExpectation {
                label: "movies".to_string(),
                expected_kind: Some(MediaKind::Movie),
            },
        ];

        let misplaced = find_misplaced_content_for_asset_type(
            "shows",
            dir.path(),
            &roots,
            MediaRootAssetType::Shows,
        )
        .unwrap();

        assert_eq!(misplaced.len(), 1);
        assert!(misplaced[0].path.contains("Colon Movie Film for Theaters"));
    }

    #[tokio::test]
    async fn cross_root_movie_move_and_undo_are_journaled_and_safe() {
        let shows = tempfile::tempdir().unwrap();
        let movies = tempfile::tempdir().unwrap();
        let path = "Aqua Teen Hunger Force Colon Movie Film for Theaters (2007)/Aqua Teen Hunger Force Colon Movie Film for Theaters (2007).mkv";
        write(shows.path(), path, "movie");
        let misplaced = vec![MisplacedItem {
            path: path.to_string(),
            kind: "movie",
            current_root_label: "shows".to_string(),
            correct_root_label: "movies".to_string(),
        }];
        let items = plan_misplaced_moves(shows.path(), movies.path(), "movies", &misplaced).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].destination_root_label.as_deref(), Some("movies"));

        let roots = HashMap::from([("movies".to_string(), movies.path().to_path_buf())]);
        let applied = apply_plan_with_roots(shows.path(), &roots, &items);
        assert_eq!(applied.applied, 1);
        assert!(!shows.path().join(path).exists());
        assert!(movies.path().join(path).exists());

        let undone = undo_plan_with_roots(shows.path(), &roots, &applied.applied_moves);
        assert_eq!(undone.applied, 1);
        assert!(shows.path().join(path).exists());
        assert!(!movies.path().join(path).exists());
    }

    #[tokio::test]
    async fn a_mixed_root_never_reports_anything() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Dragon Ball Super/Dragon.Ball.Super.S01E01.mkv", "x");
        let roots = vec![
            RootExpectation {
                label: "Everything".to_string(),
                expected_kind: None,
            },
            RootExpectation {
                label: "Shows".to_string(),
                expected_kind: Some(MediaKind::Episode),
            },
        ];

        let misplaced = find_misplaced_content("Everything", dir.path(), &roots).unwrap();

        assert!(misplaced.is_empty());
    }

    #[tokio::test]
    async fn an_ambiguous_correct_root_is_left_out_of_the_report() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Dragon Ball Super/Dragon.Ball.Super.S01E01.mkv", "x");
        let roots = vec![
            RootExpectation {
                label: "Movies".to_string(),
                expected_kind: Some(MediaKind::Movie),
            },
            RootExpectation {
                label: "Shows A".to_string(),
                expected_kind: Some(MediaKind::Episode),
            },
            RootExpectation {
                label: "Shows B".to_string(),
                expected_kind: Some(MediaKind::Episode),
            },
        ];

        let misplaced = find_misplaced_content("Movies", dir.path(), &roots).unwrap();

        assert!(misplaced.is_empty());
    }

    /// Findings remain structurally separate from executable reviewed
    /// moves; `plan_misplaced_moves` is the only conversion point.
    #[test]
    fn misplaced_findings_cannot_be_applied_without_planning() {
        let _: fn(&Path, &[ReorgItem]) -> ApplyOutcome = apply_plan;
    }
}
