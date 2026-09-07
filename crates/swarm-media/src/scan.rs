//! Library scanning: allowlist filesystem snapshot → path/(size, mtime) diff
//! → fingerprint/tag/probe only for additions and changes → reconciliation
//! with pending-change tracking. Comprehensive Check additionally verifies
//! otherwise-unchanged files sequentially with the existing fingerprint.
//! A rename is a delete of the old path plus an add of the new one (entry
//! keys are path-derived by design).

use crate::roots::{MediaRoot, MediaRootAssetType};
use crate::scrape::artwork;
use crate::store::{ArtworkKind, EntryRecord, Library, MissingDisposition, ScanManifestEntry};
use crate::{classify, probe, tags};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use swarm_core::peer::MediaKind;
use swarm_core::{entry_key, fingerprint};
use tokio::sync::mpsc::Sender;

pub(crate) const MISSING_CONFIRMATION_GRACE_MS: i64 = 60 * 60 * 1_000;

/// User-selectable scan behavior. The fast path is always performed first;
/// comprehensive verification only adds sequential content checks for files
/// whose path, size, and timestamp otherwise match the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanOptions {
    pub comprehensive_check: bool,
    pub scan_music_tracks: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            comprehensive_check: false,
            // Library-crate callers predate the desktop preference and keep
            // their historical behavior. The desktop explicitly passes its
            // persisted default (`false`).
            scan_music_tracks: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    pub unchanged: u64,
    /// Labels of roots whose directory walk returned an incomplete snapshot
    /// this pass (a continuously-changing network share — see
    /// [`crate::scan`]'s handling below). Their manifest entries are staged
    /// but deliberately *not* applied to the catalog, so an incomplete pass
    /// never mutates the served catalog / thumbprint. The auto library watch
    /// (`apps/server/src/gui.rs`) uses this to back a flapping root off an
    /// exponentially longer rescan interval instead of re-walking it every
    /// cycle.
    pub incomplete_roots: Vec<String>,
    /// Deterministic Plex-conformance problems found while scanning (issue
    /// #247): files that match no supported Plex layout, or match one only
    /// partially (an episode with no `Season NN` folder, a movie with no
    /// derivable title, …). Best-effort and bounded at
    /// [`MAX_VALIDATION_ISSUES`]; the full library-wide validator is the
    /// reorganize planner (`apps/server/src/reorganize.rs`). Empty when
    /// every scanned file conforms. The AI helper consumes these as ground
    /// truth for automatic Plex-compatible repair.
    pub validation_issues: Vec<crate::plex::PlexValidationIssue>,
}

/// Ceiling on how many [`crate::plex::PlexValidationIssue`]s one scan
/// collects, so a pathologically messy library can't grow the report
/// unboundedly. The reorganize planner reports the complete set.
pub const MAX_VALIDATION_ISSUES: usize = 500;

/// Live progress for [`scan_roots`], in two phases matching its own two
/// passes: `Discovering` while the directory walk across every root is
/// still finding files (no total is knowable yet — real bug, found live:
/// this walk alone can take minutes over a slow network mount (SMB/NFS)
/// with thousands of files, and originally reported nothing at all, so a
/// rescan looked hung the entire time), then `Processing` once that walk
/// finishes and the total is fixed, ticking as each file's fingerprint/
/// probe work completes (`processed` counts every discovered file as it's
/// visited — added/updated/unchanged/skipped all tick the same way — so
/// "X of Y" reflects real linear progress regardless of outcome).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ScanProgressEvent {
    Discovering { found: u64 },
    Processing { processed: u64, total: u64 },
}

/// Optional live progress side-channel for [`scan_roots`], mirroring
/// `scrape::runner::ScrapeProgress` — a bounded `mpsc` sender, not a Tauri
/// type, so this crate stays usable by the headless daemon with zero UI
/// dependency. Intermediate frames may be dropped when the UI falls behind;
/// progress reporting must never grow memory or stall the scan itself.
struct ScanProgress {
    sender: Sender<ScanProgressEvent>,
    found: AtomicU64,
    processed: AtomicU64,
}

impl ScanProgress {
    fn new(sender: Sender<ScanProgressEvent>) -> Self {
        Self {
            sender,
            found: AtomicU64::new(0),
            processed: AtomicU64::new(0),
        }
    }

    /// Called once per file as the directory walk discovers it, before the
    /// total is known.
    fn tick_discovering(&self) {
        let found = self.found.fetch_add(1, Ordering::Relaxed) + 1;
        // Progress is advisory. Never let a slow webview accumulate one
        // queued allocation per file or stall the filesystem walk.
        let _ = self
            .sender
            .try_send(ScanProgressEvent::Discovering { found });
    }

    /// Called once per file as the second pass (fingerprint/probe/store)
    /// visits it, now that `total` (the walk's final file count) is fixed.
    fn tick_processing(&self, total: u64) {
        let processed = self.processed.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self
            .sender
            .try_send(ScanProgressEvent::Processing { processed, total });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("at least one media root is required")]
    NoMediaRoots,
    #[error("I/O error while scanning media root \"{label}\" at {path}: {source}")]
    RootIo {
        label: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("library store error: {0}")]
    Store(#[from] sqlx::Error),
    #[error("scan cancelled")]
    Cancelled,
    #[error(
        "found 0 files across every configured root, but the library already has {0} known entries — refusing to \
         treat this as \"everything was deleted\" (a dropped network mount looks identical to an empty root). \
         Check that every media root is actually reachable, then rescan again."
    )]
    SuspiciousEmptyScan(usize),
}

/// Walk a single `root` and reconcile the library with what is on disk.
/// Thin wrapper over [`scan_roots`] for the overwhelmingly common
/// single-root case — see that function and `crate::roots` for why a
/// single root never gets a `{label}/` prefix on `relative_path`.
pub async fn scan_root(library: &Library, root: &Path) -> Result<ScanReport, ScanError> {
    scan_roots(
        library,
        &[MediaRoot {
            label: "local".to_string(),
            path: root.to_path_buf(),
            asset_type: MediaRootAssetType::Mixed,
        }],
        None,
    )
    .await
}

