//! Save downloaded artwork bytes to disk, following the Drone convention:
//! images live in an `images/` subfolder beside the source file(s), not in
//! the library database. Movies get a per-file name (two files in the same
//! folder, e.g. `S01E01.mkv`/`S01E02.mkv`, must not collide); music uses
//! fixed names shared by every track in the album folder.

use crate::roots::SharedRootResolver;
use crate::store::{ArtworkReference, Library};
use futures_util::{stream, StreamExt};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtworkReconcileReport {
    pub recovered: u64,
    pub cleared: u64,
}

pub fn sanitize_stem(stem: &str) -> String {
    let cleaned: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn file_stem(relative_path: &str) -> &str {
    let name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name)
}

/// Write `bytes` into `<folder containing relative_path>/images/<filename>`
/// and return the artwork's own stored `relative_path` (resolved through the
/// same root that owns `relative_path`, and re-labeled the same way — see
/// `crate::roots::RootResolver`), for storage via `Library::set_artwork`.
pub async fn save_artwork(
    roots: &SharedRootResolver,
    relative_path: &str,
    filename: &str,
    bytes: &[u8],
) -> std::io::Result<String> {
    let (root_path, _) = roots.split(relative_path);
    let source_path = roots.resolve_existing(relative_path);
    let parent = source_path.parent().unwrap_or(&root_path);
    let images_dir = parent.join("images");
    tokio::fs::create_dir_all(&images_dir).await?;
    let target = images_dir.join(filename);
    tokio::fs::write(&target, bytes).await?;
    let relative_under_root = target
        .strip_prefix(&root_path)
        .unwrap_or(&target)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let label = roots.label_for(relative_path);
    Ok(roots.compose(&label, &relative_under_root))
}

/// True if the artwork file at a stored `relative_path` (as returned by
/// [`save_artwork`] and recorded via `Library::set_artwork`) is still
/// present on disk. Lets a non-force scrape skip re-downloading artwork
/// whose DB row is intact but whose file was, say, manually deleted —
/// checking the DB column alone would wrongly treat that as "already have
/// it" and never repair it.
pub async fn exists(roots: &SharedRootResolver, relative_path: &str) -> bool {
    let absolute = roots.resolve_existing(relative_path);
    tokio::fs::try_exists(&absolute).await.unwrap_or(false)
}

fn absolute_path(roots: &SharedRootResolver, relative_path: &str) -> std::path::PathBuf {
    roots.resolve_existing(relative_path)
}

