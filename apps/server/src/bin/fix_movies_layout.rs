//! One-off migration for a Movies library left in a mangled state by an
//! earlier ad hoc reorganization: newly-renamed movie folders ended up
//! nested under a spurious `Movies/Movies/` instead of directly under the
//! configured Movies root, the original release-named folders were left
//! behind full of non-video cruft (`.nfo`/`.txt`/scraped artwork), and a
//! batch of Whisper-generated subtitle files (see
//! `swarm_server::transcription::whisper_subtitle_path`) were orphaned at
//! the library root when their videos moved without them.
//!
//! Three phases, always run in this order and printed as one report before
//! anything is touched:
//!
//! 1. Run the real `reorganize::scan_root`/`apply_plan` against the whole
//!    root. `classify()` doesn't care about an extra `Movies/` ancestor
//!    wrapper folder for a plain movie file, so this naturally "flattens"
//!    anything nested under `Movies/Movies/` to its true canonical
//!    location as a side effect of normal reorganization — no bespoke
//!    flatten step needed. This also catches any videos still loose
//!    directly in the root.
//! 2. Quarantine (never delete) every top-level folder left with no video
//!    file inside once phase 1 has run, into `_cleanup_leftovers/<original
//!    name>/` for manual review.
//! 3. Match orphaned subtitle files still sitting loose at the root to
//!    their now-canonical movie folder: strip the Whisper suffix with the
//!    same `parse_subtitle_name` the live matcher uses, feed the remaining
//!    stem through `classify()` (with a synthesized video extension) to
//!    recover a clean title/year using the exact scene-release parsing
//!    real video files go through, then match that against the canonical
//!    folder list. A subtitle with no confident single match is left in
//!    place and reported — never guessed.
//!
//! Usage:
//!   fix-movies-layout <path to Movies folder> [--apply]
//!
//! Without `--apply` this only prints the full plan (dry run, the default —
//! safer default than requiring an opt-out flag, given the scale of a
//! real library). Nothing on disk changes until `--apply` is passed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use swarm_media::classify;
use swarm_media::subtitles::{parse_subtitle_name, subtitle_extension};
use swarm_server::reorganize;

/// Folder names at the Movies root this tool must never treat as "leftover
/// cruft to quarantine" or "a subtitle to match" — its own quarantine
/// destination, and the artwork cache a real swarm install keeps there.
const RESERVED_TOP_LEVEL_NAMES: &[&str] = &["_cleanup_leftovers", "images"];

fn is_video_path(relative: &str) -> bool {
    classify::media_extension(relative).is_some_and(|(_, is_audio)| !is_audio)
}

/// Whether `dir` still has a video anywhere under it, treating every path
/// in `moved_away` as already gone even if phase 1 hasn't actually applied
/// its plan yet — without this, a dry-run preview of phase 2 would see
/// every original release folder as still having its video (since nothing
/// has moved on disk) and never propose quarantining any of them, wildly
/// under-representing what `--apply` will actually do.
fn dir_contains_a_video(dir: &Path, moved_away: &HashSet<PathBuf>) -> bool {
    fn walk(dir: &Path, moved_away: &HashSet<PathBuf>) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if walk(&path, moved_away) {
                    return true;
                }
            } else if file_type.is_file() && !moved_away.contains(&path) {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if is_video_path(name) {
                        return true;
                    }
                }
            }
        }
        false
    }
    walk(dir, moved_away)
}

/// Every source path phase 1 plans to move a video out of (excluding any
/// conflicted item, which will stay right where it is) — the "predicted
/// post-phase-1 state" phases 2 and 3 need to give an accurate dry-run
/// preview instead of only reflecting what's on disk *right now*.
fn planned_video_sources(root: &Path, plan: &reorganize::ReorgPlan) -> HashSet<PathBuf> {
    plan.items
        .iter()
        .filter(|item| item.kind == "video" && item.conflict.is_none())
        .map(|item| root.join(&item.from))
        .collect()
}

/// Lowercased top-level canonical folder name -> (its real-case name, the
/// stem the video inside it will have), predicted from phase 1's plan
/// (`to` is always `"<dir>/<stem>.<ext>"` for a video item) rather than
/// read off disk, since that folder may not exist yet in a dry run.
fn planned_video_dirs(plan: &reorganize::ReorgPlan) -> HashMap<String, (String, String)> {
    let mut by_dir = HashMap::new();
    for item in &plan.items {
        if item.kind != "video" || item.conflict.is_some() {
            continue;
        }
        let Some((dir, file)) = item.to.split_once('/') else {
            continue;
        };
        if let Some(stem) = Path::new(file).file_stem().and_then(|s| s.to_str()) {
            by_dir.insert(dir.to_lowercase(), (dir.to_string(), stem.to_string()));
        }
    }
    by_dir
}

/// Same character-stripping rule `reorganize::canonical_video_path` uses
/// for a folder/file name — duplicated here (rather than made `pub` on a
/// private helper) since it's three lines and this is a one-off tool, not
/// a second caller worth coupling the product code's visibility to.
fn sanitize(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect::<String>()
        .trim()
        .to_string()
}

