//! Multiple named library roots — lets a server point at more than one
//! filesystem location (e.g. a local drive plus a mounted NAS share) while
//! keeping `entry_key` (which hashes `relative_path` alone, see
//! `swarm_core::entry_key`) collision-free across roots: two roots
//! containing the same sub-path would otherwise produce the same key.
//!
//! Single-root installs — the overwhelmingly common case — get
//! byte-identical `relative_path`/`entry_key` values whether this module
//! exists or not: no `{label}/` prefix is ever applied unless 2+ roots are
//! configured. This is a deliberate, permanent asymmetry rather than a
//! migration shim. Multi-root support has never shipped, so there is no
//! installed base whose paths need to stay stable across a 1→2 transition —
//! nothing is gained by forcing a prefix onto the single-root case just to
//! make a future transition uniform.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Declared contents of a media root. `Mixed` exists only for settings
/// written before roots required a type; every newly-added root should use
/// one of the concrete variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaRootAssetType {
    #[default]
    Mixed,
    Movies,
    Shows,
    Music,
    PhotosVideos,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRoot {
    pub label: String,
    pub path: PathBuf,
    pub asset_type: MediaRootAssetType,
}

/// Resolves a stored `relative_path` (as written by `scan::scan_roots`) back
/// to an absolute filesystem path, and vice versa. Single source of truth
/// for the "1 root → no prefix, 2+ roots → `{label}/` prefix" convention, so
/// scanning, serving, and artwork-writing can't drift out of sync on it.
#[derive(Debug, Clone)]
pub struct RootResolver {
    roots: Vec<MediaRoot>,
}

impl RootResolver {
    /// # Panics
    /// If `roots` is empty — at least one root is always required.
    pub fn new(roots: Vec<MediaRoot>) -> Self {
        assert!(
            !roots.is_empty(),
            "RootResolver requires at least one media root"
        );
        Self { roots }
    }

    pub fn single(path: PathBuf) -> Self {
        Self {
            roots: vec![MediaRoot {
                label: "local".to_string(),
                path,
                asset_type: MediaRootAssetType::Mixed,
            }],
        }
    }

    pub fn roots(&self) -> &[MediaRoot] {
        &self.roots
    }

    fn multi(&self) -> bool {
        self.roots.len() > 1
    }

    /// Absolute filesystem path for a stored `relative_path`.
    pub fn resolve(&self, relative_path: &str) -> PathBuf {
        let (root, rest) = self.split(relative_path);
        root.join(rest)
    }

    /// (absolute root directory, path under that root) for a stored
    /// `relative_path`. Falls back to the first configured root when the
    /// path carries no recognized `{label}/` prefix (always true in the
    /// single-root case, and a safe degrade for an unrecognized label —
    /// callers resolving a filesystem path from the result simply fail to
    /// find the file rather than panicking).
    pub fn split(&self, relative_path: &str) -> (PathBuf, String) {
        if self.multi() {
            if let Some((label, rest)) = relative_path.split_once('/') {
                if let Some(root) = self.roots.iter().find(|r| r.label == label) {
                    return (root.path.clone(), rest.to_string());
                }
            }
        }
        let root = self
            .roots
            .first()
            .map(|r| r.path.clone())
            .unwrap_or_default();
        (root, relative_path.to_string())
    }

    /// The label that owns a stored `relative_path` — used to round-trip
    /// [`Self::compose`] after resolving a path back out with [`Self::split`]
    /// (e.g. writing a new artwork file alongside an already-scanned entry).
    pub fn label_for(&self, relative_path: &str) -> String {
        if self.multi() {
            if let Some((label, _)) = relative_path.split_once('/') {
                if self.roots.iter().any(|r| r.label == label) {
                    return label.to_string();
                }
            }
        }
        self.roots
            .first()
            .map(|r| r.label.clone())
            .unwrap_or_default()
    }

    /// Build a stored `relative_path` from a root's label and a path under
    /// that root — the inverse of [`Self::split`].
    pub fn compose(&self, label: &str, path_under_root: &str) -> String {
        if self.multi() {
            format!("{label}/{path_under_root}")
        } else {
            path_under_root.to_string()
        }
    }
}

/// A [`RootResolver`] behind a shared, swappable handle. `ServerCore` and
/// `MediaService` each hold a clone of the same handle (cheap — just an
/// `Arc` bump), so changing the configured roots via
/// [`SharedRootResolver::replace`] takes effect for scanning, scraping, and
/// P2P serving/artwork all at once, with no restart and no way for the two
/// to drift onto different root sets.
#[derive(Clone)]
pub struct SharedRootResolver {
    inner: Arc<RwLock<RootResolver>>,
}

impl SharedRootResolver {
    pub fn new(resolver: RootResolver) -> Self {
        Self {
            inner: Arc::new(RwLock::new(resolver)),
        }
    }