/// Makes stored artwork references truthful again. A historical reorganization
/// bug moved media while leaving its sibling `images/` directory behind, then
/// restored metadata with paths remapped to the new (nonexistent) location.
/// Prefer copying the still-existing archived image into that intended path;
/// when no surviving source exists, clear the stale column so an ordinary
/// missing-only scrape will download it again.
pub async fn reconcile_references(
    library: &Library,
    roots: &SharedRootResolver,
) -> sqlx::Result<ArtworkReconcileReport> {
    let references = library.artwork_references().await?;
    let outcomes = stream::iter(references)
        .map(|reference| async move { reconcile_reference(library, roots, reference).await })
        .buffer_unordered(32)
        .collect::<Vec<_>>()
        .await;

    let mut report = ArtworkReconcileReport::default();
    for outcome in outcomes {
        match outcome? {
            ReconcileOutcome::Unchanged => {}
            ReconcileOutcome::Recovered => report.recovered += 1,
            ReconcileOutcome::Cleared => report.cleared += 1,
        }
    }
    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileOutcome {
    Unchanged,
    Recovered,
    Cleared,
}

async fn reconcile_reference(
    library: &Library,
    roots: &SharedRootResolver,
    reference: ArtworkReference,
) -> sqlx::Result<ReconcileOutcome> {
    if exists(roots, &reference.relative_path).await {
        return Ok(ReconcileOutcome::Unchanged);
    }

    let target = absolute_path(roots, &reference.relative_path);
    for historical in library
        .historical_artwork_paths(&reference.entry_key, reference.kind)
        .await?
    {
        if historical == reference.relative_path || !exists(roots, &historical).await {
            continue;
        }
        let source = absolute_path(roots, &historical);
        if let Some(parent) = target.parent() {
            if tokio::fs::create_dir_all(parent).await.is_err() {
                continue;
            }
        }
        // Copy rather than rename: album artwork may be shared by many rows,
        // and another still-valid entry can legitimately retain the old path.
        if tokio::fs::copy(&source, &target).await.is_ok() {
            library.bump_artwork_version(&reference.entry_key).await?;
            return Ok(ReconcileOutcome::Recovered);
        }
    }

    if library
        .clear_artwork_if_path(
            &reference.entry_key,
            reference.kind,
            &reference.relative_path,
        )
        .await?
    {
        Ok(ReconcileOutcome::Cleared)
    } else {
        Ok(ReconcileOutcome::Unchanged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_safe_characters() {
        assert_eq!(sanitize_stem("Inception (2010)"), "Inception (2010)");
        assert_eq!(sanitize_stem("The:Matrix/Reloaded"), "The_Matrix_Reloaded");
        assert_eq!(sanitize_stem(""), "untitled");
    }

    #[test]
    fn file_stem_strips_directory_and_extension() {
        assert_eq!(file_stem("movies/Foo (2020)/Foo.2020.mkv"), "Foo.2020");
        assert_eq!(file_stem("track.flac"), "track");
    }

    #[tokio::test]
    async fn saves_into_sibling_images_folder() {
        let root = std::env::temp_dir().join(format!("swarm-artwork-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("movies/Foo (2020)")).unwrap();
        let roots = SharedRootResolver::new(crate::roots::RootResolver::single(root.clone()));
        let relative = save_artwork(
            &roots,
            "movies/Foo (2020)/Foo.2020.mkv",
            "foo-tmdb-poster.jpg",
            b"bytes",
        )
        .await
        .unwrap();
        assert_eq!(relative, "movies/Foo (2020)/images/foo-tmdb-poster.jpg");
        assert_eq!(std::fs::read(root.join(&relative)).unwrap(), b"bytes");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn multi_root_artwork_is_written_under_the_owning_root_and_relabeled() {
        let base =
            std::env::temp_dir().join(format!("swarm-artwork-multiroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let nas_root = base.join("nas");
        std::fs::create_dir_all(nas_root.join("movies/Foo (2020)")).unwrap();
        let roots = SharedRootResolver::new(crate::roots::RootResolver::new(vec![
            crate::roots::MediaRoot {
                label: "local".into(),
                path: base.join("local"),
                asset_type: Default::default(),
            },
            crate::roots::MediaRoot {
                label: "nas".into(),
                path: nas_root.clone(),
                asset_type: Default::default(),
            },
        ]));
        let relative = save_artwork(
            &roots,
            "nas/movies/Foo (2020)/Foo.2020.mkv",
            "foo-tmdb-poster.jpg",
            b"bytes",
        )
        .await
        .unwrap();
        assert_eq!(relative, "nas/movies/Foo (2020)/images/foo-tmdb-poster.jpg");
        assert_eq!(
            std::fs::read(nas_root.join("movies/Foo (2020)/images/foo-tmdb-poster.jpg")).unwrap(),
            b"bytes"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn exists_finds_artwork_in_a_unicode_normalized_directory() {
        let root =
            std::env::temp_dir().join(format!("swarm-artwork-unicode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let actual = root.join("music/Cafe\u{301}/images/album-cover.jpg");
        std::fs::create_dir_all(actual.parent().unwrap()).unwrap();
        std::fs::write(&actual, b"cover").unwrap();

        let roots = SharedRootResolver::new(crate::roots::RootResolver::single(root.clone()));
        assert!(exists(&roots, "music/Café/images/album-cover.jpg").await);

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn reconcile_recovers_artwork_left_behind_by_a_media_move() {
        let base = std::env::temp_dir().join(format!(
            "swarm-artwork-reconcile-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("media");
        let old_dir = root.join("Old Movie");
        let new_dir = root.join("New Movie");
        std::fs::create_dir_all(old_dir.join("images")).unwrap();
        std::fs::write(old_dir.join("Movie.mkv"), vec![7u8; 32]).unwrap();
        std::fs::write(
            old_dir.join("images/Movie-tmdb-poster.jpg"),
            b"poster bytes",
        )
        .unwrap();

        let library = Library::open(base.join("library.sqlite").to_str().unwrap())
            .await
            .unwrap();
        crate::scan::scan_root(&library, &root).await.unwrap();
        let original = library.list().await.unwrap().remove(0);
        library
            .set_artwork(
                &original.entry_key,
                crate::store::ArtworkKind::Poster,
                "Old Movie/images/Movie-tmdb-poster.jpg",
            )
            .await
            .unwrap();

        // Reproduce the historical bug: only the media file moved. The next
        // scan restores metadata and remaps its artwork path to New Movie,
        // even though the image bytes still live under Old Movie.
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::rename(old_dir.join("Movie.mkv"), new_dir.join("Movie.mkv")).unwrap();
        crate::scan::scan_root(&library, &root).await.unwrap();
        let moved = library.list().await.unwrap().remove(0);
        let (remapped, _) = library
            .artwork(&moved.entry_key, crate::store::ArtworkKind::Poster)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remapped, "New Movie/images/Movie-tmdb-poster.jpg");
        assert!(!new_dir.join("images/Movie-tmdb-poster.jpg").exists());

        let roots = SharedRootResolver::new(crate::roots::RootResolver::single(root));
        let report = reconcile_references(&library, &roots).await.unwrap();
        assert_eq!(report.recovered, 1);
        assert_eq!(report.cleared, 0);
        assert_eq!(
            std::fs::read(new_dir.join("images/Movie-tmdb-poster.jpg")).unwrap(),
            b"poster bytes"
        );

        // If neither the current nor historical file exists, reconciliation
        // clears the stale value so the normal missing-only scraper retries it.
        std::fs::remove_file(new_dir.join("images/Movie-tmdb-poster.jpg")).unwrap();
        std::fs::remove_file(old_dir.join("images/Movie-tmdb-poster.jpg")).unwrap();
        let report = reconcile_references(&library, &roots).await.unwrap();
        assert_eq!(report.recovered, 0);
        assert_eq!(report.cleared, 1);
        assert!(
            library
                .artwork(&moved.entry_key, crate::store::ArtworkKind::Poster)
                .await
                .unwrap()
                .is_none()
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