struct QuarantineMove {
    from: PathBuf,
    to: PathBuf,
}

fn plan_quarantine(root: &Path, moved_away: &HashSet<PathBuf>) -> std::io::Result<Vec<QuarantineMove>> {
    let mut moves = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if RESERVED_TOP_LEVEL_NAMES.contains(&name) {
            continue;
        }
        if !dir_contains_a_video(&path, moved_away) {
            moves.push(QuarantineMove {
                from: path.clone(),
                to: root.join("_cleanup_leftovers").join(name),
            });
        }
    }
    moves.sort_by(|a, b| a.from.cmp(&b.from));
    Ok(moves)
}

fn apply_quarantine(moves: &[QuarantineMove]) {
    for m in moves {
        if m.to.exists() {
            println!(
                "  SKIP  {} -> {} (destination already exists)",
                m.from.display(),
                m.to.display()
            );
            continue;
        }
        if let Some(parent) = m.to.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                println!("  SKIP  {} (could not create {}: {error})", m.from.display(), parent.display());
                continue;
            }
        }
        match std::fs::rename(&m.from, &m.to) {
            Ok(()) => println!("  MOVED {} -> {}", m.from.display(), m.to.display()),
            Err(error) => println!("  SKIP  {} (move failed: {error})", m.from.display()),
        }
    }
}

enum SubtitleMove {
    Matched { from: PathBuf, to: PathBuf },
    Unmatched { from: PathBuf, reason: String },
}

/// The single video file directly inside `dir`, if there is exactly one —
/// a subtitle only ever gets attached to an unambiguous match, same
/// "ambiguous is worse than missing" rule `match_subtitle_to_video` uses.
fn sole_video_stem(dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut found: Option<PathBuf> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_video_path(name) {
            if found.is_some() {
                return None;
            }
            found = Some(path);
        }
    }
    found.and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
}

fn plan_subtitle_matches(root: &Path, plan: &reorganize::ReorgPlan) -> std::io::Result<Vec<SubtitleMove>> {
    // Canonical movie folders are every top-level directory that will exist
    // once phase 1 has run — real directories already on disk, unioned
    // with anything phase 1 plans to create, predicted from `plan` rather
    // than read off disk. Without the predicted half, a dry-run preview
    // (nothing actually moved yet) would only ever "find" folders that
    // already existed before this tool ran at all — every subtitle whose
    // video phase 1 is about to relocate or newly organize would wrongly
    // report "no folder found", drastically under-representing what
    // `--apply` will actually match. Each entry also predicts the video
    // stem phase 1 will give that folder, so the destination filename can
    // be computed without needing that folder to exist yet either.
    let predicted = planned_video_dirs(plan);
    let mut canonical_dirs: HashMap<String, (PathBuf, Option<String>)> = HashMap::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if RESERVED_TOP_LEVEL_NAMES.contains(&name) {
            continue;
        }
        let lower = name.to_lowercase();
        let predicted_stem = predicted.get(&lower).map(|(_, stem)| stem.clone());
        canonical_dirs.insert(lower, (path, predicted_stem));
    }
    for (lower, (case_preserved_name, stem)) in &predicted {
        canonical_dirs
            .entry(lower.clone())
            .or_insert_with(|| (root.join(case_preserved_name), Some(stem.clone())));
    }

    let mut moves = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(ext) = subtitle_extension(file_name) else {
            continue;
        };
        let stem = file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(file_name);
        let parsed = parse_subtitle_name(stem);
        if parsed.base_stem.is_empty() {
            moves.push(SubtitleMove::Unmatched {
                from: path,
                reason: "no video-identifying stem left after stripping language/modifier tokens".to_string(),
            });
            continue;
        }
        // classify() only accepts a recognized video extension — synthesize
        // one so the same scene-release title/year parser real video files
        // go through can be reused here instead of a second, drifting copy
        // of that logic.
        let fake_video_name = format!("{}.mkv", parsed.base_stem);
        let Some(classified) = classify::classify(&fake_video_name) else {
            moves.push(SubtitleMove::Unmatched {
                from: path,
                reason: format!("could not derive a title from \"{}\"", parsed.base_stem),
            });
            continue;
        };
        let canonical_name = match classified.year {
            Some(year) => format!("{} ({year})", sanitize(&classified.title)),
            None => sanitize(&classified.title),
        };
        let lower = canonical_name.to_lowercase();
        let Some((target_dir, predicted_stem)) = canonical_dirs.get(&lower) else {
            moves.push(SubtitleMove::Unmatched {
                from: path,
                reason: format!("no folder named \"{canonical_name}\" found"),
            });
            continue;
        };
        // A predicted stem (phase 1 is about to put exactly one video
        // there) is exact; otherwise this is a folder phase 1 leaves
        // untouched (already canonical), so read its one real video off
        // disk the same way phase 1 itself would have named the subtitle.
        let video_stem = match predicted_stem {
            Some(stem) => stem.clone(),
            None => match sole_video_stem(target_dir) {
                Some(stem) => stem,
                None => {
                    moves.push(SubtitleMove::Unmatched {
                        from: path,
                        reason: format!(
                            "\"{}\" has no single video to attach the subtitle to",
                            target_dir.display()
                        ),
                    });
                    continue;
                }
            },
        };
        let lang_suffix = parsed.language.as_deref().map(|l| format!(".{l}")).unwrap_or_default();
        let to = target_dir.join(format!("{video_stem}{lang_suffix}.{ext}"));
        moves.push(SubtitleMove::Matched { from: path, to });
    }
    moves.sort_by(|a, b| subtitle_move_from(a).cmp(subtitle_move_from(b)));
    Ok(moves)
}