/// Walk every configured root and reconcile the library with what is on
/// disk. With 2+ roots, each root's files are stored under a
/// `{label}/`-prefixed `relative_path` (see `crate::roots::RootResolver`) so
/// two roots containing the same sub-path don't collide on `entry_key`; with
/// exactly one root, no prefix is applied and behavior is byte-identical to
/// [`scan_root`]. `progress_tx`, when given, receives best-effort
/// [`ScanProgressEvent`] updates during both phases (directory walk, then
/// per-file reconciliation) — see [`ScanProgress`]'s doc comment.
pub async fn scan_roots(
    library: &Library,
    roots: &[MediaRoot],
    progress_tx: Option<Sender<ScanProgressEvent>>,
) -> Result<ScanReport, ScanError> {
    scan_roots_with_options(library, roots, progress_tx, ScanOptions::default()).await
}

pub async fn scan_roots_with_options(
    library: &Library,
    roots: &[MediaRoot],
    progress_tx: Option<Sender<ScanProgressEvent>>,
    options: ScanOptions,
) -> Result<ScanReport, ScanError> {
    scan_roots_scoped_inner_entry(
        library,
        roots,
        roots.len() > 1,
        progress_tx,
        None,
        options,
    )
    .await
}

/// Cancellable counterpart to [`scan_roots`]. Cancellation is checked while
/// walking directories and between files during catalog processing. A scan
/// cancelled before reconciliation never marks absent entries unavailable.
pub async fn scan_roots_cancellable(
    library: &Library,
    roots: &[MediaRoot],
    progress_tx: Option<Sender<ScanProgressEvent>>,
    cancel: Arc<AtomicBool>,
) -> Result<ScanReport, ScanError> {
    scan_roots_cancellable_with_options(
        library,
        roots,
        progress_tx,
        cancel,
        ScanOptions::default(),
    )
    .await
}

pub async fn scan_roots_cancellable_with_options(
    library: &Library,
    roots: &[MediaRoot],
    progress_tx: Option<Sender<ScanProgressEvent>>,
    cancel: Arc<AtomicBool>,
    options: ScanOptions,
) -> Result<ScanReport, ScanError> {
    scan_roots_scoped_inner_entry(
        library,
        roots,
        roots.len() > 1,
        progress_tx,
        Some(cancel),
        options,
    )
    .await
}

/// Reconcile only `roots` while preserving the path namespace of the full
/// configured root set. When `multi_root_namespace` is true, paths retain
/// their `{label}/` prefix and deletion reconciliation is restricted to the
/// supplied labels. This is used after one network root recovers so unrelated
/// local or network roots do not have to be walked again.
pub async fn scan_roots_scoped(
    library: &Library,
    roots: &[MediaRoot],
    multi_root_namespace: bool,
    progress_tx: Option<Sender<ScanProgressEvent>>,
) -> Result<ScanReport, ScanError> {
    scan_roots_scoped_with_options(
        library,
        roots,
        multi_root_namespace,
        progress_tx,
        ScanOptions::default(),
    )
    .await
}

pub async fn scan_roots_scoped_with_options(
    library: &Library,
    roots: &[MediaRoot],
    multi_root_namespace: bool,
    progress_tx: Option<Sender<ScanProgressEvent>>,
    options: ScanOptions,
) -> Result<ScanReport, ScanError> {
    scan_roots_scoped_inner_entry(
        library,
        roots,
        multi_root_namespace,
        progress_tx,
        None,
        options,
    )
    .await
}

async fn scan_roots_scoped_inner_entry(
    library: &Library,
    roots: &[MediaRoot],
    multi_root_namespace: bool,
    progress_tx: Option<Sender<ScanProgressEvent>>,
    cancel: Option<Arc<AtomicBool>>,
    options: ScanOptions,
) -> Result<ScanReport, ScanError> {
    if roots.is_empty() {
        return Err(ScanError::NoMediaRoots);
    }
    let scan_id = library.begin_scan_manifest().await?;
    let result = scan_roots_scoped_inner(
        library,
        roots,
        multi_root_namespace,
        progress_tx,
        cancel,
        options,
        &scan_id,
    )
    .await;
    let cleanup = library.clear_scan_manifest(&scan_id).await;
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(report), Ok(())) => Ok(report),
    }
}

