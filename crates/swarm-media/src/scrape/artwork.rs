//! Save downloaded artwork bytes to disk, following the Drone convention:
//! images live in an `images/` subfolder beside the source file(s), not in
//! the library database. Movies get a per-file name (two files in the same
//! folder, e.g. `S01E01.mkv`/`S01E02.mkv`, must not collide); music uses
//! fixed names shared by every track in the album folder.

use crate::roots::SharedRootResolver;

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
    let (root_path, rest) = roots.split(relative_path);
    let source_path = root_path.join(rest.replace('/', std::path::MAIN_SEPARATOR_STR));
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
}