fn subtitle_move_from(m: &SubtitleMove) -> &Path {
    match m {
        SubtitleMove::Matched { from, .. } => from,
        SubtitleMove::Unmatched { from, .. } => from,
    }
}

fn apply_subtitle_moves(moves: &[SubtitleMove]) {
    for m in moves {
        let SubtitleMove::Matched { from, to } = m else {
            continue;
        };
        if to.exists() {
            println!("  SKIP  {} -> {} (destination already exists)", from.display(), to.display());
            continue;
        }
        match std::fs::rename(from, to) {
            Ok(()) => println!("  MOVED {} -> {}", from.display(), to.display()),
            Err(error) => println!("  SKIP  {} (move failed: {error})", from.display()),
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let apply = args.iter().any(|a| a == "--apply");
    let root_arg = args.iter().skip(1).find(|a| !a.starts_with("--"));
    let Some(root_arg) = root_arg else {
        eprintln!("usage: fix-movies-layout <path to Movies folder> [--apply]");
        std::process::exit(2);
    };
    let root = PathBuf::from(root_arg);
    if !root.is_dir() {
        eprintln!("{}: not a directory", root.display());
        std::process::exit(1);
    }

    println!("=== Phase 1: reorganize (also flattens any nested Movies/Movies/) ===");
    let plan = match reorganize::scan_root("Movies", &root, None, None).await {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("scan failed: {error}");
            std::process::exit(1);
        }
    };
    println!(
        "  {} video move(s) proposed, {} conflict(s)",
        plan.items.iter().filter(|i| i.kind == "video").count(),
        plan.conflict_count,
    );
    for item in &plan.items {
        match &item.conflict {
            Some(reason) => println!("  SKIP  [{}] {} -> {} ({reason})", item.kind, item.from, item.to),
            None => println!("  {: <5} [{}] {} -> {}", if apply { "MOVE" } else { "PLAN" }, item.kind, item.from, item.to),
        }
    }
    if !plan.validation.is_empty() {
        println!("  {} pre-existing Plex-conformance issue(s) found (unrelated to this run):", plan.validation.len());
        for issue in &plan.validation {
            println!("    {} — {}", issue.current_path, issue.problem);
        }
    }
    if apply {
        let outcome = reorganize::apply_plan(&root, &plan.items);
        println!(
            "  applied: {} moved, {} skipped, {} error(s)",
            outcome.applied, outcome.skipped, outcome.errors.len()
        );
        for error in &outcome.errors {
            println!("    {error}");
        }
    }

    let moved_away = planned_video_sources(&root, &plan);

    println!("\n=== Phase 2: quarantine leftover non-video folders ===");
    let quarantine = match plan_quarantine(&root, &moved_away) {
        Ok(moves) => moves,
        Err(error) => {
            eprintln!("could not scan {} for leftovers: {error}", root.display());
            std::process::exit(1);
        }
    };
    if quarantine.is_empty() {
        println!("  nothing to quarantine.");
    }
    for m in &quarantine {
        println!("  {} [folder] {} -> {}", if apply { "MOVE" } else { "PLAN" }, m.from.display(), m.to.display());
    }
    if apply {
        apply_quarantine(&quarantine);
    }

    println!("\n=== Phase 3: match orphaned subtitles to their canonical folder ===");
    let subtitle_moves = match plan_subtitle_matches(&root, &plan) {
        Ok(moves) => moves,
        Err(error) => {
            eprintln!("could not scan {} for orphaned subtitles: {error}", root.display());
            std::process::exit(1);
        }
    };
    let (matched, unmatched): (Vec<_>, Vec<_>) = subtitle_moves
        .iter()
        .partition(|m| matches!(m, SubtitleMove::Matched { .. }));
    println!("  {} matched, {} unmatched", matched.len(), unmatched.len());
    for m in &subtitle_moves {
        match m {
            SubtitleMove::Matched { from, to } => {
                println!("  {: <5} [subtitle] {} -> {}", if apply { "MOVE" } else { "PLAN" }, from.display(), to.display())
            }
            SubtitleMove::Unmatched { from, reason } => {
                println!("  SKIP  [subtitle] {} ({reason})", from.display())
            }
        }
    }
    if apply {
        apply_subtitle_moves(&subtitle_moves);
    }

    if !apply {
        println!("\nDry run only — nothing was changed. Re-run with --apply to execute.");
    }
}
