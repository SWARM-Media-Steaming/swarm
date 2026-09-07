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
//! Scope: movies and TV episodes only. Music libraries already have their
//! own artist/album folder convention (recovered from flat filenames when
//! needed — see the "Recover artist/album from filenames in flat music
//! libraries" work) and aren't touched here.

use crate::ai::AiClient;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use swarm_core::peer::MediaKind;
use swarm_media::classify::{self, Classified};
use swarm_media::plex::{self, PlexValidationIssue};
use swarm_media::subtitles::{parse_subtitle_name, subtitle_extension};

/// Cap on how many AI calls one scan will make, so a folder full of
/// ambiguous names can't turn into an unbounded (and unboundedly expensive)
/// run. Files beyond the cap still use the deterministic classifier and are
/// never silently omitted from the plan.
const MAX_AI_GUESSES: usize = 25;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReorgItem {
    /// Path relative to the scanned root, forward-slashed.
    pub from: String,
    pub to: String,
    pub kind: &'static str,
    pub ai_assisted: bool,
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
    pub conflict_count: u32,
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
/// place at all — see the module doc comment.
pub async fn scan_root(root_label: &str, root: &Path, ai: Option<&AiClient>) -> std::io::Result<ReorgPlan> {
    let mut video_files = Vec::new();
    walk(root, root, &mut video_files)?;
    video_files.sort();

    let mut items = Vec::new();
    let mut ai_assisted_count = 0u32;
    let mut ai_budget = MAX_AI_GUESSES;
    let mut planned_targets: HashSet<String> = HashSet::new();

    for relative in &video_files {
        let unix_relative = to_unix(relative);
        let Some((ext, false)) = classify::media_extension(&unix_relative) else {
            continue;
        };

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

        let canonical = canonical_video_path(&classified, ext);
        if canonical == unix_relative {
            continue;
        }
        if ai_assisted {
            ai_assisted_count += 1;
        }

        let conflict = conflict_reason(root, &canonical, &mut planned_targets);
        items.push(ReorgItem {
            from: unix_relative.clone(),
            to: canonical.clone(),
            kind: "video",
            ai_assisted,
            conflict,
        });

        for (sub_from, sub_to) in find_sidecar_moves(root, &unix_relative, &canonical) {
            let conflict = conflict_reason(root, &sub_to, &mut planned_targets);
            items.push(ReorgItem {
                from: sub_from,
                to: sub_to,
                kind: "subtitle",
                ai_assisted,
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
        if classify::media_extension(&unix_relative).is_none() {
            continue;
        }
        let classified = classify::classify(&unix_relative);
        if let Some(issue) = plex::validate_media_file(&unix_relative, classified.as_ref()) {
            validation.push(issue);
        }
    }

    let conflict_count = items.iter().filter(|i| i.conflict.is_some()).count() as u32;
    Ok(ReorgPlan {
        root_label: root_label.to_string(),
        items,
        ai_assisted_count,
        conflict_count,
        validation,
    })
}

fn conflict_reason(root: &Path, target: &str, planned_targets: &mut HashSet<String>) -> Option<String> {
    if root.join(target).exists() {
        Some("a file already exists at the destination".to_string())
    } else if !planned_targets.insert(target.to_string()) {
        Some("likely a duplicate — another item in this plan already targets this path".to_string())
    } else {
        None
    }
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

    #[tokio::test]
    async fn proposes_a_canonical_movie_folder_for_a_scene_release_name() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "10.Cloverfield.Lane.2016.1080p.BluRay.x264-GROUP.mkv", "x");
        let plan = scan_root("local", dir.path(), None).await.unwrap();
        assert_eq!(plan.items.len(), 1);
        let item = &plan.items[0];
        assert_eq!(item.to, "10 Cloverfield Lane (2016)/10 Cloverfield Lane (2016).mkv");
        assert!(item.conflict.is_none());
        assert!(!item.ai_assisted);
    }

    #[tokio::test]
    async fn brings_a_matching_subtitle_sidecar_along() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mkv", "x");
        write(dir.path(), "Heat.1995.en.srt", "x");
        write(dir.path(), "Heat.1995.es.vtt", "x");
        let plan = scan_root("local", dir.path(), None).await.unwrap();
        let subtitles: Vec<_> = plan.items.iter().filter(|i| i.kind == "subtitle").collect();
        assert_eq!(subtitles.len(), 2);
        assert!(subtitles.iter().any(|item| item.to == "Heat (1995)/Heat (1995).en.srt"));
        assert!(subtitles.iter().any(|item| item.to == "Heat (1995)/Heat (1995).es.vtt"));
        assert!(subtitles.iter().all(|item| item.conflict.is_none()));
    }

    #[tokio::test]
    async fn flags_a_conflict_when_the_destination_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mkv", "x");
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "already here");
        let plan = scan_root("local", dir.path(), None).await.unwrap();
        let video = plan.items.iter().find(|i| i.kind == "video").expect("video item");
        assert!(video.conflict.is_some());
        assert_eq!(plan.conflict_count, 1);
    }

    #[tokio::test]
    async fn leaves_an_already_canonical_file_out_of_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat (1995)/Heat (1995).mkv", "x");
        let plan = scan_root("local", dir.path(), None).await.unwrap();
        assert!(plan.items.is_empty());
    }

    #[tokio::test]
    async fn proposes_every_video_even_when_metadata_is_incomplete_and_ai_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "asdf1234.mkv", "x");
        let plan = scan_root("local", dir.path(), None).await.unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].to, "asdf1234/asdf1234.mkv");
        assert_eq!(plan.ai_assisted_count, 0);
    }

    #[tokio::test]
    async fn identifies_two_sources_with_the_same_target_as_likely_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Heat.1995.mp4", "x");
        write(dir.path(), "Heat (1995).mp4", "x");
        let plan = scan_root("local", dir.path(), None).await.unwrap();
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
                conflict: None,
            },
            ReorgItem {
                from: "does-not-exist.srt".to_string(),
                to: "Heat (1995)/Heat (1995).srt".to_string(),
                kind: "subtitle",
                ai_assisted: false,
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
}