async fn scan_roots_scoped_inner(
    library: &Library,
    roots: &[MediaRoot],
    multi_root_namespace: bool,
    progress_tx: Option<Sender<ScanProgressEvent>>,
    cancel: Option<Arc<AtomicBool>>,
    options: ScanOptions,
    scan_id: &str,
) -> Result<ScanReport, ScanError> {
    let mut report = ScanReport::default();
    let mut artwork_cache = ArtworkCache::default();
    // Arc, not a bare ScanProgress: discover_media_files below hands a clone
    // across a spawn_blocking boundary, which needs 'static + Send ownership,
    // not a borrow tied to this function's stack frame.
    let progress = progress_tx.map(|tx| Arc::new(ScanProgress::new(tx)));

    // A misconfigured pair of roots (one nested inside, or identical to,
    // another — e.g. a dedicated mount added for one show's folder on top of
    // an existing umbrella root that already contains it) would otherwise
    // walk the same physical files twice, once per root's `{label}/` prefix,
    // silently duplicating every entry under the overlap even though there
    // is exactly one copy of each file on disk. An ancestor root's own
    // recursive walk already visits everything under a nested root, so the
    // nested one is dropped unconditionally — regardless of configuration
    // order, since skipping the *ancestor* instead would silently stop
    // scanning everything else under it, not just the duplicate-causing
    // overlap. Two roots at the exact same location (neither a genuine
    // ancestor of the other) tie-break on configuration order. Dropped roots
    // still participate in this scan's deletion reconciliation below (see
    // `redundant_roots`) so any already-duplicated entries under their
    // prefix, from before the overlap was recognized, self-heal on this very
    // scan instead of lingering forever.
    let mut walked_roots: Vec<&MediaRoot> = Vec::with_capacity(roots.len());
    let mut redundant_roots: Vec<&MediaRoot> = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        let redundant = roots.iter().enumerate().any(|(other_index, other)| {
            if other_index == index || !crate::roots::path_contains(&other.path, &root.path) {
                return false;
            }
            // `other` and `root` are the exact same location, not a genuine
            // ancestor/descendant pair: keep only the earlier-configured one.
            if crate::roots::path_contains(&root.path, &other.path) {
                other_index < index
            } else {
                true
            }
        });
        if redundant {
            tracing::warn!(
                root = %root.label,
                path = %root.path.display(),
                "media root overlaps with another configured root; skipping its scan and \
                 reconciling any entries already catalogued under it as missing"
            );
            redundant_roots.push(root);
        } else {
            walked_roots.push(root);
        }
    }

    // Finish every root walk before catalog mutation, but stage the results
    // in local SQLite rather than growing an in-memory Vec with the library.
    let mut complete_roots = Vec::with_capacity(walked_roots.len());
    let mut incomplete_roots: Vec<String> = Vec::new();
    let mut subtitle_sidecars: Vec<SubtitleSidecar> = Vec::new();
    for root in &walked_roots {
        check_cancelled(cancel.as_deref())?;
        let (complete, sidecars) = discover_media_files(
            library,
            scan_id,
            root,
            multi_root_namespace,
            progress.as_ref(),
            cancel.as_ref(),
            options,
        )
        .await?;
        let remaining = MAX_SUBTITLE_SIDECARS.saturating_sub(subtitle_sidecars.len());
        subtitle_sidecars.extend(sidecars.into_iter().take(remaining));
        if complete {
            complete_roots.push(*root);
        } else {
            // A changing directory is not a complete snapshot. It must not
            // reconcile deletions (absent rows stay available), and its
            // partial adds/updates must not be applied either: a root that
            // keeps coming back incomplete every pass (a network share
            // written to continuously) would otherwise move the catalog
            // thumbprint on every cycle and force every client into a full
            // manifest reload. Its manifest entries are staged but skipped
            // during catalog processing below; a later complete pass applies
            // the real state in one go.
            incomplete_roots.push(root.label.clone());
            tracing::warn!(
                root = %root.label,
                path = %root.path.display(),
                "media root changed during scan; skipping catalog reconciliation for this root"
            );
        }
    }
    // Never walked, but still fully reconciled: any entry already
    // catalogued under a redundant root's `{label}/` prefix is guaranteed
    // absent from this scan's manifest (that root was never walked) and so
    // is correctly marked missing below — self-healing pre-existing
    // duplicates from before the overlap was recognized.
    complete_roots.extend(redundant_roots.iter().copied());
    let total = library.scan_manifest_count(scan_id).await?;
    let known_in_scope = if multi_root_namespace {
        let mut count = 0usize;
        for root in roots {
            count = count.saturating_add(
                library
                    .entry_count_with_prefix(Some(&format!("{}/", root.label)))
                    .await?,
            );
        }
        count
    } else {
        library.entry_count_with_prefix(None).await?
    };

    let intentionally_empty_music_scope = !options.scan_music_tracks
        && roots
            .iter()
            .all(|root| root.asset_type == MediaRootAssetType::Music);
    if total == 0 && known_in_scope > 0 && !intentionally_empty_music_scope {
        return Err(ScanError::SuspiciousEmptyScan(known_in_scope));
    }

    let prefixes = if multi_root_namespace {
        complete_roots
            .iter()
            .map(|root| Some(format!("{}/", root.label)))
            .collect::<Vec<_>>()
    } else if complete_roots.is_empty() {
        Vec::new()
    } else {
        vec![None]
    };

    // Catalog mutation (adds/updates just as much as deletions) is confined
    // to roots that produced a complete snapshot this pass. A staged
    // manifest entry under an incomplete root's `{label}/` prefix is skipped
    // entirely during processing below, so an incomplete pass leaves the
    // served catalog — and therefore the thumbprint — untouched.
    let entry_in_completed_scope =
        |relative_path: &str| path_in_completed_scope(relative_path, multi_root_namespace, &prefixes);

    let mut cursor = String::new();
    loop {
        check_cancelled(cancel.as_deref())?;
        let files = library.scan_manifest_page(scan_id, &cursor, 256).await?;
        if files.is_empty() {
            break;
        }
        for file in files {
            check_cancelled(cancel.as_deref())?;
            cursor.clone_from(&file.relative_path);
            let relative = file.relative_path.clone();
            let absolute = PathBuf::from(&file.absolute_path);
            if let Some(progress) = &progress {
                progress.tick_processing(total);
            }
            if !entry_in_completed_scope(&relative) {
                continue;
            }
            // Deterministic Plex-conformance check (issue #247). Pure string
            // work, no I/O — cheap even on a full rescan. Bounded so a very
            // messy library can't grow the report without limit.
            if report.validation_issues.len() < MAX_VALIDATION_ISSUES {
                let classified = classify::classify(&file.relative_under_root);
                if let Some(issue) = crate::plex::validate_media_file(
                    &file.relative_under_root,
                    classified.as_ref(),
                ) {
                    report.validation_issues.push(issue);
                }
            }
            let known = library.known_entry_by_path(&relative).await?;
            let mut comprehensive_fingerprint = None;
            if let Some(known_entry) = known.as_ref() {
                if known_entry.size == file.size && known_entry.modified_time == file.modified_time {
                    let content_matches = if options.comprehensive_check {
                        // This await is deliberately inside the ordered loop:
                        // comprehensive verification must never fan out disk
                        // reads across a media root.
                        let fp_path = absolute.clone();
                        let fingerprint = tokio::task::spawn_blocking(move || {
                            fingerprint::fingerprint_file(&fp_path)
                        })
                        .await
                        .expect("fingerprint task panicked");
                        let Ok(fingerprint) = fingerprint else {
                            continue;
                        };
                        let matches = fingerprint == known_entry.fingerprint;
                        comprehensive_fingerprint = Some(fingerprint);
                        matches
                    } else {
                        true
                    };
                    if content_matches {
                        if known_entry.available {
                            report.unchanged += 1;
                        } else if library.restore_available_by_path(&relative).await? {
                            report.added += 1;
                        }
                        if !known_entry.has_artwork {
                            if let Some(classified) = classify::classify(&file.relative_under_root) {
                                recover_existing_artwork(
                                    library,
                                    &absolute,
                                    &entry_key::entry_key(&relative),
                                    &relative,
                                    classified.kind,
                                    &mut artwork_cache,
                                )
                                .await?;
                            }
                        }
                        continue;
                    }
                }
            }

            let asset_type = asset_type_for_path(roots, &file.relative_path, multi_root_namespace);
            let Some(classified) = classify_for_asset_type(&file.relative_under_root, asset_type)
            else {
                continue;
            };
            // fingerprint_file/read_tags are synchronous std::fs I/O — each a
            // potential SMB/NFS round trip — and this task shares its tokio
            // worker thread with QUIC/HTTP request handling; spawn_blocking
            // keeps a slow network mount from stalling every other request
            // on the server for as long as this file takes. Only new/changed
            // files reach this line at all (the unchanged fast path above
            // already `continue`d), so this cost is paid exactly where it's
            // unavoidable, not on every file in a routine rescan.
            let fp = if let Some(fingerprint) = comprehensive_fingerprint {
                fingerprint
            } else {
                let fp_path = absolute.clone();
                let Ok(fingerprint) =
                    tokio::task::spawn_blocking(move || fingerprint::fingerprint_file(&fp_path))
                        .await
                        .expect("fingerprint task panicked")
                else {
                    continue;
                };
                fingerprint
            };
            let tags_path = absolute.clone();
            let tag = tokio::task::spawn_blocking(move || tags::read_tags(&tags_path))
                .await
                .expect("tag-read task panicked");
            let media = probe::probe(&absolute).await;
            check_cancelled(cancel.as_deref())?;
            let entry_key = entry_key::entry_key(&relative);
            let mut record = EntryRecord {
                entry_key: entry_key.clone(),
                relative_path: relative,
                kind: classified.kind,
                title: tag
                    .as_ref()
                    .and_then(|tag| tag.title.clone())
                    .unwrap_or(classified.title),
                size: file.size,
                modified_time: file.modified_time,
                fingerprint: fp,
                artist: tag
                    .as_ref()
                    .and_then(|tag| tag.artist.clone())
                    .or(classified.artist),
                album: tag
                    .as_ref()
                    .and_then(|tag| tag.album.clone())
                    .or(classified.album),
                track_number: tag
                    .as_ref()
                    .and_then(|tag| tag.track_number)
                    .or(classified.track_number),
                show_title: classified.show_title,
                season: classified.season,
                episode: classified.episode,
                year: classified.year,
                duration_secs: media.as_ref().and_then(|media| media.duration_secs),
                video: media.as_ref().and_then(|media| media.video.clone()),
                audio: media.as_ref().and_then(|media| media.audio.clone()),
                scraped_title: None,
                episode_title: None,
                genres: Vec::new(),
                artwork_version: 0,
                cast: Vec::new(),
                overview: None,
                rating: None,
                community_rating: None,
                community_rating_votes: None,
            };
            if known.as_ref().is_some_and(|entry| entry.kind_overridden) {
                if let Ok(Some(existing)) = library.get(&entry_key).await {
                    record.kind = existing.kind;
                    record.title = existing.title;
                    record.artist = existing.artist;
                    record.album = existing.album;
                    record.track_number = existing.track_number;
                    record.show_title = existing.show_title;
                    record.season = existing.season;
                    record.episode = existing.episode;
                }
            }
            library.upsert(&record).await?;
            if known.as_ref().is_some_and(|entry| entry.available) {
                library.mark_scrape_stale(&record.entry_key).await?;
            }
            if known.is_none() {
                library
                    .restore_archived_metadata(
                        &record.entry_key,
                        &record.fingerprint,
                        record.size,
                        &record.relative_path,
                    )
                    .await?;
            }
            let already_had_artwork = known.as_ref().is_some_and(|entry| entry.has_artwork);
            if known.as_ref().is_some_and(|entry| entry.available) {
                report.updated += 1;
            } else {
                report.added += 1;
            }
            if !already_had_artwork {
                recover_existing_artwork(
                    library,
                    &absolute,
                    &record.entry_key,
                    &record.relative_path,
                    record.kind,
                    &mut artwork_cache,
                )
                .await?;
            }
        }
    }

    // Only a fully processed manifest may change availability. Missing rows
    // remain as durable tombstones; the active list/catalog hides them while
    // later successful scans advance their confirmation counter.
    for prefix in &prefixes {
        let mut missing_cursor = String::new();
        loop {
            check_cancelled(cancel.as_deref())?;
            let missing = library
                .paths_missing_from_scan(scan_id, prefix.as_deref(), &missing_cursor, 256)
                .await?;
            if missing.is_empty() {
                break;
            }
            for path in missing {
                check_cancelled(cancel.as_deref())?;
                missing_cursor.clone_from(&path);
                if matches!(
                    library
                        .mark_missing_by_path(&path, MISSING_CONFIRMATION_GRACE_MS)
                        .await?,
                    Some(MissingDisposition::NewlyMissing)
                ) {
                    report.removed += 1;
                }
            }
        }
    }

    // Side-loaded subtitles: match each `.srt`/`.vtt` discovered during the
    // walk to its owning catalog entry and replace the previous external
    // subtitle set. Only completed roots participate — a partial snapshot
    // must not wipe a sidecar just because its directory was mid-change.
    reconcile_external_subtitles(
        library,
        &complete_roots,
        multi_root_namespace,
        &subtitle_sidecars,
    )
    .await?;

    report.incomplete_roots = incomplete_roots;
    Ok(report)
}

