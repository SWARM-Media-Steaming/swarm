//! One-off migration for issue #41: earlier builds wrote Whisper-generated
//! subtitles into `<app data dir>/subtitles/`. The server now writes them
//! alongside the source media file instead (see
//! `swarm_server::transcription::whisper_subtitle_path`), so every
//! previously generated file needs to be moved/renamed to its new location
//! and `subtitle_tracks.file_path` needs to be updated to match.
//!
//! Before running this: disable "Generate with Whisper" in Details and quit
//! the SWARM media server, so nothing is reading or writing `library.sqlite`
//! or a `.part` file while this runs. Restart the server afterward — it will
//! resume generating subtitles (in the new location) for anything still
//! queued.
//!
//! Usage:
//!   migrate-whisper-subtitles <path to app data dir> [--dry-run]
//!
//! The app data dir is the same directory that holds `settings.json` and
//! `library.sqlite` (macOS: `~/Library/Application Support/<bundle id>`,
//! Linux: `~/.local/share/<bundle id>`, Windows: `%APPDATA%\<bundle id>`).

use std::path::PathBuf;
use swarm_media::roots::{MediaRoot, RootResolver};
use swarm_media::store::Library;
use swarm_server::transcription::whisper_subtitle_path;

#[derive(serde::Deserialize, Default)]
struct MediaRootSetting {
    label: String,
    path: String,
}

#[derive(serde::Deserialize, Default)]
struct SettingsFile {
    #[serde(default)]
    media_roots: Vec<MediaRootSetting>,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let positional: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|arg| arg.as_str() != "--dry-run")
        .collect();
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let Some(data_dir) = positional.first() else {
        eprintln!("usage: migrate-whisper-subtitles <path to app data dir> [--dry-run]");
        std::process::exit(2);
    };
    let data_dir = PathBuf::from(data_dir);

    let settings_path = data_dir.join("settings.json");
    let settings: SettingsFile = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default();
    if settings.media_roots.is_empty() {
        eprintln!(
            "no media roots found in {} — is this the right app data directory?",
            settings_path.display()
        );
        std::process::exit(1);
    }
    let roots = RootResolver::new(
        settings
            .media_roots
            .into_iter()
            .map(|root| MediaRoot {
                label: root.label,
                path: PathBuf::from(root.path),
                asset_type: Default::default(),
            })
            .collect(),
    );

    let database_path = data_dir.join("library.sqlite");
    let library = match Library::open(&database_path.to_string_lossy()).await {
        Ok(library) => library,
        Err(error) => {
            eprintln!("could not open {}: {error}", database_path.display());
            std::process::exit(1);
        }
    };

    let tracks = match library.subtitle_tracks_by_source("whisper").await {
        Ok(tracks) => tracks,
        Err(error) => {
            eprintln!("could not read subtitle_tracks: {error}");
            std::process::exit(1);
        }
    };

    if tracks.is_empty() {
        println!("no Whisper-generated subtitles found — nothing to migrate.");
        return;
    }

    let (mut moved, mut already_correct, mut skipped) = (0u32, 0u32, 0u32);
    for mut track in tracks {
        let entry = match library.get(&track.entry_key).await {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                eprintln!(
                    "skipping {}: library entry no longer exists",
                    track.file_path
                );
                skipped += 1;
                continue;
            }
            Err(error) => {
                eprintln!("skipping {}: {error}", track.file_path);
                skipped += 1;
                continue;
            }
        };
        let media_path = roots.resolve(&entry.relative_path);
        let new_path = whisper_subtitle_path(&media_path);
        let old_path = PathBuf::from(&track.file_path);

        if old_path == new_path {
            already_correct += 1;
            continue;
        }
        if new_path.exists() {
            println!(
                "skipping {}: destination already exists ({})",
                old_path.display(),
                new_path.display()
            );
            skipped += 1;
            continue;
        }
        if !old_path.is_file() {
            eprintln!(
                "skipping {}: source file no longer exists",
                old_path.display()
            );
            skipped += 1;
            continue;
        }

        println!("{} -> {}", old_path.display(), new_path.display());
        if dry_run {
            moved += 1;
            continue;
        }

        if let Some(parent) = new_path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("could not create {}: {error}", parent.display());
                skipped += 1;
                continue;
            }
        }
        if let Err(rename_error) = std::fs::rename(&old_path, &new_path) {
            // Old and new locations can be on different filesystems (e.g. a
            // network share moved since the file was generated) — fall back
            // to copy + remove rather than failing the whole migration.
            if let Err(copy_error) = std::fs::copy(&old_path, &new_path)
                .and_then(|_| std::fs::remove_file(&old_path))
            {
                eprintln!(
                    "could not move {}: rename failed ({rename_error}), copy fallback also failed ({copy_error})",
                    old_path.display()
                );
                skipped += 1;
                continue;
            }
        }

        track.file_path = new_path.to_string_lossy().to_string();
        if let Err(error) = library.upsert_subtitle(&track).await {
            eprintln!(
                "moved {} but could not update the database: {error}",
                new_path.display()
            );
            skipped += 1;
            continue;
        }
        moved += 1;
    }

    println!(
        "{}{} moved, {already_correct} already in place, {skipped} skipped.",
        if dry_run { "[dry run] " } else { "" },
        moved
    );
    if dry_run {
        println!("re-run without --dry-run to apply these changes.");
    }
}