    /// Atomically swap in a fresh set of roots. Every clone of this handle
    /// observes the change on its very next call — see the type doc.
    ///
    /// # Panics
    /// If `roots` is empty (see [`RootResolver::new`]) — callers reachable
    /// from user input (e.g. a Tauri command) must validate non-emptiness
    /// themselves before calling this.
    pub fn replace(&self, roots: Vec<MediaRoot>) {
        *self.inner.write().unwrap() = RootResolver::new(roots);
    }

    pub fn resolve(&self, relative_path: &str) -> PathBuf {
        self.inner.read().unwrap().resolve(relative_path)
    }

    pub fn split(&self, relative_path: &str) -> (PathBuf, String) {
        self.inner.read().unwrap().split(relative_path)
    }

    pub fn label_for(&self, relative_path: &str) -> String {
        self.inner.read().unwrap().label_for(relative_path)
    }

    pub fn compose(&self, label: &str, path_under_root: &str) -> String {
        self.inner.read().unwrap().compose(label, path_under_root)
    }

    /// Point-in-time copy of the configured roots. Owned (not a borrow, unlike
    /// [`RootResolver::roots`]) since it must outlive the read-lock guard.
    pub fn roots(&self) -> Vec<MediaRoot> {
        self.inner.read().unwrap().roots().to_vec()
    }
}

/// True if a recursive directory walk rooted at `container` visits every
/// file under `candidate` too — either the same location, or `candidate`
/// nested somewhere inside it (`Path::starts_with` is component-wise, so a
/// merely-shared string prefix like `"tv"` vs. `"tv2"` never false-positives).
///
/// Compares canonicalized paths when possible, so two different mount
/// points for the same underlying share (symlinks, bind mounts, an SMB
/// share mounted both directly and via a parent folder) still resolve to
/// the same real location. Falls back to the configured path as-is when
/// canonicalization fails (e.g. a network root that isn't currently
/// mounted) — an unreachable root must not be treated as automatically
/// distinct just because it can't be resolved right now.
pub fn path_contains(container: &Path, candidate: &Path) -> bool {
    let container = std::fs::canonicalize(container).unwrap_or_else(|_| container.to_path_buf());
    let candidate = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    candidate.starts_with(&container)
}

/// True if `a` and `b` name the same filesystem location or one is nested
/// inside the other — scanning both as separate roots would catalog the
/// same physical files twice, once per root's `{label}/` prefix (see this
/// module's doc comment), silently duplicating every affected show/movie/
/// track in the catalog even though there is exactly one copy of each file
/// on disk. Real trigger: a NAS share (or its subfolder) mounted and added
/// as a second root alongside a root that already contains it — e.g. a
/// dedicated mount for one show's folder added on top of an existing
/// umbrella "TV Shows" root.
pub fn paths_overlap(a: &Path, b: &Path) -> bool {
    path_contains(a, b) || path_contains(b, a)
}

/// The first pair of configured `roots` whose filesystem locations overlap
/// (see [`paths_overlap`]), if any — checked pairwise, which is fine for the
/// handful of roots a real install ever configures.
pub fn find_overlapping_roots(roots: &[MediaRoot]) -> Option<(&MediaRoot, &MediaRoot)> {
    for (i, a) in roots.iter().enumerate() {
        for b in &roots[i + 1..] {
            if paths_overlap(&a.path, &b.path) {
                return Some((a, b));
            }
        }
    }
    None
}