/// Match every discovered subtitle sidecar to a catalog entry and rewrite
/// the `source = "external"` rows for each completed scope. Best-effort per
/// file: an unmatched or ambiguous sidecar is simply not offered.
async fn reconcile_external_subtitles(
    library: &Library,
    complete_roots: &[&MediaRoot],
    multi_root_namespace: bool,
    sidecars: &[SubtitleSidecar],
) -> Result<(), ScanError> {
    if multi_root_namespace {
        for root in complete_roots {
            let label_prefix = format!("{}/", root.label);
            let scoped: Vec<&SubtitleSidecar> = sidecars
                .iter()
                .filter(|s| s.relative_path.starts_with(&label_prefix))
                .collect();
            let records = match_subtitle_records(library, &scoped).await?;
            library
                .replace_external_subtitles(Some(&root.label), &records)
                .await?;
        }
    } else if !complete_roots.is_empty() {
        let scoped: Vec<&SubtitleSidecar> = sidecars.iter().collect();
        let records = match_subtitle_records(library, &scoped).await?;
        library.replace_external_subtitles(None, &records).await?;
    }
    Ok(())
}

async fn match_subtitle_records(
    library: &Library,
    sidecars: &[&SubtitleSidecar],
) -> Result<Vec<crate::store::SubtitleRecord>, ScanError> {
    use crate::subtitles::{self, SubtitleCandidate};
    use std::collections::HashMap;
    let mut records = Vec::new();
    // Sidecars in the same folder share a media directory; one query per
    // distinct directory rather than one per file.
    let mut dir_cache: HashMap<String, Vec<crate::store::EntryRecord>> = HashMap::new();
    for sidecar in sidecars {
        let Some(format) = subtitles::subtitle_extension(&sidecar.relative_path) else {
            continue;
        };
        let (media_dir, hints) = subtitles::subtitle_media_dir(&sidecar.relative_path);
        let entries = match dir_cache.get(&media_dir) {
            Some(entries) => entries,
            None => {
                let fetched = library.video_entries_under_dir(&media_dir).await?;
                dir_cache.entry(media_dir.clone()).or_insert(fetched)
            }
        };
        if entries.is_empty() {
            continue;
        }
        let candidates: Vec<SubtitleCandidate> = entries
            .iter()
            .map(|entry| SubtitleCandidate {
                entry_key: entry.entry_key.clone(),
                relative_path: entry.relative_path.clone(),
                kind: entry.kind,
                season: entry.season,
                episode: entry.episode,
            })
            .collect();
        let Some(chosen) = subtitles::match_subtitle_to_video(&sidecar.relative_path, &candidates)
        else {
            continue;
        };
        let Some(entry) = entries.iter().find(|e| e.entry_key == chosen.entry_key) else {
            continue;
        };

        let file_name = sidecar.relative_path.rsplit('/').next().unwrap_or_default();
        let stem = file_name
            .rsplit_once('.')
            .map(|(s, _)| s)
            .unwrap_or(file_name);
        let parsed = subtitles::parse_subtitle_name(stem);
        let label = if !parsed.label.is_empty() {
            parsed.label
        } else if let Some(hint) = hints.first() {
            hint.clone()
        } else {
            "External subtitles".to_string()
        };
        // Stable, path-derived id so a rescan upserts the same row rather
        // than churning it.
        let id = format!(
            "external-{}",
            swarm_core::entry_key::entry_key(&sidecar.relative_path)
        );
        records.push(crate::store::SubtitleRecord {
            id,
            entry_key: entry.entry_key.clone(),
            language: parsed.language.unwrap_or_else(|| "und".to_string()),
            label,
            source: "external".to_string(),
            format: format.to_string(),
            file_path: sidecar.absolute_path.clone(),
            fingerprint: entry.fingerprint.clone(),
        });
    }
    Ok(records)
}

/// Whether a staged manifest entry's stored `relative_path` belongs to a
/// root that produced a complete snapshot this pass, and may therefore
/// mutate the catalog. `completed_prefixes` is the same list used for
/// deletion reconciliation: in a multi-root install, one `Some("{label}/")`
/// per completed root; in a single-root install, `[None]` when the sole root
/// completed and empty otherwise.
fn path_in_completed_scope(
    relative_path: &str,
    multi_root_namespace: bool,
    completed_prefixes: &[Option<String>],
) -> bool {
    if multi_root_namespace {
        completed_prefixes.iter().any(|prefix| {
            prefix
                .as_deref()
                .is_some_and(|prefix| relative_path.starts_with(prefix))
        })
    } else {
        !completed_prefixes.is_empty()
    }
}

fn check_cancelled(cancel: Option<&AtomicBool>) -> Result<(), ScanError> {
    if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
        Err(ScanError::Cancelled)
    } else {
        Ok(())
    }
}

fn asset_type_for_path(
    roots: &[MediaRoot],
    relative_path: &str,
    multi_root_namespace: bool,
) -> MediaRootAssetType {
    if multi_root_namespace {
        if let Some((label, _)) = relative_path.split_once('/') {
            if let Some(root) = roots.iter().find(|root| root.label == label) {
                return root.asset_type;
            }
        }
    }
    roots
        .first()
        .map(|root| root.asset_type)
        .unwrap_or_default()
}

fn classify_for_asset_type(
    relative_path: &str,
    asset_type: MediaRootAssetType,
) -> Option<classify::Classified> {
    let mut classified = classify::classify(relative_path)?;
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
            classified.kind = MediaKind::Episode;
            if classified.show_title.is_none() {
                classified.show_title = relative_path
                    .split('/')
                    .next()
                    .filter(|part| !part.is_empty())
                    .map(str::to_string);
            }
        }
    }
    Some(classified)
}