/// Parse `SWARM_MEDIA_ROOTS`'s `label=path,label2=path2` format.
pub fn parse_roots_env(value: &str) -> Vec<MediaRoot> {
    value
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let (label, path) = entry.split_once('=')?;
            let label = label.trim();
            let path = path.trim();
            if label.is_empty() || path.is_empty() {
                return None;
            }
            Some(MediaRoot {
                label: label.to_string(),
                path: PathBuf::from(path),
                asset_type: MediaRootAssetType::Mixed,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_roots() -> RootResolver {
        RootResolver::new(vec![
            MediaRoot {
                label: "local".into(),
                path: PathBuf::from("/media"),
                asset_type: MediaRootAssetType::Mixed,
            },
            MediaRoot {
                label: "nas".into(),
                path: PathBuf::from("/Volumes/nas"),
                asset_type: MediaRootAssetType::Mixed,
            },
        ])
    }

    #[test]
    fn single_root_applies_no_prefix() {
        let r = RootResolver::single(PathBuf::from("/media"));
        assert_eq!(
            r.resolve("movies/Foo.mkv"),
            PathBuf::from("/media/movies/Foo.mkv")
        );
        assert_eq!(r.compose("local", "movies/Foo.mkv"), "movies/Foo.mkv");
        assert_eq!(r.label_for("movies/Foo.mkv"), "local");
    }

    #[test]
    fn multi_root_resolves_by_label_prefix() {
        let r = two_roots();
        assert_eq!(
            r.resolve("nas/movies/Foo.mkv"),
            PathBuf::from("/Volumes/nas/movies/Foo.mkv")
        );
        assert_eq!(
            r.resolve("local/movies/Foo.mkv"),
            PathBuf::from("/media/movies/Foo.mkv")
        );
        assert_eq!(r.label_for("nas/movies/Foo.mkv"), "nas");
    }

    #[test]
    fn multi_root_compose_round_trips_through_split() {
        let r = two_roots();
        let stored = r.compose("nas", "movies/Foo.mkv");
        assert_eq!(stored, "nas/movies/Foo.mkv");
        let (root, rest) = r.split(&stored);
        assert_eq!(root, PathBuf::from("/Volumes/nas"));
        assert_eq!(rest, "movies/Foo.mkv");
    }

    #[test]
    fn parses_label_equals_path_pairs() {
        let roots = parse_roots_env("local=/media,nas=/Volumes/nas");
        assert_eq!(
            roots,
            vec![
                MediaRoot {
                    label: "local".into(),
                    path: PathBuf::from("/media"),
                    asset_type: MediaRootAssetType::Mixed,
                },
                MediaRoot {
                    label: "nas".into(),
                    path: PathBuf::from("/Volumes/nas"),
                    asset_type: MediaRootAssetType::Mixed,
                },
            ]
        );
    }

    #[test]
    fn parse_roots_env_skips_malformed_entries() {
        assert_eq!(parse_roots_env(""), vec![]);
        assert_eq!(parse_roots_env("no-equals-sign"), vec![]);
        assert_eq!(
            parse_roots_env("=novalue,label=,ok=/path"),
            vec![MediaRoot {
                label: "ok".into(),
                path: PathBuf::from("/path"),
                asset_type: MediaRootAssetType::Mixed,
            }]
        );
    }

    #[test]
    fn overlapping_roots_detects_identical_and_nested_paths() {
        assert!(paths_overlap(
            Path::new("/media/tv/office-nonexistent"),
            Path::new("/media/tv/office-nonexistent")
        ));
        assert!(paths_overlap(
            Path::new("/media/tv-nonexistent"),
            Path::new("/media/tv-nonexistent/office")
        ));
        assert!(paths_overlap(
            Path::new("/media/tv-nonexistent/office"),
            Path::new("/media/tv-nonexistent")
        ));
        assert!(!paths_overlap(
            Path::new("/media/tv-nonexistent"),
            Path::new("/media/music-nonexistent")
        ));
        // A shared prefix that isn't a real path-component boundary must not
        // false-positive (e.g. "/media/tv" vs "/media/tv2").
        assert!(!paths_overlap(
            Path::new("/media/tv-nonexistent"),
            Path::new("/media/tv-nonexistent2")
        ));
    }

    #[test]
    fn find_overlapping_roots_reports_the_first_conflicting_pair() {
        let roots = vec![
            MediaRoot {
                label: "local".into(),
                path: PathBuf::from("/media/tv-nonexistent"),
                asset_type: MediaRootAssetType::Mixed,
            },
            MediaRoot {
                label: "nas".into(),
                path: PathBuf::from("/Volumes/nas-nonexistent"),
                asset_type: MediaRootAssetType::Mixed,
            },
            MediaRoot {
                label: "office".into(),
                path: PathBuf::from("/media/tv-nonexistent/The Office"),
                asset_type: MediaRootAssetType::Mixed,
            },
        ];
        let (a, b) = find_overlapping_roots(&roots).expect("overlap expected");
        assert_eq!(a.label, "local");
        assert_eq!(b.label, "office");
    }

    #[test]
    fn find_overlapping_roots_is_none_for_disjoint_roots() {
        let roots = vec![
            MediaRoot {
                label: "local".into(),
                path: PathBuf::from("/media/tv-nonexistent"),
                asset_type: MediaRootAssetType::Mixed,
            },
            MediaRoot {
                label: "nas".into(),
                path: PathBuf::from("/Volumes/nas-nonexistent"),
                asset_type: MediaRootAssetType::Mixed,
            },
        ];
        assert!(find_overlapping_roots(&roots).is_none());
    }

    #[test]
    fn shared_resolver_replace_is_visible_on_every_clone() {
        let shared = SharedRootResolver::new(RootResolver::single(PathBuf::from("/old")));
        let other_handle = shared.clone();
        assert_eq!(
            shared.resolve("movies/Foo.mkv"),
            PathBuf::from("/old/movies/Foo.mkv")
        );

        shared.replace(vec![MediaRoot {
            label: "nas".into(),
            path: PathBuf::from("/Volumes/nas"),
            asset_type: MediaRootAssetType::Mixed,
        }]);

        // Both handles observe the swap — this is the whole point of the
        // shared Arc<RwLock<..>>: ServerCore and MediaService must never see
        // different root sets after a live update.
        assert_eq!(
            shared.resolve("movies/Foo.mkv"),
            PathBuf::from("/Volumes/nas/movies/Foo.mkv")
        );
        assert_eq!(
            other_handle.resolve("movies/Foo.mkv"),
            PathBuf::from("/Volumes/nas/movies/Foo.mkv")
        );
        assert_eq!(
            shared.roots(),
            vec![MediaRoot {
                label: "nas".into(),
                path: PathBuf::from("/Volumes/nas"),
                asset_type: MediaRootAssetType::Mixed,
            }]
        );
    }
}