fn media_path_allowed(
    relative_path: &str,
    asset_type: MediaRootAssetType,
    scan_music_tracks: bool,
) -> bool {
    let Some((_, is_audio)) = classify::media_extension(relative_path) else {
        return false;
    };
    if is_audio {
        scan_music_tracks
            && matches!(asset_type, MediaRootAssetType::Mixed | MediaRootAssetType::Music)
    } else {
        !matches!(asset_type, MediaRootAssetType::Music)
    }
}

/// Classifies an `images/` sibling file by the exact, small set of
/// filenames every artwork-writing path in this codebase actually produces
/// — `save_video_artwork`'s `{stem}-tmdb-{poster,season-poster,backdrop}.jpg`
/// (`scrape/runner.rs`), `scrape_one_album_group`'s fixed `album-cover.jpg`/
/// `artist-photo.jpg`, and the GUI's manual-upload `manual-{poster,backdrop,
/// cover,artist}.<ext>` (`apps/server/src/gui.rs`, `ArtworkKind::
/// route_segment`). Extension-agnostic (manual uploads aren't always
/// `.jpg`) — matches on the filename stem only.
fn recovered_artwork_kind(filename: &str) -> Option<ArtworkKind> {
    let lower = filename.to_lowercase();
    let stem = lower
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(lower.as_str());
    if stem.ends_with("-tmdb-poster") || stem == "manual-poster" {
        Some(ArtworkKind::Poster)
    } else if stem.ends_with("-tmdb-season-poster") || stem == "manual-season" {
        Some(ArtworkKind::SeasonPoster)
    } else if stem.ends_with("-tmdb-backdrop") || stem == "manual-backdrop" {
        Some(ArtworkKind::Backdrop)
    } else if stem == "album-cover" || stem == "manual-cover" {
        Some(ArtworkKind::Cover)
    } else if stem == "artist-photo" || stem == "manual-artist" {
        Some(ArtworkKind::ArtistPhoto)
    } else {
        None
    }
}

/// Real gap this closes, found live: scraped artwork is saved as a plain
/// file in an `images/` folder right beside the source media (see
/// `scrape::artwork::save_artwork`) — entirely independent of the SQLite
/// catalog. A rescan's `upsert` deliberately never touches artwork columns
/// for an *existing* row (so a normal incremental rescan can't clobber
/// already-scraped data), so any row with every artwork column unset —
/// whether it's a brand-new row (genuinely new file, or the catalog itself
/// was emptied and rebuilt) or an already-known, unchanged row that simply
/// never got relinked (e.g. it was first re-added by a scan that ran
/// *before* this recovery existed) — starts, or stays, looking unscraped
/// regardless of what's physically sitting on disk. Without this, a rebuilt
/// catalog looks completely unscraped even though the real artwork files
/// never moved, and would need a full, redundant re-scrape — real TMDb/
/// MusicBrainz API calls all over again — just to get back images that
/// were already there. Called only when the caller already knows this
/// entry has no artwork set (see both call sites) — an entry that already
/// has some is left alone, correctly untouched.
async fn recover_existing_artwork(
    library: &Library,
    absolute: &Path,
    entry_key: &str,
    relative_path: &str,
    kind: MediaKind,
    artwork_cache: &mut ArtworkCache,
) -> sqlx::Result<()> {
    let Some(parent) = absolute.parent() else {
        return Ok(());
    };
    let images_dir = parent.join("images");
    let candidates = artwork_cache.candidates(&images_dir).await;

    let relevant = |k: ArtworkKind| match kind {
        MediaKind::Movie | MediaKind::Episode => {
            matches!(
                k,
                ArtworkKind::Poster | ArtworkKind::SeasonPoster | ArtworkKind::Backdrop
            )
        }
        MediaKind::Track => matches!(k, ArtworkKind::Cover | ArtworkKind::ArtistPhoto),
    };
    let relative_images_dir = match relative_path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/images"),
        None => "images".to_string(),
    };
    // Movies/episodes each get their OWN poster/backdrop file, even though
    // several of them can share one `images/` folder (a flat movie root
    // with no per-movie subfolder, or a season folder holding many
    // episodes) — `save_video_artwork` names every file `{own_stem}-tmdb-
    // {poster,backdrop}.jpg`, so that prefix is the only safe way to tell
    // "mine" from "a sibling's". Real bug, found live: without this check,
    // *any* poster sitting in a shared `images/` folder got linked to
    // every sibling recovered from it — one movie's poster ("10 Cloverfield
    // Lane", in the reported case) ended up on many unrelated movies,
    // whichever happened to be recovered after it read the same folder.
    // Tracks are the deliberate exception, not a gap: `album-cover.jpg`/
    // `artist-photo.jpg` really are meant to be shared by every track in
    // the same album folder, so no stem check applies to them. Manual
    // uploads (`manual-poster.<ext>` etc.) have no per-entry stem to check
    // against at all, so they're deliberately excluded from automatic
    // recovery here — a real, separate gap for a manually-uploaded movie
    // poster in a shared folder, not one this fix can safely close too.
    let own_stem_prefix = format!(
        "{}-tmdb-",
        artwork::sanitize_stem(artwork::file_stem(relative_path)).to_lowercase()
    );

    for (name, found_kind) in candidates {
        if !relevant(found_kind) {
            continue;
        }
        if matches!(
            found_kind,
            ArtworkKind::Poster | ArtworkKind::SeasonPoster | ArtworkKind::Backdrop
        ) && !name.to_lowercase().starts_with(&own_stem_prefix)
        {
            continue;
        }
        library
            .set_artwork(
                entry_key,
                found_kind,
                &format!("{relative_images_dir}/{name}"),
            )
            .await?;
    }
    Ok(())
}

/// Path-ordered manifest pages keep sibling media together, so caching just
/// the current artwork directory avoids repeated SMB listings without an
/// unbounded directory-to-files map.
#[derive(Default)]
struct ArtworkCache {
    directory: Option<PathBuf>,
    entries: Vec<(String, ArtworkKind)>,
}

impl ArtworkCache {
    /// Async, not a plain fn, specifically so the cache-miss path's
    /// `std::fs::read_dir` (a real SMB/NFS round trip, same reasoning as
    /// `discover_media_files`'s doc comment) can run via `spawn_blocking`
    /// instead of inline on this shared runtime. The cache-hit path (the
    /// common case — sibling media in the same directory) does no I/O at
    /// all and returns immediately.
    async fn candidates(&mut self, directory: &Path) -> Vec<(String, ArtworkKind)> {
        if self.directory.as_deref() != Some(directory) {
            self.directory = Some(directory.to_path_buf());
            let directory = directory.to_path_buf();
            self.entries = tokio::task::spawn_blocking(move || {
                std::fs::read_dir(&directory)
                    .map(|entries| {
                        entries
                            .flatten()
                            .filter_map(|entry| {
                                let name = entry.file_name().to_string_lossy().into_owned();
                                recovered_artwork_kind(&name).map(|kind| (name, kind))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
        }
        self.entries.clone()
    }
}

/// Recursive allowlist walk, staged into SQLite in fixed-size batches. Size
/// and modification time are captured while each entry is hot in the SMB
/// client cache, avoiding a second network metadata round trip.
///
/// The walk itself (`walk_media_files`, below) runs entirely inside
/// `spawn_blocking` — never inline on this `async fn`'s own task. Real
/// incident: this task's tokio worker thread is shared with every QUIC/HTTP
/// request this server handles, and `std::fs::read_dir`/`metadata` against a
/// flaky or slow network mount (SMB over a VPN'd link, in the case that
/// found this) can each cost a real round trip with no yield point in
/// between — a single scan blocked every other request on the server for as
/// long as the walk ran, surfacing as artwork/playback requests stalling or
/// timing out client-side, not as a scan-specific symptom. A bounded
/// channel bridges the walk's synchronous batches back to this function's
/// async DB writes; its capacity also bounds how far the walk can run ahead
/// of persistence, the same "bound scan memory" goal the batching itself
/// already serves.
async fn discover_media_files(
    library: &Library,
    scan_id: &str,
    root: &MediaRoot,
    multi_root_namespace: bool,
    progress: Option<&Arc<ScanProgress>>,
    cancel: Option<&Arc<AtomicBool>>,
    options: ScanOptions,
) -> Result<(bool, Vec<SubtitleSidecar>), ScanError> {
    const BATCH_SIZE: usize = 256;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<ScanManifestEntry>>(2);
    let root_path = root.path.clone();
    let label = root.label.clone();
    let progress = progress.cloned();
    let cancel = cancel.cloned();
    let asset_type = root.asset_type;
    let walk = tokio::task::spawn_blocking(move || {
        walk_media_files(WalkConfig {
            root_path: &root_path,
            label: &label,
            multi_root_namespace,
            progress: progress.as_deref(),
            cancel: cancel.as_deref(),
            asset_type,
            scan_music_tracks: options.scan_music_tracks,
            batch_size: BATCH_SIZE,
            tx,
        })
    });

    while let Some(batch) = rx.recv().await {
        library.append_scan_manifest(scan_id, &batch).await?;
    }
    let outcome = walk
        .await
        .expect("media walk task panicked")
        .map_err(|source| ScanError::RootIo {
            label: root.label.clone(),
            path: root.path.clone(),
            source,
        })?;
    if outcome.cancelled {
        return Err(ScanError::Cancelled);
    }
    Ok((outcome.complete, outcome.subtitles))
}

#[derive(Debug, Clone, Default)]
struct WalkOutcome {
    complete: bool,
    cancelled: bool,
    /// Subtitle sidecar files (`.srt`/`.vtt`) seen during the same walk —
    /// carried out rather than streamed like media entries because they are
    /// sparse and matched to their owning video only after the full media
    /// snapshot is reconciled. Capped at [`MAX_SUBTITLE_SIDECARS`].
    subtitles: Vec<SubtitleSidecar>,
}

/// A `.srt`/`.vtt` file found beside media (or in a `Subs/` folder) during
/// the library walk, before it is matched to a catalog entry.
#[derive(Debug, Clone)]
struct SubtitleSidecar {
    /// Stored form, matching a catalog entry's `relative_path` (carries the
    /// `{label}/` prefix in a multi-root install).
    relative_path: String,
    /// Absolute path on disk — what the peer subtitle route reads and, for
    /// `.srt`, converts to WebVTT on the way out.
    absolute_path: String,
}

/// A defensive ceiling on how many subtitle sidecars one scan will hold in
/// memory at once. Real libraries have far fewer sidecars than media files;
/// this only guards against a pathological tree and keeps the "bound scan
/// memory" property the media walk's batching already has.
const MAX_SUBTITLE_SIDECARS: usize = 100_000;

/// An entry can be renamed or removed after `read_dir` returns it but before
/// `file_type` or `metadata` runs. Only that precise race is skippable. The
/// incomplete marker prevents the resulting partial snapshot from being
/// used to reconcile deletions.
fn walk_value<T>(result: std::io::Result<T>, complete: &mut bool) -> std::io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            *complete = false;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Bounded retry for `DirEntry::file_type`/`DirEntry::metadata`-style stat
/// calls that can be reissued safely (they just re-stat the same path).
/// Confirmed live: a busy `smbfs` mount under concurrent load (media serving
/// alongside a scrape run reading tags off the same share) returns transient
/// `ENOENT` for these calls on files that are still there — `find` over the
/// same tree moments later enumerates every one with zero errors. Without a
/// retry, that single flaky stat condemns the whole walk as incomplete,
/// which permanently blocks deletion reconciliation for any root busy
/// enough to hit this (#230). Only a `NotFound` that survives every retry
/// is treated as real.
const TRANSIENT_STAT_RETRIES: u32 = 4;
const TRANSIENT_STAT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

fn retry_transient_not_found<T>(mut stat: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 0;
    loop {
        match stat() {
            Ok(value) => return Ok(value),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && attempt + 1 < TRANSIENT_STAT_RETRIES =>
            {
                std::thread::sleep(TRANSIENT_STAT_RETRY_DELAY * (1 << attempt));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Pure synchronous directory walk — see [`discover_media_files`]'s doc
/// comment for why this must only ever run via `spawn_blocking`, never
/// inline on the shared async runtime. Sends completed batches through `tx`
/// as it goes; `Sender::blocking_send` blocks *this* (already-blocking-pool)
/// thread, not any async worker thread, when the channel is full, which is
/// exactly the backpressure a bounded channel is for.
struct WalkConfig<'a> {
    root_path: &'a Path,
    label: &'a str,
    multi_root_namespace: bool,
    progress: Option<&'a ScanProgress>,
    cancel: Option<&'a AtomicBool>,
    asset_type: MediaRootAssetType,
    scan_music_tracks: bool,
    batch_size: usize,
    tx: Sender<Vec<ScanManifestEntry>>,
}

fn walk_media_files(config: WalkConfig<'_>) -> std::io::Result<WalkOutcome> {
    let WalkConfig {
        root_path,
        label,
        multi_root_namespace,
        progress,
        cancel,
        asset_type,
        scan_music_tracks,
        batch_size,
        tx,
    } = config;
    let mut batch = Vec::with_capacity(batch_size);
    let mut subtitles: Vec<SubtitleSidecar> = Vec::new();
    let mut stack = vec![root_path.to_path_buf()];
    let mut complete = true;
    while let Some(dir) = stack.pop() {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
            return Ok(WalkOutcome {
                complete,
                cancelled: true,
                subtitles: Vec::new(),
            });
        }
        // The root itself not existing is a real unavailable-root failure.
        // A descendant can legitimately disappear after its parent was
        // listed, so tolerate only NotFound below the root.
        let entries = match retry_transient_not_found(|| std::fs::read_dir(&dir)) {
            Ok(entries) => entries,
            Err(error) if dir != root_path && error.kind() == std::io::ErrorKind::NotFound => {
                complete = false;
                continue;
            }
            Err(error) => return Err(error),
        };
        for entry in entries {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
                return Ok(WalkOutcome {
                    complete,
                    cancelled: true,
                    subtitles: Vec::new(),
                });
            }
            let Some(entry) = walk_value(entry, &mut complete)? else {
                continue;
            };
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Some(file_type) =
                walk_value(retry_transient_not_found(|| entry.file_type()), &mut complete)?
            else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(relative) = path.strip_prefix(root_path) else {
                continue;
            };
            let relative_under_root = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if crate::subtitles::subtitle_extension(&relative_under_root).is_some() {
                if subtitles.len() < MAX_SUBTITLE_SIDECARS {
                    let relative_path = if multi_root_namespace {
                        format!("{label}/{relative_under_root}")
                    } else {
                        relative_under_root.clone()
                    };
                    subtitles.push(SubtitleSidecar {
                        relative_path,
                        absolute_path: path.to_string_lossy().into_owned(),
                    });
                }
                continue;
            }
            if media_path_allowed(&relative_under_root, asset_type, scan_music_tracks) {
                let Some(metadata) =
                    walk_value(retry_transient_not_found(|| entry.metadata()), &mut complete)?
                else {
                    continue;
                };
                let modified_time = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs() as i64)
                    .unwrap_or(0);
                if let Some(p) = progress {
                    p.tick_discovering();
                }
                let relative_path = if multi_root_namespace {
                    format!("{label}/{relative_under_root}")
                } else {
                    relative_under_root.clone()
                };
                batch.push(ScanManifestEntry {
                    relative_path,
                    absolute_path: path.to_string_lossy().into_owned(),
                    relative_under_root,
                    size: metadata.len(),
                    modified_time,
                });
                if batch.len() == batch_size {
                    let full_batch = std::mem::replace(&mut batch, Vec::with_capacity(batch_size));
                    // Receiver gone means discover_media_files already
                    // failed on an earlier DB write — nothing left to hand
                    // batches to, so stop walking rather than keep working
                    // toward a result no one will read.
                    if tx.blocking_send(full_batch).is_err() {
                        return Ok(WalkOutcome {
                            complete,
                            cancelled: false,
                            subtitles: Vec::new(),
                        });
                    }
                }
            }
        }
    }
    if !batch.is_empty() {
        let _ = tx.blocking_send(batch);
    }
    Ok(WalkOutcome {
        complete,
        cancelled: false,
        subtitles,
    })
}

#[cfg(test)]
mod tests {
    use super::{path_in_completed_scope, walk_value};

    #[test]
    fn single_root_scope_gates_on_the_sole_root_completing() {
        // `[None]` = the one root completed; `[]` = it did not.
        assert!(path_in_completed_scope("movie.mp4", false, &[None]));
        assert!(!path_in_completed_scope("movie.mp4", false, &[]));
    }

    #[test]
    fn multi_root_scope_only_admits_completed_root_prefixes() {
        let completed = [Some("local/".to_string())];
        assert!(path_in_completed_scope("local/movie.mp4", true, &completed));
        // "share" was incomplete this pass, so nothing under it may mutate.
        assert!(!path_in_completed_scope("share/music/song.flac", true, &completed));
        // No completed roots at all: the whole pass is inert.
        assert!(!path_in_completed_scope("local/movie.mp4", true, &[]));
    }

    #[test]
    fn vanished_walk_entry_is_skipped_and_marks_snapshot_incomplete() {
        let mut complete = true;
        let error = std::io::Error::from(std::io::ErrorKind::NotFound);

        assert!(walk_value::<()>(Err(error), &mut complete)
            .unwrap()
            .is_none());
        assert!(!complete);
    }

    #[test]
    fn non_transient_walk_error_is_not_hidden() {
        let mut complete = true;
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);

        let result = walk_value::<()>(Err(error), &mut complete);
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(complete);
    }
}
