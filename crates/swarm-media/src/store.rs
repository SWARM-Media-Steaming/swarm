//! SQLite library store — the Batocera.Drone media-store architecture in
//! Rust: entries + pending-changes queue + deleted-archive + whole-library
//! thumbprint (the delta-sync/library-version primitive). Schema is created
//! idempotently; never bump applied schema in place.

use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::str::FromStr;
use swarm_core::peer::{
    AudioStreamInfo, CatalogEntry, MediaKind, SkipSegment, TrackLyrics, VideoStreamInfo,
};

#[derive(Debug, Clone, PartialEq)]
pub struct EntryRecord {
    pub entry_key: String,
    pub relative_path: String,
    pub kind: MediaKind,
    pub title: String,
    pub size: u64,
    pub modified_time: i64,
    pub fingerprint: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<u32>,
    pub show_title: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    /// Path/filename-derived release year (see `classify::extract_bracket_tags`)
    /// — same "never a grouping key, just a signal" status as everything
    /// else in this struct that isn't scraper overlay; used to disambiguate
    /// a TMDb search, not stored as scraper output itself.
    pub year: Option<u32>,
    pub duration_secs: Option<f64>,
    pub video: Option<VideoStreamInfo>,
    pub audio: Option<AudioStreamInfo>,
    /// Scraper overlay — display-only, never a grouping key (Drone rule).
    pub scraped_title: Option<String>,
    /// TV episode/special title from TMDb, separate from the canonical show
    /// title stored in `scraped_title` for cross-folder grouping.
    pub episode_title: Option<String>,
    pub genres: Vec<String>,
    pub artwork_version: u32,
    /// Scraper overlay, movies/episodes only — empty for tracks (music has
    /// no cast concept).
    pub cast: Vec<CastMember>,
    /// Synopsis — TMDb's own for movies/episodes (auto-populated at scrape
    /// time), or a manual override via [`Library::set_overview`]. `None`
    /// for tracks (no synopsis concept) and for anything not yet scraped.
    pub overview: Option<String>,
    /// US content rating — MPAA-style (`"PG-13"`) for a movie, TV Parental
    /// Guidelines-style (`"TV-14"`) for a show — TMDb's own (auto-populated
    /// at scrape time, see `scrape::tmdb::ScrapedVideo::certification`) or a
    /// manual override via [`Library::set_rating`]. `None` for tracks (no
    /// rating concept) and for anything not yet scraped or without a US
    /// certification on file.
    pub rating: Option<String>,
    /// Provider community score normalized to a common 0–10 scale: TMDb's
    /// native score for movies/TV and twice MusicBrainz's 0–5 release-group
    /// score for music. Kept separate from `rating`, which is a parental
    /// content certification used by Kid Mode.
    pub community_rating: Option<f64>,
    /// Number of provider votes behind `community_rating`, when supplied.
    pub community_rating_votes: Option<u64>,
}

/// Bump whenever a successful online scrape begins populating new durable
/// metadata. Rows written by an older scraper version become eligible for a
/// one-time backfill even though their title/artwork scrape already finished.
const CURRENT_SCRAPE_VERSION: i64 = 3;

/// One TMDb credits-list entry, capped to roughly the first ten (billing
/// order) at scrape time. Same status as `scraped_title`/`genres` — display
/// overlay only, never a grouping key.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CastMember {
    pub name: String,
    pub character: Option<String>,
}

/// Which artwork slot a downloaded image fills. Maps 1:1 onto the
/// `/art/{entry_key}/{kind}` peer route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtworkKind {
    Poster,
    SeasonPoster,
    Backdrop,
    Cover,
    ArtistPhoto,
}

impl ArtworkKind {
    pub fn route_segment(self) -> &'static str {
        match self {
            ArtworkKind::Poster => "poster",
            ArtworkKind::SeasonPoster => "season",
            ArtworkKind::Backdrop => "backdrop",
            ArtworkKind::Cover => "cover",
            ArtworkKind::ArtistPhoto => "artist",
        }
    }

    pub fn parse(segment: &str) -> Option<Self> {
        match segment {
            "poster" => Some(ArtworkKind::Poster),
            "season" => Some(ArtworkKind::SeasonPoster),
            "backdrop" => Some(ArtworkKind::Backdrop),
            "cover" => Some(ArtworkKind::Cover),
            "artist" => Some(ArtworkKind::ArtistPhoto),
            _ => None,
        }
    }

    fn column(self) -> &'static str {
        match self {
            ArtworkKind::Poster => "poster_relative_path",
            ArtworkKind::SeasonPoster => "season_poster_relative_path",
            ArtworkKind::Backdrop => "backdrop_relative_path",
            ArtworkKind::Cover => "cover_relative_path",
            ArtworkKind::ArtistPhoto => "artist_art_relative_path",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingChange {
    pub entry_key: String,
    pub operation: String,
}

/// [`Library::snapshot`]'s per-entry summary — just enough for
/// `scan::scan_roots` to detect a real content change (`size`/
/// `modified_time`) and whether artwork-recovery is worth attempting
/// (`has_artwork`, true if any of the four artwork columns is set) without
/// a second per-entry query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownEntry {
    pub size: u64,
    pub modified_time: i64,
    pub fingerprint: String,
    pub has_artwork: bool,
    /// Set by [`Library::set_manual_kind`] — a scan that finds this file
    /// *unchanged* never touches `kind`/grouping anyway (its whole record is
    /// skipped), but a scan that finds it *changed on disk* (re-encoded,
    /// replaced) would otherwise re-derive and overwrite them from the path
    /// alone via `classify()`, silently reverting the override. `scan_roots`
    /// checks this flag specifically for that case.
    pub kind_overridden: bool,
    pub available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingDisposition {
    NewlyMissing,
    StillMissing,
    ConfirmedMissing,
}

/// One filesystem result staged by a library scan. Keeping the complete
/// manifest in SQLite lets the scanner reconcile deletions safely without
/// retaining every path and metadata record in process memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanManifestEntry {
    pub relative_path: String,
    pub absolute_path: String,
    pub relative_under_root: String,
    pub size: u64,
    pub modified_time: i64,
}

/// A stored [`swarm_core::peer::ClientErrorReport`] — `received_at_ms` is the
/// server's own clock, kept alongside the client-reported `occurred_at_ms`
/// since the two can disagree (clock skew, queued-then-flushed reports) and
/// triage cares about both: when it happened on the device, and when this
/// server actually found out about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientErrorRecord {
    pub id: i64,
    pub device_id: String,
    pub device_name: String,
    pub entry_key: Option<String>,
    pub asset_title: Option<String>,
    pub kind: Option<String>,
    pub message: String,
    pub context: Option<String>,
    pub occurred_at_ms: i64,
    pub received_at_ms: i64,
    pub resolution_comments: Option<String>,
    pub resolved_at_ms: Option<i64>,
    pub dismissed_at_ms: Option<i64>,
}

/// A resolved problem waiting for the reporting client to view or dismiss it.
/// The server remains the source of truth while the TV keeps a local copy for
/// offline viewing and de-duplicates the next poll by `(server_id, id)`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ClientResolutionNotification {
    pub id: i64,
    pub asset_title: Option<String>,
    pub original_message: String,
    pub comments: Option<String>,
    pub resolved_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerNotificationRecord {
    pub id: i64,
    pub level: String,
    pub title: String,
    pub message: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleRecord {
    pub id: String,
    pub entry_key: String,
    pub language: String,
    pub label: String,
    pub source: String,
    pub format: String,
    pub file_path: String,
    pub fingerprint: String,
}

/// Files associated with one library entry that can be removed when the
/// entry itself is permanently deleted. The manifest distinguishes all
/// referenced artwork from its unshared subset because album covers, artist
/// photos, and season artwork may belong to multiple entries.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetDeletionManifest {
    pub entry: EntryRecord,
    /// Every artwork path referenced by this entry, including shared art.
    pub artwork_paths: Vec<String>,
    /// The subset of `artwork_paths` with no reference from another entry.
    pub unshared_artwork_paths: Vec<String>,
    pub subtitle_tracks: Vec<SubtitleRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionJob {
    pub entry_key: String,
    pub fingerprint: String,
    pub model: String,
    pub language: String,
    pub total_segments: u32,
    pub completed_segments: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptionQueueStatus {
    pub queued: u64,
    pub completed: u64,
    pub failed: u64,
    pub total_segments: u64,
    pub completed_segments: u64,
}

fn kind_str(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movie",
        MediaKind::Episode => "episode",
        MediaKind::Track => "track",
    }
}

fn parse_kind(raw: &str) -> MediaKind {
    match raw {
        "episode" => MediaKind::Episode,
        "track" => MediaKind::Track,
        _ => MediaKind::Movie,
    }
}

fn unix_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub struct Library {
    pool: SqlitePool,
}

impl Library {
    pub async fn open(database_path: &str) -> sqlx::Result<Self> {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{database_path}"))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS library_entries (
                entry_key TEXT PRIMARY KEY,
                relative_path TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                title TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified_time INTEGER NOT NULL,
                fingerprint TEXT NOT NULL,
                artist TEXT, album TEXT, track_number INTEGER,
                show_title TEXT, season INTEGER, episode INTEGER,
                duration_secs REAL, video_json TEXT, audio_json TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_entries_path ON library_entries(relative_path COLLATE NOCASE);
            CREATE TABLE IF NOT EXISTS track_lyrics (
                entry_key TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                provider_id INTEGER,
                language TEXT,
                plain_lyrics TEXT,
                synced_lyrics TEXT,
                instrumental INTEGER NOT NULL DEFAULT 0 CHECK (instrumental IN (0, 1)),
                fetched_at_ms INTEGER NOT NULL,
                FOREIGN KEY (entry_key) REFERENCES library_entries(entry_key) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_track_lyrics_provider_id ON track_lyrics(provider, provider_id);
            CREATE INDEX IF NOT EXISTS idx_track_lyrics_language ON track_lyrics(language);
            CREATE TABLE IF NOT EXISTS transcription_jobs (
                entry_key TEXT PRIMARY KEY,
                fingerprint TEXT NOT NULL,
                model TEXT NOT NULL,
                language TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('queued','transcribing','finalizing','completed','failed')),
                total_segments INTEGER NOT NULL CHECK (total_segments > 0),
                completed_segments INTEGER NOT NULL DEFAULT 0 CHECK (completed_segments >= 0),
                error TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                FOREIGN KEY (entry_key) REFERENCES library_entries(entry_key) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_transcription_jobs_status ON transcription_jobs(status, updated_at_ms);
            CREATE TABLE IF NOT EXISTS transcription_segments (
                entry_key TEXT NOT NULL,
                segment_index INTEGER NOT NULL,
                cues_json TEXT NOT NULL,
                completed_at_ms INTEGER NOT NULL,
                PRIMARY KEY (entry_key, segment_index),
                FOREIGN KEY (entry_key) REFERENCES transcription_jobs(entry_key) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS subtitle_tracks (
                entry_key TEXT NOT NULL,
                id TEXT NOT NULL,
                language TEXT NOT NULL,
                label TEXT NOT NULL,
                source TEXT NOT NULL,
                format TEXT NOT NULL,
                file_path TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                generated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (entry_key, id),
                FOREIGN KEY (entry_key) REFERENCES library_entries(entry_key) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_subtitle_tracks_entry ON subtitle_tracks(entry_key, language);
            CREATE TABLE IF NOT EXISTS deleted_library_entries (
                entry_key TEXT PRIMARY KEY,
                relative_path TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                deleted_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS library_changes (
                entry_key TEXT PRIMARY KEY,
                operation TEXT NOT NULL CHECK (operation IN ('upsert','delete'))
            );
            CREATE TABLE IF NOT EXISTS client_errors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                device_id TEXT NOT NULL,
                device_name TEXT NOT NULL,
                entry_key TEXT,
                asset_title TEXT,
                kind TEXT,
                message TEXT NOT NULL,
                context TEXT,
                occurred_at_ms INTEGER NOT NULL,
                received_at_ms INTEGER NOT NULL,
                resolution_comments TEXT,
                resolved_at_ms INTEGER,
                dismissed_at_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_client_errors_received ON client_errors(received_at_ms DESC);
            CREATE TABLE IF NOT EXISTS server_notifications (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                level TEXT NOT NULL CHECK (level IN ('success','warning','error')),
                title TEXT NOT NULL,
                message TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_server_notifications_created ON server_notifications(created_at_ms DESC, id DESC);
            CREATE TABLE IF NOT EXISTS entry_likes (
                entry_key TEXT NOT NULL,
                device_id TEXT NOT NULL,
                device_name TEXT NOT NULL,
                liked_at_ms INTEGER NOT NULL,
                PRIMARY KEY (entry_key, device_id)
            );
            CREATE TABLE IF NOT EXISTS scan_manifest (
                scan_id TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                absolute_path TEXT NOT NULL,
                relative_under_root TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified_time INTEGER NOT NULL,
                PRIMARY KEY (scan_id, relative_path)
            );
            CREATE TABLE IF NOT EXISTS asset_metadata_history (
                fingerprint TEXT NOT NULL,
                size INTEGER NOT NULL,
                source_relative_path TEXT NOT NULL,
                kind TEXT NOT NULL,
                scraped_title TEXT,
                episode_title TEXT,
                genres_json TEXT,
                cast_json TEXT,
                overview TEXT,
                rating TEXT,
                community_rating REAL,
                community_rating_votes INTEGER,
                poster_relative_path TEXT,
                season_poster_relative_path TEXT,
                backdrop_relative_path TEXT,
                cover_relative_path TEXT,
                artist_art_relative_path TEXT,
                scrape_version INTEGER NOT NULL DEFAULT 0,
                restore_automatically INTEGER NOT NULL DEFAULT 1 CHECK (restore_automatically IN (0, 1)),
                archived_at_ms INTEGER NOT NULL,
                PRIMARY KEY (fingerprint, size, restore_automatically)
            );
            "#,
        )
        .execute(&pool)
        .await?;
        // Scraper/artwork columns, added the idempotent way (never bump the
        // base CREATE TABLE in place — the Drone convention).
        for (column, ddl_type) in [
            ("scraped_title", "TEXT"),
            ("genres_json", "TEXT"),
            ("cast_json", "TEXT"),
            ("year", "INTEGER"),
            ("poster_relative_path", "TEXT"),
            ("season_poster_relative_path", "TEXT"),
            ("backdrop_relative_path", "TEXT"),
            ("cover_relative_path", "TEXT"),
            ("artist_art_relative_path", "TEXT"),
            ("artwork_version", "INTEGER NOT NULL DEFAULT 0"),
            ("overview", "TEXT"),
            ("kind_overridden", "INTEGER NOT NULL DEFAULT 0"),
            ("rating", "TEXT"),
            ("community_rating", "REAL"),
            ("community_rating_votes", "INTEGER"),
            ("scrape_version", "INTEGER NOT NULL DEFAULT 0"),
            ("episode_title", "TEXT"),
            ("tmdb_id", "INTEGER"),
            ("skip_segments_json", "TEXT"),
            ("available", "INTEGER NOT NULL DEFAULT 1"),
            ("missing_scan_count", "INTEGER NOT NULL DEFAULT 0"),
            ("missing_since_ms", "INTEGER"),
            ("missing_confirmed", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            ensure_column(&pool, "library_entries", column, ddl_type).await?;
        }
        for (column, ddl_type) in [("tmdb_id", "INTEGER"), ("skip_segments_json", "TEXT")] {
            ensure_column(&pool, "asset_metadata_history", column, ddl_type).await?;
        }
        for (column, ddl_type) in [
            ("resolution_comments", "TEXT"),
            ("resolved_at_ms", "INTEGER"),
            ("dismissed_at_ms", "INTEGER"),
        ] {
            ensure_column(&pool, "client_errors", column, ddl_type).await?;
        }
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_client_errors_notifications \
             ON client_errors(device_id, resolved_at_ms DESC) \
             WHERE resolved_at_ms IS NOT NULL AND dismissed_at_ms IS NULL",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    pub async fn begin_scan_manifest(&self) -> sqlx::Result<String> {
        let scan_id = format!(
            "{}-{}-{}",
            std::process::id(),
            unix_time_ms(),
            rand::random::<u64>()
        );
        // A prior process may have exited mid-scan. Those rows can never be
        // used again because scan IDs are unique, so reclaim them here.
        sqlx::query("DELETE FROM scan_manifest")
            .execute(&self.pool)
            .await?;
        Ok(scan_id)
    }

    pub async fn append_scan_manifest(
        &self,
        scan_id: &str,
        entries: &[ScanManifestEntry],
    ) -> sqlx::Result<()> {
        let mut transaction = self.pool.begin().await?;
        for entry in entries {
            sqlx::query(
                "INSERT OR REPLACE INTO scan_manifest \
                 (scan_id, relative_path, absolute_path, relative_under_root, size, modified_time) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(scan_id)
            .bind(&entry.relative_path)
            .bind(&entry.absolute_path)
            .bind(&entry.relative_under_root)
            .bind(entry.size as i64)
            .bind(entry.modified_time)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await
    }

    pub async fn scan_manifest_count(&self, scan_id: &str) -> sqlx::Result<u64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM scan_manifest WHERE scan_id = ?")
                .bind(scan_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count.max(0) as u64)
    }

    /// Returns the next bounded page in path order. `after` is an exclusive
    /// keyset cursor, avoiding OFFSET work as the manifest grows.
    pub async fn scan_manifest_page(
        &self,
        scan_id: &str,
        after: &str,
        limit: u32,
    ) -> sqlx::Result<Vec<ScanManifestEntry>> {
        type Row = (String, String, String, i64, i64);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT relative_path, absolute_path, relative_under_root, size, modified_time \
             FROM scan_manifest WHERE scan_id = ? AND relative_path > ? \
             ORDER BY relative_path LIMIT ?",
        )
        .bind(scan_id)
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(relative_path, absolute_path, relative_under_root, size, modified_time)| {
                    ScanManifestEntry {
                        relative_path,
                        absolute_path,
                        relative_under_root,
                        size: size.max(0) as u64,
                        modified_time,
                    }
                },
            )
            .collect())
    }

    pub async fn known_entry_by_path(
        &self,
        relative_path: &str,
    ) -> sqlx::Result<Option<KnownEntry>> {
        type Row = (
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            i64,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT size, modified_time, fingerprint, poster_relative_path, \
             season_poster_relative_path, backdrop_relative_path, cover_relative_path, \
             artist_art_relative_path, kind_overridden, available FROM library_entries \
             WHERE relative_path = ?",
        )
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(
                size,
                modified_time,
                fingerprint,
                poster,
                season_poster,
                backdrop,
                cover,
                artist,
                kind_overridden,
                available,
            )| {
                KnownEntry {
                    size: size.max(0) as u64,
                    modified_time,
                    fingerprint,
                    has_artwork: poster.is_some()
                        || season_poster.is_some()
                        || backdrop.is_some()
                        || cover.is_some()
                        || artist.is_some(),
                    kind_overridden: kind_overridden != 0,
                    available: available != 0,
                }
            },
        ))
    }

    pub async fn entry_count_with_prefix(&self, prefix: Option<&str>) -> sqlx::Result<usize> {
        let count: (i64,) = if let Some(prefix) = prefix {
            sqlx::query_as(
                "SELECT COUNT(*) FROM library_entries \
                 WHERE substr(relative_path, 1, ?) = ?",
            )
            .bind(prefix.chars().count() as i64)
            .bind(prefix)
            .fetch_one(&self.pool)
            .await?
        } else {
            sqlx::query_as("SELECT COUNT(*) FROM library_entries")
                .fetch_one(&self.pool)
                .await?
        };
        Ok(count.0.max(0) as usize)
    }

    /// Finds a keyset page of stored paths absent from this scan manifest.
    /// This includes already-unavailable tombstones so each later successful
    /// scan can advance confirmation without loading them all into memory.
    pub async fn paths_missing_from_scan(
        &self,
        scan_id: &str,
        prefix: Option<&str>,
        after: &str,
        limit: u32,
    ) -> sqlx::Result<Vec<String>> {
        let rows: Vec<(String,)> = if let Some(prefix) = prefix {
            sqlx::query_as(
                "SELECT entries.relative_path FROM library_entries entries \
                 LEFT JOIN scan_manifest manifest ON manifest.scan_id = ? \
                    AND manifest.relative_path = entries.relative_path \
                 WHERE manifest.relative_path IS NULL \
                    AND substr(entries.relative_path, 1, ?) = ? \
                    AND entries.relative_path > ? \
                 ORDER BY entries.relative_path LIMIT ?",
            )
            .bind(scan_id)
            .bind(prefix.chars().count() as i64)
            .bind(prefix)
            .bind(after)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT entries.relative_path FROM library_entries entries \
                 LEFT JOIN scan_manifest manifest ON manifest.scan_id = ? \
                    AND manifest.relative_path = entries.relative_path \
                 WHERE manifest.relative_path IS NULL \
                    AND entries.relative_path > ? \
                 ORDER BY entries.relative_path LIMIT ?",
            )
            .bind(scan_id)
            .bind(after)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows.into_iter().map(|row| row.0).collect())
    }

    pub async fn clear_scan_manifest(&self, scan_id: &str) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM scan_manifest WHERE scan_id = ?")
            .bind(scan_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// [`KnownEntry`] per relative path — the scanner's change-detection
    /// snapshot.
    pub async fn snapshot(&self) -> sqlx::Result<HashMap<String, KnownEntry>> {
        type Row = (
            String,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            i64,
        );
        let rows: Vec<Row> =
            sqlx::query_as(
                "SELECT relative_path, size, modified_time, fingerprint, \
                 poster_relative_path, season_poster_relative_path, backdrop_relative_path, cover_relative_path, artist_art_relative_path, \
                 kind_overridden, available \
                 FROM library_entries",
            )
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    path,
                    size,
                    mtime,
                    fingerprint,
                    poster,
                    season_poster,
                    backdrop,
                    cover,
                    artist,
                    kind_overridden,
                    available,
                )| {
                    let has_artwork = poster.is_some()
                        || season_poster.is_some()
                        || backdrop.is_some()
                        || cover.is_some()
                        || artist.is_some();
                    (
                        path,
                        KnownEntry {
                            size: size as u64,
                            modified_time: mtime,
                            fingerprint,
                            has_artwork,
                            kind_overridden: kind_overridden != 0,
                            available: available != 0,
                        },
                    )
                },
            )
            .collect())
    }

    pub async fn upsert(&self, record: &EntryRecord) -> sqlx::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO library_entries
                (entry_key, relative_path, kind, title, size, modified_time, fingerprint,
                 artist, album, track_number, show_title, season, episode,
                 duration_secs, video_json, audio_json, year)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(entry_key) DO UPDATE SET
                relative_path = excluded.relative_path, kind = excluded.kind, title = excluded.title,
                size = excluded.size, modified_time = excluded.modified_time, fingerprint = excluded.fingerprint,
                artist = excluded.artist, album = excluded.album, track_number = excluded.track_number,
                show_title = excluded.show_title, season = excluded.season, episode = excluded.episode,
                duration_secs = excluded.duration_secs, video_json = excluded.video_json,
                audio_json = excluded.audio_json, year = excluded.year,
                available = 1, missing_scan_count = 0, missing_since_ms = NULL, missing_confirmed = 0
            "#,
        )
        .bind(&record.entry_key)
        .bind(&record.relative_path)
        .bind(kind_str(record.kind))
        .bind(&record.title)
        .bind(record.size as i64)
        .bind(record.modified_time)
        .bind(&record.fingerprint)
        .bind(&record.artist)
        .bind(&record.album)
        .bind(record.track_number.map(|n| n as i64))
        .bind(&record.show_title)
        .bind(record.season.map(|n| n as i64))
        .bind(record.episode.map(|n| n as i64))
        .bind(record.duration_secs)
        .bind(record.video.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()))
        .bind(record.audio.as_ref().map(|a| serde_json::to_string(a).unwrap_or_default()))
        .bind(record.year.map(|n| n as i64))
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO library_changes (entry_key, operation) VALUES (?, 'upsert') \
             ON CONFLICT(entry_key) DO UPDATE SET operation = 'upsert'",
        )
        .bind(&record.entry_key)
        .execute(&self.pool)
        .await?;
        // A resurrected path is no longer deleted.
        sqlx::query("DELETE FROM deleted_library_entries WHERE entry_key = ?")
            .bind(&record.entry_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Keep the currently-visible metadata while making a changed asset
    /// eligible for the next incremental scrape. This avoids a blank UI
    /// between scan and scrape, while ensuring new file contents are not
    /// incorrectly treated as already matched.
    pub async fn mark_scrape_stale(&self, entry_key: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE library_entries SET scrape_version = 0 WHERE entry_key = ?")
            .bind(entry_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn archive_metadata(
        &self,
        entry_key: &str,
        restore_automatically: bool,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO asset_metadata_history (\
                fingerprint, size, source_relative_path, kind, scraped_title, episode_title, \
                genres_json, cast_json, overview, rating, community_rating, community_rating_votes, \
                poster_relative_path, season_poster_relative_path, backdrop_relative_path, \
                cover_relative_path, artist_art_relative_path, tmdb_id, skip_segments_json, scrape_version, \
                restore_automatically, archived_at_ms) \
             SELECT fingerprint, size, relative_path, kind, scraped_title, episode_title, \
                genres_json, cast_json, overview, rating, community_rating, community_rating_votes, \
                poster_relative_path, season_poster_relative_path, backdrop_relative_path, \
                cover_relative_path, artist_art_relative_path, tmdb_id, skip_segments_json, scrape_version, ?, ? \
             FROM library_entries WHERE entry_key = ? \
             ON CONFLICT(fingerprint, size, restore_automatically) DO UPDATE SET \
                source_relative_path = excluded.source_relative_path, kind = excluded.kind, \
                scraped_title = excluded.scraped_title, episode_title = excluded.episode_title, \
                genres_json = excluded.genres_json, cast_json = excluded.cast_json, \
                overview = excluded.overview, rating = excluded.rating, \
                community_rating = excluded.community_rating, \
                community_rating_votes = excluded.community_rating_votes, \
                poster_relative_path = excluded.poster_relative_path, \
                season_poster_relative_path = excluded.season_poster_relative_path, \
                backdrop_relative_path = excluded.backdrop_relative_path, \
                cover_relative_path = excluded.cover_relative_path, \
                artist_art_relative_path = excluded.artist_art_relative_path, \
                tmdb_id = excluded.tmdb_id, skip_segments_json = excluded.skip_segments_json, \
                scrape_version = excluded.scrape_version, \
                restore_automatically = excluded.restore_automatically, \
                archived_at_ms = excluded.archived_at_ms",
        )
        .bind(i64::from(restore_automatically))
        .bind(unix_time_ms())
        .bind(entry_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_missing_by_path(
        &self,
        relative_path: &str,
        confirmation_grace_ms: i64,
    ) -> sqlx::Result<Option<MissingDisposition>> {
        type Row = (String, String, i64, i64, i64, Option<i64>);
        let row: Option<Row> = sqlx::query_as(
            "SELECT entry_key, fingerprint, size, available, missing_scan_count, missing_since_ms \
             FROM library_entries WHERE relative_path = ?",
        )
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await?;
        let Some((entry_key, fingerprint, _size, available, missing_count, missing_since)) = row
        else {
            return Ok(None);
        };
        let now = unix_time_ms();
        if available != 0 {
            self.archive_metadata(&entry_key, true).await?;
            let mut transaction = self.pool.begin().await?;
            sqlx::query(
                "UPDATE library_entries SET available = 0, missing_scan_count = 1, \
                 missing_since_ms = ?, missing_confirmed = 0 WHERE entry_key = ?",
            )
            .bind(now)
            .bind(&entry_key)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT OR REPLACE INTO deleted_library_entries \
                 (entry_key, relative_path, fingerprint, deleted_at) VALUES (?, ?, ?, ?)",
            )
            .bind(&entry_key)
            .bind(relative_path)
            .bind(&fingerprint)
            .bind(now / 1_000)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO library_changes (entry_key, operation) VALUES (?, 'delete') \
                 ON CONFLICT(entry_key) DO UPDATE SET operation = 'delete'",
            )
            .bind(&entry_key)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(Some(MissingDisposition::NewlyMissing));
        }

        let next_count = missing_count.saturating_add(1);
        let confirmed = next_count >= 2
            && missing_since
                .is_some_and(|since| now.saturating_sub(since) >= confirmation_grace_ms);
        sqlx::query(
            "UPDATE library_entries SET missing_scan_count = ?, missing_confirmed = ? \
             WHERE entry_key = ?",
        )
        .bind(next_count)
        .bind(i64::from(confirmed))
        .bind(&entry_key)
        .execute(&self.pool)
        .await?;
        Ok(Some(if confirmed {
            MissingDisposition::ConfirmedMissing
        } else {
            MissingDisposition::StillMissing
        }))
    }

    pub async fn restore_available_by_path(&self, relative_path: &str) -> sqlx::Result<bool> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT entry_key FROM library_entries WHERE relative_path = ? AND available = 0",
        )
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await?;
        let Some((entry_key,)) = row else {
            return Ok(false);
        };
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE library_entries SET available = 1, missing_scan_count = 0, \
             missing_since_ms = NULL, missing_confirmed = 0 WHERE entry_key = ?",
        )
        .bind(&entry_key)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM deleted_library_entries WHERE entry_key = ?")
            .bind(&entry_key)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO library_changes (entry_key, operation) VALUES (?, 'upsert') \
             ON CONFLICT(entry_key) DO UPDATE SET operation = 'upsert'",
        )
        .bind(&entry_key)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn restore_archived_metadata(
        &self,
        entry_key: &str,
        fingerprint: &str,
        size: u64,
        new_relative_path: &str,
    ) -> sqlx::Result<bool> {
        let archived = sqlx::query_as::<_, MetadataHistoryRow>(
            "SELECT source_relative_path, kind, scraped_title, episode_title, genres_json, cast_json, \
             overview, rating, community_rating, community_rating_votes, poster_relative_path, \
             season_poster_relative_path, backdrop_relative_path, cover_relative_path, \
             artist_art_relative_path, tmdb_id, skip_segments_json, scrape_version FROM asset_metadata_history \
             WHERE fingerprint = ? AND size = ? AND restore_automatically = 1 \
               AND kind = (SELECT kind FROM library_entries WHERE entry_key = ?)",
        )
        .bind(fingerprint)
        .bind(size as i64)
        .bind(entry_key)
        .fetch_optional(&self.pool)
        .await?;
        // During a rename's first scan, the old path is still an active row
        // until the complete manifest has processed successfully. It is a
        // safe bounded fallback source and avoids a write-heavy pre-archive
        // pass solely to make rename restoration possible.
        let history = match archived {
            Some(history) => Some(history),
            None => sqlx::query_as::<_, MetadataHistoryRow>(
                "SELECT relative_path AS source_relative_path, kind, scraped_title, episode_title, \
                 genres_json, cast_json, overview, rating, community_rating, community_rating_votes, \
                 poster_relative_path, season_poster_relative_path, backdrop_relative_path, \
                 cover_relative_path, artist_art_relative_path, tmdb_id, skip_segments_json, scrape_version \
                 FROM library_entries WHERE fingerprint = ? AND size = ? AND entry_key != ? \
                   AND kind = (SELECT kind FROM library_entries WHERE entry_key = ?) \
                 ORDER BY available DESC LIMIT 1",
            )
            .bind(fingerprint)
            .bind(size as i64)
            .bind(entry_key)
            .bind(entry_key)
            .fetch_optional(&self.pool)
            .await?,
        };
        let Some(history) = history else {
            return Ok(false);
        };
        let remap = |path: Option<String>| {
            path.map(|path| {
                remap_archived_artwork_path(&path, &history.source_relative_path, new_relative_path)
            })
        };
        sqlx::query(
            "UPDATE library_entries SET scraped_title = ?, episode_title = ?, genres_json = ?, \
             cast_json = ?, overview = ?, rating = ?, community_rating = ?, \
             community_rating_votes = ?, poster_relative_path = ?, season_poster_relative_path = ?, \
             backdrop_relative_path = ?, cover_relative_path = ?, artist_art_relative_path = ?, \
             tmdb_id = ?, skip_segments_json = ?, scrape_version = ?, \
             artwork_version = artwork_version + 1 WHERE entry_key = ?",
        )
        .bind(history.scraped_title)
        .bind(history.episode_title)
        .bind(history.genres_json)
        .bind(history.cast_json)
        .bind(history.overview)
        .bind(history.rating)
        .bind(history.community_rating)
        .bind(history.community_rating_votes)
        .bind(remap(history.poster_relative_path))
        .bind(remap(history.season_poster_relative_path))
        .bind(remap(history.backdrop_relative_path))
        .bind(remap(history.cover_relative_path))
        .bind(remap(history.artist_art_relative_path))
        .bind(history.tmdb_id)
        .bind(history.skip_segments_json)
        .bind(history.scrape_version)
        .bind(entry_key)
        .execute(&self.pool)
        .await?;
        Ok(true)
    }

    pub async fn has_archived_metadata(
        &self,
        fingerprint: &str,
        size: u64,
        restore_automatically: bool,
    ) -> sqlx::Result<bool> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM asset_metadata_history \
             WHERE fingerprint = ? AND size = ? AND restore_automatically = ?",
        )
        .bind(fingerprint)
        .bind(size as i64)
        .bind(i64::from(restore_automatically))
        .fetch_one(&self.pool)
        .await?;
        Ok(count > 0)
    }

    /// Permanently forgets an asset and its archived metadata. Automatic
    /// scans use `mark_missing_by_path` instead.
    pub async fn remove_by_path(&self, relative_path: &str) -> sqlx::Result<()> {
        let row: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT entry_key, fingerprint, size FROM library_entries WHERE relative_path = ?",
        )
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await?;
        let Some((entry_key, fingerprint, size)) = row else {
            return Ok(());
        };
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM library_entries WHERE entry_key = ?")
            .bind(&entry_key)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM asset_metadata_history WHERE fingerprint = ? AND size = ?")
            .bind(&fingerprint)
            .bind(size)
            .execute(&mut *transaction)
            .await?;
        // Likes deliberately have no foreign key because they can arrive
        // from a client before a catalog sync has populated the entry. A
        // permanent, user-requested deletion still needs to forget them.
        sqlx::query("DELETE FROM entry_likes WHERE entry_key = ?")
            .bind(&entry_key)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT OR REPLACE INTO deleted_library_entries (entry_key, relative_path, fingerprint, deleted_at) \
             VALUES (?, ?, ?, strftime('%s','now'))",
        )
        .bind(&entry_key)
        .bind(relative_path)
        .bind(&fingerprint)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO library_changes (entry_key, operation) VALUES (?, 'delete') \
             ON CONFLICT(entry_key) DO UPDATE SET operation = 'delete'",
        )
        .bind(&entry_key)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Build the filesystem cleanup plan for a permanent asset deletion.
    /// The caller owns filesystem I/O; keeping the reference check here
    /// makes the shared-artwork rule authoritative and testable next to the
    /// schema that defines those references.
    pub async fn asset_deletion_manifest(
        &self,
        entry_key: &str,
    ) -> sqlx::Result<Option<AssetDeletionManifest>> {
        let Some(entry) = self.get(entry_key).await? else {
            return Ok(None);
        };
        type ArtworkPathRow = (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let artwork: ArtworkPathRow = sqlx::query_as(
            "SELECT poster_relative_path, season_poster_relative_path, backdrop_relative_path, \
             cover_relative_path, artist_art_relative_path FROM library_entries WHERE entry_key = ?",
        )
        .bind(entry_key)
        .fetch_one(&self.pool)
        .await?;

        let mut seen = std::collections::HashSet::new();
        let mut artwork_paths = Vec::new();
        let mut unshared_artwork_paths = Vec::new();
        for path in [artwork.0, artwork.1, artwork.2, artwork.3, artwork.4]
            .into_iter()
            .flatten()
        {
            if !seen.insert(path.clone()) {
                continue;
            }
            artwork_paths.push(path.clone());
            let (references,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM library_entries WHERE entry_key != ? AND (\
                 poster_relative_path = ? OR season_poster_relative_path = ? OR \
                 backdrop_relative_path = ? OR cover_relative_path = ? OR artist_art_relative_path = ?)",
            )
            .bind(entry_key)
            .bind(&path)
            .bind(&path)
            .bind(&path)
            .bind(&path)
            .bind(&path)
            .fetch_one(&self.pool)
            .await?;
            if references == 0 {
                unshared_artwork_paths.push(path);
            }
        }

        Ok(Some(AssetDeletionManifest {
            entry,
            artwork_paths,
            unshared_artwork_paths,
            subtitle_tracks: self.subtitle_tracks(entry_key).await?,
        }))
    }

    pub async fn get(&self, entry_key: &str) -> sqlx::Result<Option<EntryRecord>> {
        let row = sqlx::query_as::<_, EntryRow>(&format!(
            "{ENTRY_SELECT} WHERE entry_key = ? AND available = 1"
        ))
        .bind(entry_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(EntryRecord::from))
    }

    pub async fn list(&self) -> sqlx::Result<Vec<EntryRecord>> {
        let rows = sqlx::query_as::<_, EntryRow>(&format!(
            "{ENTRY_SELECT} WHERE available = 1 ORDER BY relative_path"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(EntryRecord::from).collect())
    }

    /// Make every movie/episode with a known duration eligible for local
    /// transcription. Existing work for the same fingerprint/model/language
    /// is preserved; changed media is reset atomically to a fresh job.
    /// `skip_if_subtitles_exist` leaves any entry with an existing subtitle
    /// track (of any source, not just Whisper) untouched — the bulk "skip"
    /// preference. A targeted, user-triggered request always goes through
    /// [`Self::enqueue_transcription_for_entry`] instead, which ignores it.
    pub async fn enqueue_missing_transcriptions(
        &self,
        model: &str,
        language: &str,
        segment_duration_secs: u64,
        skip_if_subtitles_exist: bool,
    ) -> sqlx::Result<u64> {
        let now_ms = unix_time_ms();
        let segment_secs = segment_duration_secs.max(1) as f64;
        let skip_clause = if skip_if_subtitles_exist {
            " AND NOT EXISTS (SELECT 1 FROM subtitle_tracks st WHERE st.entry_key = entries.entry_key)"
        } else {
            ""
        };
        let needs_queue = format!(
            "entries.available = 1 AND entries.kind IN ('movie', 'episode') \
             AND entries.audio_json IS NOT NULL AND entries.duration_secs > 0 \
             AND (jobs.entry_key IS NULL OR jobs.fingerprint != entries.fingerprint \
                  OR jobs.model != ? OR jobs.language != ?){skip_clause}"
        );
        let count_sql = format!(
            "SELECT COUNT(*) FROM library_entries entries \
             LEFT JOIN transcription_jobs jobs ON jobs.entry_key = entries.entry_key \
             WHERE {needs_queue}"
        );
        let (queued,): (i64,) = sqlx::query_as(&count_sql)
            .bind(model)
            .bind(language)
            .fetch_one(&self.pool)
            .await?;

        let mut transaction = self.pool.begin().await?;
        let delete_subtitles_sql = format!(
            "DELETE FROM subtitle_tracks WHERE source = 'whisper' AND entry_key IN (\
                SELECT entries.entry_key FROM library_entries entries \
                LEFT JOIN transcription_jobs jobs ON jobs.entry_key = entries.entry_key \
                WHERE {needs_queue}\
             )"
        );
        sqlx::query(&delete_subtitles_sql)
            .bind(model)
            .bind(language)
            .execute(&mut *transaction)
            .await?;

        // Deleting a stale job cascades its old segment rows. New/missing
        // jobs have no row to delete and are inserted by the next statement.
        let delete_jobs_sql = format!(
            "DELETE FROM transcription_jobs WHERE entry_key IN (\
                SELECT entries.entry_key FROM library_entries entries \
                JOIN transcription_jobs jobs ON jobs.entry_key = entries.entry_key \
                WHERE entries.available = 1 AND entries.kind IN ('movie', 'episode') \
                  AND entries.audio_json IS NOT NULL AND entries.duration_secs > 0 \
                  AND (jobs.fingerprint != entries.fingerprint OR jobs.model != ? OR jobs.language != ?){skip_clause}\
             )"
        );
        sqlx::query(&delete_jobs_sql)
            .bind(model)
            .bind(language)
            .execute(&mut *transaction)
            .await?;

        let insert_jobs_sql = format!(
            "INSERT INTO transcription_jobs \
                (entry_key, fingerprint, model, language, status, total_segments, \
                 completed_segments, created_at_ms, updated_at_ms) \
             SELECT entries.entry_key, entries.fingerprint, ?, ?, 'queued', \
                CAST(entries.duration_secs / ? AS INTEGER) + \
                  CASE WHEN entries.duration_secs > CAST(entries.duration_secs / ? AS INTEGER) * ? \
                       THEN 1 ELSE 0 END, \
                0, ?, ? \
             FROM library_entries entries \
             LEFT JOIN transcription_jobs jobs ON jobs.entry_key = entries.entry_key \
             WHERE entries.available = 1 AND entries.kind IN ('movie', 'episode') \
               AND entries.audio_json IS NOT NULL AND entries.duration_secs > 0 \
               AND jobs.entry_key IS NULL{skip_clause}"
        );
        sqlx::query(&insert_jobs_sql)
            .bind(model)
            .bind(language)
            .bind(segment_secs)
            .bind(segment_secs)
            .bind(segment_secs)
            .bind(now_ms)
            .bind(now_ms)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(queued.max(0) as u64)
    }

    /// Force one entry into the transcription queue regardless of any
    /// existing completed/failed job or existing subtitle track — the
    /// "targeted creation" entry point for a user clicking "Generate
    /// subtitles" on a specific movie/episode. Returns `false` when the
    /// entry doesn't exist or isn't eligible (wrong kind, no audio, unknown
    /// duration).
    pub async fn enqueue_transcription_for_entry(
        &self,
        entry_key: &str,
        model: &str,
        language: &str,
        segment_duration_secs: u64,
    ) -> sqlx::Result<bool> {
        let now_ms = unix_time_ms();
        let segment_secs = segment_duration_secs.max(1) as f64;
        let mut transaction = self.pool.begin().await?;
        let eligible: Option<(String, f64)> = sqlx::query_as(
            "SELECT fingerprint, duration_secs FROM library_entries \
             WHERE entry_key = ? AND available = 1 AND kind IN ('movie', 'episode') \
               AND audio_json IS NOT NULL AND duration_secs > 0",
        )
        .bind(entry_key)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((fingerprint, duration_secs)) = eligible else {
            transaction.rollback().await?;
            return Ok(false);
        };
        sqlx::query("DELETE FROM subtitle_tracks WHERE entry_key = ? AND source = 'whisper'")
            .bind(entry_key)
            .execute(&mut *transaction)
            .await?;
        // Cascades any existing segment rows for this entry.
        sqlx::query("DELETE FROM transcription_jobs WHERE entry_key = ?")
            .bind(entry_key)
            .execute(&mut *transaction)
            .await?;
        let total_segments = (duration_secs / segment_secs).ceil().max(1.0) as i64;
        sqlx::query(
            "INSERT INTO transcription_jobs \
                (entry_key, fingerprint, model, language, status, total_segments, \
                 completed_segments, created_at_ms, updated_at_ms) \
             VALUES (?, ?, ?, ?, 'queued', ?, 0, ?, ?)",
        )
        .bind(entry_key)
        .bind(&fingerprint)
        .bind(model)
        .bind(language)
        .bind(total_segments)
        .bind(now_ms)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    /// A process exit can leave a claimed job marked transcribing/finalizing.
    /// Completed segment rows are durable, so startup only needs to requeue
    /// the job; it resumes at the first missing segment.
    pub async fn recover_interrupted_transcriptions(&self) -> sqlx::Result<()> {
        sqlx::query(
            "UPDATE transcription_jobs SET status = 'queued', error = NULL, updated_at_ms = ? \
             WHERE status IN ('transcribing','finalizing')",
        )
        .bind(unix_time_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn claim_next_transcription(&self) -> sqlx::Result<Option<TranscriptionJob>> {
        let mut transaction = self.pool.begin().await?;
        type Row = (String, String, String, String, i64, i64);
        let row: Option<Row> = sqlx::query_as(
            "SELECT jobs.entry_key, jobs.fingerprint, jobs.model, jobs.language, \
                    jobs.total_segments, jobs.completed_segments \
             FROM transcription_jobs jobs \
             JOIN library_entries entries ON entries.entry_key = jobs.entry_key \
             WHERE jobs.status = 'queued' AND entries.available = 1 \
             ORDER BY jobs.created_at_ms, jobs.entry_key LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((entry_key, fingerprint, model, language, total_segments, completed_segments)) =
            row
        else {
            transaction.commit().await?;
            return Ok(None);
        };
        sqlx::query("UPDATE transcription_jobs SET status = 'transcribing', updated_at_ms = ? WHERE entry_key = ?")
            .bind(unix_time_ms())
            .bind(&entry_key)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(Some(TranscriptionJob {
            entry_key,
            fingerprint,
            model,
            language,
            total_segments: total_segments as u32,
            completed_segments: completed_segments as u32,
        }))
    }

    pub async fn requeue_transcription(&self, entry_key: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE transcription_jobs SET status = 'queued', updated_at_ms = ? WHERE entry_key = ?")
            .bind(unix_time_ms())
            .bind(entry_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn fail_transcription(&self, entry_key: &str, error: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE transcription_jobs SET status = 'failed', error = ?, updated_at_ms = ? WHERE entry_key = ?")
            .bind(error)
            .bind(unix_time_ms())
            .bind(entry_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn store_transcription_segment(
        &self,
        entry_key: &str,
        segment_index: u32,
        cues_json: &str,
    ) -> sqlx::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO transcription_segments (entry_key, segment_index, cues_json, completed_at_ms) VALUES (?, ?, ?, ?) \
             ON CONFLICT(entry_key, segment_index) DO UPDATE SET cues_json = excluded.cues_json, completed_at_ms = excluded.completed_at_ms",
        )
        .bind(entry_key)
        .bind(i64::from(segment_index))
        .bind(cues_json)
        .bind(unix_time_ms())
        .execute(&mut *transaction)
        .await?;
        let (completed,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM transcription_segments WHERE entry_key = ?")
                .bind(entry_key)
                .fetch_one(&mut *transaction)
                .await?;
        sqlx::query("UPDATE transcription_jobs SET completed_segments = ?, updated_at_ms = ? WHERE entry_key = ?")
            .bind(completed)
            .bind(unix_time_ms())
            .bind(entry_key)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn transcription_segments(
        &self,
        entry_key: &str,
    ) -> sqlx::Result<Vec<(u32, String)>> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT segment_index, cues_json FROM transcription_segments WHERE entry_key = ? ORDER BY segment_index",
        )
        .bind(entry_key)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(index, json)| (index as u32, json))
            .collect())
    }

    pub async fn complete_transcription(&self, subtitle: &SubtitleRecord) -> sqlx::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO subtitle_tracks \
             (entry_key, id, language, label, source, format, file_path, fingerprint, generated_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(entry_key, id) DO UPDATE SET language = excluded.language, label = excluded.label, \
             source = excluded.source, format = excluded.format, file_path = excluded.file_path, \
             fingerprint = excluded.fingerprint, generated_at_ms = excluded.generated_at_ms",
        )
        .bind(&subtitle.entry_key)
        .bind(&subtitle.id)
        .bind(&subtitle.language)
        .bind(&subtitle.label)
        .bind(&subtitle.source)
        .bind(&subtitle.format)
        .bind(&subtitle.file_path)
        .bind(&subtitle.fingerprint)
        .bind(unix_time_ms())
        .execute(&mut *transaction)
        .await?;
        sqlx::query("UPDATE transcription_jobs SET status = 'completed', completed_segments = total_segments, error = NULL, updated_at_ms = ? WHERE entry_key = ?")
            .bind(unix_time_ms())
            .bind(&subtitle.entry_key)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Register a completed subtitle from a non-Whisper source. Playback
    /// treats every row in `subtitle_tracks` uniformly, while transcription
    /// job state remains untouched.
    pub async fn upsert_subtitle(&self, subtitle: &SubtitleRecord) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO subtitle_tracks \
             (entry_key, id, language, label, source, format, file_path, fingerprint, generated_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(entry_key, id) DO UPDATE SET language = excluded.language, label = excluded.label, \
             source = excluded.source, format = excluded.format, file_path = excluded.file_path, \
             fingerprint = excluded.fingerprint, generated_at_ms = excluded.generated_at_ms",
        )
        .bind(&subtitle.entry_key)
        .bind(&subtitle.id)
        .bind(&subtitle.language)
        .bind(&subtitle.label)
        .bind(&subtitle.source)
        .bind(&subtitle.format)
        .bind(&subtitle.file_path)
        .bind(&subtitle.fingerprint)
        .bind(unix_time_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn subtitle_tracks(&self, entry_key: &str) -> sqlx::Result<Vec<SubtitleRecord>> {
        type Row = (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, entry_key, language, label, source, format, file_path, fingerprint \
             FROM subtitle_tracks WHERE entry_key = ? ORDER BY language, label",
        )
        .bind(entry_key)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, entry_key, language, label, source, format, file_path, fingerprint)| {
                    SubtitleRecord {
                        id,
                        entry_key,
                        language,
                        label,
                        source,
                        format,
                        file_path,
                        fingerprint,
                    }
                },
            )
            .collect())
    }

    pub async fn subtitle_track(
        &self,
        entry_key: &str,
        id: &str,
    ) -> sqlx::Result<Option<SubtitleRecord>> {
        Ok(self
            .subtitle_tracks(entry_key)
            .await?
            .into_iter()
            .find(|track| track.id == id))
    }

    /// Every subtitle track across the whole library from a given source
    /// (e.g. `"whisper"`) — used by the one-off storage-location migration
    /// script rather than by the running server.
    pub async fn subtitle_tracks_by_source(
        &self,
        source: &str,
    ) -> sqlx::Result<Vec<SubtitleRecord>> {
        type Row = (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, entry_key, language, label, source, format, file_path, fingerprint \
             FROM subtitle_tracks WHERE source = ? ORDER BY entry_key",
        )
        .bind(source)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, entry_key, language, label, source, format, file_path, fingerprint)| {
                    SubtitleRecord {
                        id,
                        entry_key,
                        language,
                        label,
                        source,
                        format,
                        file_path,
                        fingerprint,
                    }
                },
            )
            .collect())
    }

    /// Available movie/episode entries whose file lives anywhere under
    /// `dir` (a stored `relative_path` directory, forward slashes, with the
    /// `{label}/` prefix in a multi-root install). An empty `dir` means the
    /// whole library. Used by subtitle sidecar matching to gather the
    /// candidate videos a `Subs/` file could belong to.
    pub async fn video_entries_under_dir(&self, dir: &str) -> sqlx::Result<Vec<EntryRecord>> {
        let rows = if dir.is_empty() {
            sqlx::query_as::<_, EntryRow>(&format!(
                "{ENTRY_SELECT} WHERE available = 1 AND kind IN ('movie', 'episode') \
                 ORDER BY relative_path"
            ))
            .fetch_all(&self.pool)
            .await?
        } else {
            let prefix = format!("{dir}/");
            sqlx::query_as::<_, EntryRow>(&format!(
                "{ENTRY_SELECT} WHERE available = 1 AND kind IN ('movie', 'episode') \
                 AND substr(relative_path, 1, ?) = ? ORDER BY relative_path"
            ))
            .bind(prefix.chars().count() as i64)
            .bind(prefix)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows.into_iter().map(EntryRecord::from).collect())
    }

    /// Reconcile the side-loaded (`source = 'external'`) subtitle tracks for
    /// every entry under `prefix` (`None` = whole library; `Some("label")` =
    /// one multi-root label) to exactly `records`. Runs in one transaction
    /// so a rescan never leaves playback seeing a half-updated sidecar set.
    /// Whisper/OpenSubtitles tracks are never touched.
    pub async fn replace_external_subtitles(
        &self,
        prefix: Option<&str>,
        records: &[SubtitleRecord],
    ) -> sqlx::Result<()> {
        let mut transaction = self.pool.begin().await?;
        match prefix {
            Some(label) => {
                let label_prefix = format!("{label}/");
                sqlx::query(
                    "DELETE FROM subtitle_tracks WHERE source = 'external' AND entry_key IN \
                     (SELECT entry_key FROM library_entries WHERE substr(relative_path, 1, ?) = ?)",
                )
                .bind(label_prefix.chars().count() as i64)
                .bind(label_prefix)
                .execute(&mut *transaction)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM subtitle_tracks WHERE source = 'external'")
                    .execute(&mut *transaction)
                    .await?;
            }
        }
        let now = unix_time_ms();
        for record in records {
            sqlx::query(
                "INSERT INTO subtitle_tracks \
                 (entry_key, id, language, label, source, format, file_path, fingerprint, generated_at_ms) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(entry_key, id) DO UPDATE SET language = excluded.language, \
                 label = excluded.label, source = excluded.source, format = excluded.format, \
                 file_path = excluded.file_path, fingerprint = excluded.fingerprint, \
                 generated_at_ms = excluded.generated_at_ms",
            )
            .bind(&record.entry_key)
            .bind(&record.id)
            .bind(&record.language)
            .bind(&record.label)
            .bind(&record.source)
            .bind(&record.format)
            .bind(&record.file_path)
            .bind(&record.fingerprint)
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await
    }

    pub async fn transcription_queue_status(&self) -> sqlx::Result<TranscriptionQueueStatus> {
        type Row = (i64, i64, i64, i64, i64);
        let (queued, completed, failed, total_segments, completed_segments): Row = sqlx::query_as(
            "SELECT \
                COALESCE(SUM(CASE WHEN jobs.status IN ('queued','transcribing','finalizing') THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN jobs.status = 'completed' THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN jobs.status = 'failed' THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(jobs.total_segments), 0), COALESCE(SUM(jobs.completed_segments), 0) \
             FROM transcription_jobs jobs \
             JOIN library_entries entries ON entries.entry_key = jobs.entry_key \
             WHERE entries.available = 1",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(TranscriptionQueueStatus {
            queued: queued as u64,
            completed: completed as u64,
            failed: failed as u64,
            total_segments: total_segments as u64,
            completed_segments: completed_segments as u64,
        })
    }

    /// Every track sharing exactly this (artist, album) pair — the sibling
    /// set a pinpoint music rescrape re-syncs together, matching bulk
    /// scrape's per-album (not per-track) grouping.
    pub async fn entries_by_artist_album(
        &self,
        artist: &str,
        album: &str,
    ) -> sqlx::Result<Vec<EntryRecord>> {
        let rows = sqlx::query_as::<_, EntryRow>(&format!(
            "{ENTRY_SELECT} WHERE available = 1 AND artist = ? AND album = ? ORDER BY relative_path"
        ))
        .bind(artist)
        .bind(album)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(EntryRecord::from).collect())
    }

    pub async fn pending_changes(&self) -> sqlx::Result<Vec<PendingChange>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT entry_key, operation FROM library_changes ORDER BY entry_key")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(entry_key, operation)| PendingChange {
                entry_key,
                operation,
            })
            .collect())
    }

    pub async fn clear_pending_changes(&self) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM library_changes")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Atomically replace the community playback markers associated with a
    /// TMDb match. An empty vector is meaningful: TheIntroDB was checked and
    /// has no accepted data, which prevents needless repeat requests.
    pub async fn set_introdb_segments(
        &self,
        entry_key: &str,
        tmdb_id: u64,
        segments: &[SkipSegment],
    ) -> sqlx::Result<()> {
        let json = serde_json::to_string(segments).unwrap_or_else(|_| "[]".into());
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE library_entries SET tmdb_id = ?, skip_segments_json = ? WHERE entry_key = ?",
        )
        .bind(tmdb_id as i64)
        .bind(json)
        .bind(entry_key)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO library_changes (entry_key, operation) VALUES (?, 'upsert') \
             ON CONFLICT(entry_key) DO UPDATE SET operation = 'upsert'",
        )
        .bind(entry_key)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await
    }

    /// Clear stale segment data when TMDb no longer matches the asset.
    pub async fn clear_introdb_segments(&self, entry_key: &str) -> sqlx::Result<()> {
        sqlx::query(
            "UPDATE library_entries SET tmdb_id = NULL, skip_segments_json = NULL WHERE entry_key = ?",
        )
        .bind(entry_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Leave a transient IntroDB failure eligible for the next automatic
    /// metadata run without throwing away the successful TMDb scrape.
    pub async fn mark_introdb_retry(&self, entry_key: &str) -> sqlx::Result<()> {
        sqlx::query(
            "UPDATE library_entries SET scrape_version = ? WHERE entry_key = ? AND scrape_version >= ?",
        )
        .bind(CURRENT_SCRAPE_VERSION - 1)
        .bind(entry_key)
        .bind(CURRENT_SCRAPE_VERSION)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn introdb_segments(&self) -> sqlx::Result<HashMap<String, Vec<SkipSegment>>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT entry_key, skip_segments_json FROM library_entries \
             WHERE available = 1 AND skip_segments_json IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(entry_key, json)| {
                let segments = serde_json::from_str(&json).unwrap_or_default();
                (entry_key, segments)
            })
            .collect())
    }

    /// TheIntroDB caching state for a single asset, for the Media detail
    /// page's metadata checklist. `None` means the scraper has never recorded
    /// a TheIntroDB lookup for this entry; `Some(segments)` means it has, and
    /// an empty vector is a real result: TheIntroDB was queried and published
    /// no accepted markers for the asset.
    pub async fn introdb_segments_for(
        &self,
        entry_key: &str,
    ) -> sqlx::Result<Option<Vec<SkipSegment>>> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT skip_segments_json FROM library_entries WHERE entry_key = ?",
        )
        .bind(entry_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .and_then(|(json,)| json)
            .map(|json| serde_json::from_str(&json).unwrap_or_default()))
    }

    /// The exact catalog payload plus its stable SHA-256 version token.
    ///
    /// This intentionally hashes every client-visible field, including
    /// scraped metadata, artwork versions, and aggregate like counts. The
    /// old `(path, fingerprint, size)`-only token stayed unchanged while a
    /// scraper populated artwork/titles, which made a fingerprint-aware TV
    /// cache retain stale presentation data indefinitely.
    pub async fn catalog_snapshot(&self) -> sqlx::Result<(String, Vec<CatalogEntry>)> {
        let entries = self.list().await?;
        let like_counts = self.like_counts().await?;
        let introdb_segments = self.introdb_segments().await?;
        let catalog_entries: Vec<CatalogEntry> = entries
            .iter()
            .map(|entry| {
                let mut catalog_entry = entry.to_catalog_entry();
                catalog_entry.like_count = like_counts.get(&entry.entry_key).copied().unwrap_or(0);
                catalog_entry.skip_segments = introdb_segments
                    .get(&entry.entry_key)
                    .cloned()
                    .unwrap_or_default();
                catalog_entry
            })
            .collect();
        let mut digest = Sha256::new();
        for entry in &catalog_entries {
            // CatalogEntry contains no fallible/custom serializer. Hashing
            // each entry independently avoids allocating one second copy of
            // the entire multi-megabyte manifest merely to version it.
            digest.update(serde_json::to_vec(entry).unwrap_or_default());
            digest.update(b"\n");
        }
        Ok((hex::encode(digest.finalize()), catalog_entries))
    }

    pub async fn thumbprint(&self) -> sqlx::Result<String> {
        self.catalog_snapshot()
            .await
            .map(|(thumbprint, _)| thumbprint)
    }

    pub async fn entry_count(&self) -> sqlx::Result<u64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM library_entries WHERE available = 1")
                .fetch_one(&self.pool)
                .await?;
        Ok(count as u64)
    }

    /// Entries a scraper has not yet resolved, plus entries last processed by
    /// an older metadata schema. The version clause is what backfills fields
    /// added after an existing library was already scraped.
    pub async fn missing_scrape(&self) -> sqlx::Result<Vec<EntryRecord>> {
        let rows = sqlx::query_as::<_, EntryRow>(&format!(
            "{ENTRY_SELECT} WHERE available = 1 AND (scraped_title IS NULL OR scrape_version < ?) ORDER BY relative_path"
        ))
            .bind(CURRENT_SCRAPE_VERSION)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(EntryRecord::from).collect())
    }

    /// Entries whose provider-backed presentation is incomplete. Unlike
    /// [`Self::missing_scrape`], this also revisits a successfully processed
    /// row when any metadata or artwork expected for that media kind is
    /// absent. A user-triggered normal scrape therefore repairs partial
    /// results without requiring the destructive "force re-scrape all"
    /// option.
    pub async fn incomplete_scrape(&self) -> sqlx::Result<Vec<EntryRecord>> {
        let rows = sqlx::query_as::<_, EntryRow>(&format!(
            "{ENTRY_SELECT} WHERE available = 1 AND (scrape_version < ? \
             OR (kind IN ('movie', 'episode') AND (\
                 scraped_title IS NULL OR TRIM(scraped_title) = '' \
                 OR genres_json IS NULL OR genres_json IN ('', '[]', 'null') \
                 OR cast_json IS NULL OR cast_json IN ('', '[]', 'null') \
                 OR overview IS NULL OR TRIM(overview) = '' \
                 OR rating IS NULL OR TRIM(rating) = '' \
                 OR community_rating IS NULL \
                 OR poster_relative_path IS NULL OR TRIM(poster_relative_path) = '' \
                 OR backdrop_relative_path IS NULL OR TRIM(backdrop_relative_path) = '' \
                 OR (kind = 'episode' AND (season_poster_relative_path IS NULL OR TRIM(season_poster_relative_path) = ''))\
             ))) \
             OR (kind = 'track' AND (\
                 genres_json IS NULL OR genres_json IN ('', '[]', 'null') \
                 OR community_rating IS NULL \
                 OR cover_relative_path IS NULL OR TRIM(cover_relative_path) = '' \
                 OR artist_art_relative_path IS NULL OR TRIM(artist_art_relative_path) = ''\
             )) \
             ORDER BY relative_path"
        ))
        .bind(CURRENT_SCRAPE_VERSION)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(EntryRecord::from).collect())
    }

    /// Tracks with enough tag/probe metadata for an exact LRCLIB lookup but
    /// no completed lyric lookup yet. A fresh no-match marker counts as
    /// completed so routine runs do not hammer the public service, then
    /// expires after 30 days because LRCLIB can acquire lyrics later.
    pub async fn missing_track_lyrics(&self) -> sqlx::Result<Vec<EntryRecord>> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        let not_found_cutoff_ms = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1_000);
        let rows = sqlx::query_as::<_, EntryRow>(&format!(
            "{ENTRY_SELECT} WHERE available = 1 AND kind = 'track' AND duration_secs IS NOT NULL \
             AND artist IS NOT NULL AND artist <> '' AND album IS NOT NULL AND album <> '' \
             AND NOT EXISTS (SELECT 1 FROM track_lyrics \
                 WHERE track_lyrics.entry_key = library_entries.entry_key \
                 AND (plain_lyrics IS NOT NULL OR synced_lyrics IS NOT NULL OR instrumental = 1 OR fetched_at_ms >= ?)) \
             ORDER BY relative_path"
        ))
        .bind(not_found_cutoff_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(EntryRecord::from).collect())
    }

    /// Cache an LRCLIB match. The entry foreign key makes stale lyrics leave
    /// automatically when a media file is removed from the library.
    pub async fn set_track_lyrics(
        &self,
        entry_key: &str,
        lyrics: &TrackLyrics,
    ) -> sqlx::Result<()> {
        let fetched_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        sqlx::query(
            "INSERT INTO track_lyrics \
             (entry_key, provider, provider_id, language, plain_lyrics, synced_lyrics, instrumental, fetched_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(entry_key) DO UPDATE SET provider = excluded.provider, provider_id = excluded.provider_id, \
             language = excluded.language, plain_lyrics = excluded.plain_lyrics, synced_lyrics = excluded.synced_lyrics, \
             instrumental = excluded.instrumental, fetched_at_ms = excluded.fetched_at_ms",
        )
        .bind(entry_key)
        .bind(&lyrics.provider)
        .bind(lyrics.provider_id)
        .bind(&lyrics.language)
        .bind(&lyrics.plain_lyrics)
        .bind(&lyrics.synced_lyrics)
        .bind(i64::from(lyrics.instrumental))
        .bind(fetched_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record a provider 404. This sentinel has no lyric text and is never
    /// returned to clients. Routine scrapes suppress it for 30 days, then
    /// retry because LRCLIB may have acquired the track in the meantime.
    pub async fn mark_track_lyrics_not_found(&self, entry_key: &str) -> sqlx::Result<()> {
        self.set_track_lyrics(
            entry_key,
            &TrackLyrics {
                provider: "lrclib".into(),
                provider_id: None,
                language: None,
                plain_lyrics: None,
                synced_lyrics: None,
                instrumental: false,
            },
        )
        .await
    }

    /// Cached lyrics for playback. A no-match marker returns `None`.
    pub async fn track_lyrics(&self, entry_key: &str) -> sqlx::Result<Option<TrackLyrics>> {
        type Row = (
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT provider, provider_id, language, plain_lyrics, synced_lyrics, instrumental \
             FROM track_lyrics WHERE entry_key = ?",
        )
        .bind(entry_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(
            |(provider, provider_id, language, plain_lyrics, synced_lyrics, instrumental)| {
                if plain_lyrics.is_none() && synced_lyrics.is_none() && instrumental == 0 {
                    None
                } else {
                    Some(TrackLyrics {
                        provider,
                        provider_id,
                        language,
                        plain_lyrics,
                        synced_lyrics,
                        instrumental: instrumental != 0,
                    })
                }
            },
        ))
    }

    /// Record that a scrape attempt completed for this entry — whether or not
    /// it found a match. `scraped_title` is a *display overlay*, never a
    /// grouping key: `None` means "processed, no title override" (covers
    /// both "no online match" and "matched but nothing to override," e.g.
    /// music tracks, which get release-level genres without a title
    /// override — see the runner). The sentinel is stored as `''` rather
    /// than `NULL` specifically so [`Self::missing_scrape`]'s `IS NULL`
    /// filter treats "processed" as done and never re-queues it.
    pub async fn set_scrape_result(
        &self,
        entry_key: &str,
        scraped_title: Option<&str>,
        genres: &[String],
        cast: &[CastMember],
        rating: Option<&str>,
        community_rating: Option<f64>,
        community_rating_votes: Option<u64>,
    ) -> sqlx::Result<()> {
        let genres_json = serde_json::to_string(genres).unwrap_or_else(|_| "[]".into());
        let cast_json = serde_json::to_string(cast).unwrap_or_else(|_| "[]".into());
        sqlx::query(
            "UPDATE library_entries SET scraped_title = ?, episode_title = NULL, genres_json = ?, cast_json = ?, \
             rating = ?, community_rating = ?, community_rating_votes = ?, scrape_version = ? \
             WHERE entry_key = ?",
        )
        .bind(scraped_title.unwrap_or(""))
        .bind(genres_json)
        .bind(cast_json)
        .bind(rating)
        .bind(community_rating)
        .bind(community_rating_votes.map(|votes| votes as i64))
        .bind(CURRENT_SCRAPE_VERSION)
        .bind(entry_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Manually override the display-only `scraped_title`/`genres_json`
    /// overlay — a GUI-driven correction, distinct from [`Self::set_scrape_result`]
    /// (scraper-only, always writes both together plus cast). `None` for
    /// either parameter means "leave that field's current value untouched",
    /// not "clear it" — a caller that wants to clear a title passes
    /// `Some("")`. Never touches `artist`/`album`/`track_number`/
    /// `show_title`/`season`/`episode` — those stay path-derived, the same
    /// grouping-key invariant the scraper itself is bound by (see
    /// `classify` module docs).
    pub async fn set_manual_metadata(
        &self,
        entry_key: &str,
        title: Option<&str>,
        genres: Option<&[String]>,
    ) -> sqlx::Result<()> {
        if let Some(title) = title {
            sqlx::query(
                "UPDATE library_entries SET scraped_title = ?, scrape_version = ? WHERE entry_key = ?",
            )
                .bind(title)
                .bind(CURRENT_SCRAPE_VERSION)
                .bind(entry_key)
                .execute(&self.pool)
                .await?;
        }
        if let Some(genres) = genres {
            let genres_json = serde_json::to_string(genres).unwrap_or_else(|_| "[]".into());
            sqlx::query(
                "UPDATE library_entries SET genres_json = ?, scrape_version = ? WHERE entry_key = ?",
            )
                .bind(genres_json)
                .bind(CURRENT_SCRAPE_VERSION)
                .bind(entry_key)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Manually moves an entry to a different [`MediaKind`] — the escape
    /// hatch for files `classify()` structurally can't get right from the
    /// path/extension alone (a music video sitting in a movies/shows folder
    /// as an .mkv, indistinguishable from a real movie/episode without
    /// actually watching it). Clears whichever grouping fields don't apply
    /// to the new kind (a former Track's `artist`/`album`/`track_number`
    /// don't belong on a Movie, a former Episode's `show_title`/`season`/
    /// `episode` don't either) so stale cross-kind data never lingers and
    /// confuses a later regroup. Sets `kind_overridden = 1`, which
    /// `reclassify_all` and `scan_roots` both check to keep this from being
    /// silently reverted by a later "Fix classifications" run or by the
    /// file changing on disk (re-encoded, replaced) — see [`KnownEntry`]'s
    /// doc comment for the latter.
    pub async fn set_manual_kind(
        &self,
        entry_key: &str,
        kind: MediaKind,
        artist: Option<&str>,
        album: Option<&str>,
        show_title: Option<&str>,
    ) -> sqlx::Result<()> {
        let (artist, album, show_title) = match kind {
            MediaKind::Track => (artist, album, None),
            MediaKind::Episode => (None, None, show_title),
            MediaKind::Movie => (None, None, None),
        };
        sqlx::query(
            "UPDATE library_entries SET kind = ?, artist = ?, album = ?, show_title = ?, \
             season = NULL, episode = NULL, episode_title = NULL, track_number = NULL, kind_overridden = 1 \
             WHERE entry_key = ?",
        )
        .bind(kind_str(kind))
        .bind(artist)
        .bind(album)
        .bind(show_title)
        .bind(entry_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Sets the synopsis directly — used both by a scrape (writing TMDb's
    /// own overview right after `set_scrape_result`) and by a manual GUI
    /// edit. Kept as its own method rather than folded into
    /// `set_scrape_result`/`set_manual_metadata`: tracks have no synopsis
    /// concept at all, so every existing call site for those two methods
    /// would otherwise need a new always-irrelevant parameter.
    pub async fn set_overview(&self, entry_key: &str, overview: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE library_entries SET overview = ? WHERE entry_key = ?")
            .bind(overview)
            .bind(entry_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Stores the episode/special title returned by TMDb while leaving the
    /// path-derived filename title and canonical show title untouched.
    pub async fn set_episode_title(
        &self,
        entry_key: &str,
        title: Option<&str>,
    ) -> sqlx::Result<()> {
        sqlx::query("UPDATE library_entries SET episode_title = ? WHERE entry_key = ?")
            .bind(title.filter(|value| !value.trim().is_empty()))
            .bind(entry_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Sets the US content rating directly — same status/call shape as
    /// [`Self::set_overview`] (a scrape writes TMDb's own certification
    /// right after `set_scrape_result`; a manual GUI edit calls this too).
    pub async fn set_rating(&self, entry_key: &str, rating: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE library_entries SET rating = ? WHERE entry_key = ?")
            .bind(rating)
            .bind(entry_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Every distinct, non-empty genre value currently assigned to any
    /// entry, sorted case-insensitively — backs the GUI's category picker
    /// ("assign to an existing category" instead of retyping one from
    /// scratch each time). Genres are stored per-entry as a JSON array
    /// rather than a normalized join table (matching how `cast`/`genres`
    /// have always been treated here — display overlay, not a relational
    /// concept), so this reads every row and unions them in memory; fine at
    /// library scale (a few thousand rows), called only when the GUI opens
    /// the picker, not on any hot path.
    pub async fn distinct_genres(&self) -> sqlx::Result<Vec<String>> {
        let rows: Vec<(Option<String>,)> = sqlx::query_as(
            "SELECT DISTINCT genres_json FROM library_entries WHERE available = 1 AND genres_json IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (genres_json,) in rows {
            let Some(genres_json) = genres_json else {
                continue;
            };
            let genres: Vec<String> = serde_json::from_str(&genres_json).unwrap_or_default();
            for g in genres {
                if !g.is_empty() {
                    set.insert(g);
                }
            }
        }
        let mut list: Vec<String> = set.into_iter().collect();
        list.sort_by_key(|g| g.to_lowercase());
        Ok(list)
    }

    /// Store a downloaded artwork image's path (relative to the media root,
    /// alongside the source file) and bump the version that backs its etag.
    pub async fn set_artwork(
        &self,
        entry_key: &str,
        kind: ArtworkKind,
        relative_path: &str,
    ) -> sqlx::Result<()> {
        let sql = format!(
            "UPDATE library_entries SET {} = ?, artwork_version = artwork_version + 1 WHERE entry_key = ?",
            kind.column()
        );
        sqlx::query(&sql)
            .bind(relative_path)
            .bind(entry_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The stored path and version (etag material) for one artwork slot, if
    /// downloaded. One query, since `serve.rs` needs both together.
    pub async fn artwork(
        &self,
        entry_key: &str,
        kind: ArtworkKind,
    ) -> sqlx::Result<Option<(String, u32)>> {
        let sql = format!(
            "SELECT {}, artwork_version FROM library_entries WHERE entry_key = ?",
            kind.column()
        );
        let row: Option<(Option<String>, i64)> = sqlx::query_as(&sql)
            .bind(entry_key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|(path, version)| path.map(|p| (p, version as u32))))
    }

    /// Reverts a bad scrape: clears the display-overlay `scraped_title`/
    /// `genres`/`cast` and every artwork slot back to unscraped, so the
    /// entry becomes eligible for `missing_scrape` again (the same `IS NULL`
    /// sentinel a fresh, never-scraped entry has). Never touches the
    /// path-derived grouping fields (`artist`/`album`/`show_title`/etc.) —
    /// same invariant every other scrape-writing method here already keeps.
    /// Returns the artwork relative paths that were cleared, if any, so the
    /// caller can best-effort delete the now-orphaned files on disk.
    pub async fn clear_scrape_result(&self, entry_key: &str) -> sqlx::Result<Vec<String>> {
        // Preserve a rollback copy, but do not let a deliberately cleared or
        // classification-invalidated result silently restore itself later.
        self.archive_metadata(entry_key, false).await?;
        let mut transaction = self.pool.begin().await?;
        type ArtworkPathRow = (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let row: ArtworkPathRow = sqlx::query_as(
            "SELECT poster_relative_path, season_poster_relative_path, backdrop_relative_path, cover_relative_path, artist_art_relative_path \
             FROM library_entries WHERE entry_key = ?",
        )
        .bind(entry_key)
        .fetch_one(&mut *transaction)
        .await?;
        let cleared_paths: Vec<String> = [row.0, row.1, row.2, row.3, row.4]
            .into_iter()
            .flatten()
            .collect();

        sqlx::query(
            "UPDATE library_entries SET scraped_title = NULL, episode_title = NULL, genres_json = NULL, cast_json = NULL, overview = NULL, \
             rating = NULL, community_rating = NULL, community_rating_votes = NULL, tmdb_id = NULL, skip_segments_json = NULL, scrape_version = 0, \
             poster_relative_path = NULL, season_poster_relative_path = NULL, backdrop_relative_path = NULL, cover_relative_path = NULL, \
             artist_art_relative_path = NULL, artwork_version = artwork_version + 1 WHERE entry_key = ?",
        )
        .bind(entry_key)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM track_lyrics WHERE entry_key = ?")
            .bind(entry_key)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(cleared_paths)
    }

    /// Re-derives every entry's classification (`kind`/`title`/`artist`/
    /// `album`/`track_number`/`show_title`/`season`/`episode`/`year`) from
    /// its already-stored `relative_path` alone — no filesystem access, so
    /// this is fast even against a large library and safe to run any time.
    ///
    /// `scan_roots` only ever calls `classify()` for a file it hasn't seen
    /// before or whose size/mtime changed (see its `unchanged` fast path) —
    /// an already-known, on-disk-unchanged file is never re-classified, so a
    /// `classify()` bug fix alone never repairs already-scanned entries.
    /// This exists specifically to repair them, on demand, without needing
    /// the underlying file to change.
    ///
    /// An entry is left completely untouched (including its existing scrape
    /// data) unless its `kind`/`show_title`/`season`/`episode`/`artist`/
    /// `album`/`track_number` actually differ from what it's currently
    /// stored as. When they do differ, the old scrape result can no longer
    /// be trusted (it was very possibly produced by searching under the
    /// wrong classification entirely — e.g. bonus content scraped as if it
    /// were a standalone movie, or a track scraped under a garbage
    /// folder-derived artist/album) — this reuses
    /// [`Self::clear_scrape_result`] for exactly those entries, so they come
    /// out the other side freshly eligible for a correct re-scrape.
    pub async fn reclassify_all(
        &self,
        roots: &crate::roots::SharedRootResolver,
    ) -> sqlx::Result<ReclassifyReport> {
        type ReclassifyRow = (
            String,
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<i64>,
            i64,
        );
        let mut report = ReclassifyReport::default();
        let rows: Vec<ReclassifyRow> = sqlx::query_as(
            "SELECT entry_key, relative_path, kind, show_title, season, episode, artist, album, track_number, kind_overridden \
             FROM library_entries WHERE available = 1",
        )
        .fetch_all(&self.pool)
        .await?;

        for (
            entry_key,
            relative_path,
            old_kind,
            old_show_title,
            old_season,
            old_episode,
            old_artist,
            old_album,
            old_track_number,
            kind_overridden,
        ) in rows
        {
            // A manually-reclassified entry (see `set_manual_kind`) must
            // never be silently reverted by a bulk path-based re-derivation
            // — that's the entire reason this flag exists.
            if kind_overridden != 0 {
                report.unchanged += 1;
                continue;
            }
            // Classified from the path *under its owning root*, never the
            // possibly `{label}/`-prefixed stored form — see scan_roots's
            // identical reasoning for why classify()'s top-anchored audio
            // grouping must never see a root's label as though it were a
            // real folder.
            let (_, path_under_root) = roots.split(&relative_path);
            let Some(classified) = crate::classify::classify(&path_under_root) else {
                continue;
            };
            let new_kind = kind_str(classified.kind);
            let new_season = classified.season.map(i64::from);
            let new_episode = classified.episode.map(i64::from);
            let new_track_number = classified.track_number.map(i64::from);
            if new_kind == old_kind
                && classified.show_title == old_show_title
                && new_season == old_season
                && new_episode == old_episode
                && classified.artist == old_artist
                && classified.album == old_album
                && new_track_number == old_track_number
            {
                report.unchanged += 1;
                continue;
            }

            sqlx::query(
                "UPDATE library_entries SET kind = ?, title = ?, artist = ?, album = ?, track_number = ?, \
                 show_title = ?, season = ?, episode = ?, year = ? WHERE entry_key = ?",
            )
            .bind(new_kind)
            .bind(&classified.title)
            .bind(&classified.artist)
            .bind(&classified.album)
            .bind(classified.track_number.map(i64::from))
            .bind(&classified.show_title)
            .bind(new_season)
            .bind(new_episode)
            .bind(classified.year.map(i64::from))
            .bind(&entry_key)
            .execute(&self.pool)
            .await?;
            self.clear_scrape_result(&entry_key).await?;
            report.changed += 1;
        }
        Ok(report)
    }

    /// Persists a client-reported error (`/errors/report`) for later triage
    /// from the swarm page. `received_at_ms` is stamped here, from this
    /// machine's own clock — see [`ClientErrorRecord`]'s doc comment.
    pub async fn record_client_error(
        &self,
        report: &swarm_core::peer::ClientErrorReport,
    ) -> sqlx::Result<()> {
        let received_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        sqlx::query(
            "INSERT INTO client_errors \
             (device_id, device_name, entry_key, asset_title, kind, message, context, occurred_at_ms, received_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&report.device_id)
        .bind(&report.device_name)
        .bind(&report.entry_key)
        .bind(&report.asset_title)
        .bind(&report.kind)
        .bind(&report.message)
        .bind(&report.context)
        .bind(report.occurred_at_ms)
        .bind(received_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Newest first — matches how the swarm page's Errors panel wants to
    /// present them (most recent triage-worthy thing first).
    pub async fn list_client_errors(&self) -> sqlx::Result<Vec<ClientErrorRecord>> {
        type Row = (
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            i64,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, device_id, device_name, entry_key, asset_title, kind, message, context, occurred_at_ms, received_at_ms, \
                    resolution_comments, resolved_at_ms, dismissed_at_ms \
             FROM client_errors ORDER BY received_at_ms DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    device_id,
                    device_name,
                    entry_key,
                    asset_title,
                    kind,
                    message,
                    context,
                    occurred_at_ms,
                    received_at_ms,
                    resolution_comments,
                    resolved_at_ms,
                    dismissed_at_ms,
                )| {
                    ClientErrorRecord {
                        id,
                        device_id,
                        device_name,
                        entry_key,
                        asset_title,
                        kind,
                        message,
                        context,
                        occurred_at_ms,
                        received_at_ms,
                        resolution_comments,
                        resolved_at_ms,
                        dismissed_at_ms,
                    }
                },
            )
            .collect())
    }

    pub async fn client_error_count(&self) -> sqlx::Result<u64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM client_errors WHERE resolved_at_ms IS NULL")
                .fetch_one(&self.pool)
                .await?;
        Ok(count as u64)
    }

    /// Marks an open report resolved. Repeating the action is idempotent and
    /// does not change the original resolution time or re-notify the client.
    pub async fn resolve_client_error(
        &self,
        id: i64,
        comments: Option<&str>,
    ) -> sqlx::Result<bool> {
        let comments = comments.map(str::trim).filter(|value| !value.is_empty());
        let result = sqlx::query(
            "UPDATE client_errors SET resolution_comments = ?, resolved_at_ms = ? \
             WHERE id = ? AND resolved_at_ms IS NULL",
        )
        .bind(comments)
        .bind(unix_time_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_client_resolution_notifications(
        &self,
        device_id: &str,
    ) -> sqlx::Result<Vec<ClientResolutionNotification>> {
        type Row = (i64, Option<String>, String, Option<String>, i64);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, asset_title, message, resolution_comments, resolved_at_ms \
             FROM client_errors \
             WHERE device_id = ? AND resolved_at_ms IS NOT NULL AND dismissed_at_ms IS NULL \
             ORDER BY resolved_at_ms DESC, id DESC",
        )
        .bind(device_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, asset_title, original_message, comments, resolved_at_ms)| {
                    ClientResolutionNotification {
                        id,
                        asset_title,
                        original_message,
                        comments,
                        resolved_at_ms,
                    }
                },
            )
            .collect())
    }

    pub async fn dismiss_client_resolution_notification(
        &self,
        device_id: &str,
        id: i64,
    ) -> sqlx::Result<bool> {
        let result = sqlx::query(
            "UPDATE client_errors SET dismissed_at_ms = ? \
             WHERE id = ? AND device_id = ? AND resolved_at_ms IS NOT NULL",
        )
        .bind(unix_time_ms())
        .bind(id)
        .bind(device_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_client_error(&self, id: i64) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM client_errors WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn clear_client_errors(&self) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM client_errors")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_server_notification(
        &self,
        level: &str,
        title: &str,
        message: &str,
    ) -> sqlx::Result<()> {
        let created_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        sqlx::query(
            "INSERT INTO server_notifications (level, title, message, created_at_ms) VALUES (?, ?, ?, ?)",
        )
        .bind(level)
        .bind(title)
        .bind(message)
        .bind(created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_server_notifications(&self) -> sqlx::Result<Vec<ServerNotificationRecord>> {
        let rows: Vec<(i64, String, String, String, i64)> = sqlx::query_as(
            "SELECT id, level, title, message, created_at_ms FROM server_notifications ORDER BY created_at_ms DESC, id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, level, title, message, created_at_ms)| ServerNotificationRecord {
                    id,
                    level,
                    title,
                    message,
                    created_at_ms,
                },
            )
            .collect())
    }

    pub async fn server_notification_count(&self) -> sqlx::Result<u64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM server_notifications")
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u64)
    }

    pub async fn delete_server_notification(&self, id: i64) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM server_notifications WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn clear_server_notifications(&self) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM server_notifications")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Idempotent toggle (`/likes/toggle`): `liked = true` upserts this
    /// device's like, `liked = false` removes it — safe to send the same
    /// desired end-state repeatedly (a D-pad button retry) without double-
    /// counting or erroring on "already liked"/"wasn't liked". Device
    /// identity is self-reported the same way `record_client_error`'s is —
    /// see that method's doc comment; this route has the identical trust
    /// model.
    pub async fn set_like(
        &self,
        entry_key: &str,
        device_id: &str,
        device_name: &str,
        liked: bool,
    ) -> sqlx::Result<()> {
        if liked {
            let liked_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            sqlx::query(
                "INSERT INTO entry_likes (entry_key, device_id, device_name, liked_at_ms) VALUES (?, ?, ?, ?) \
                 ON CONFLICT (entry_key, device_id) DO UPDATE SET device_name = excluded.device_name, liked_at_ms = excluded.liked_at_ms",
            )
            .bind(entry_key)
            .bind(device_id)
            .bind(device_name)
            .bind(liked_at_ms)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query("DELETE FROM entry_likes WHERE entry_key = ? AND device_id = ?")
                .bind(entry_key)
                .bind(device_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Like count per entry, for every entry with at least one like — one
    /// grouped query for the whole manifest rather than a per-entry lookup,
    /// same reasoning as [`Self::distinct_genres`] reading everything in one
    /// pass instead of N queries.
    pub async fn like_counts(&self) -> sqlx::Result<HashMap<String, u32>> {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT entry_key, COUNT(*) FROM entry_likes GROUP BY entry_key")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(entry_key, count)| (entry_key, count as u32))
            .collect())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReclassifyReport {
    pub changed: u64,
    pub unchanged: u64,
}

/// `ALTER TABLE ADD COLUMN IF NOT EXISTS` doesn't exist in SQLite; check
/// `pragma_table_info` first so re-adding a column already present is a
/// silent no-op instead of an error.
async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    ddl_type: &str,
) -> sqlx::Result<()> {
    let exists: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?")
        .bind(table)
        .bind(column)
        .fetch_one(pool)
        .await?;
    if exists.0 == 0 {
        sqlx::query(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {ddl_type}"
        ))
        .execute(pool)
        .await?;
    }
    Ok(())
}

const ENTRY_SELECT: &str =
    "SELECT entry_key, relative_path, kind, title, size, modified_time, fingerprint, artist, album, \
     track_number, show_title, season, episode, duration_secs, video_json, audio_json, \
     scraped_title, episode_title, genres_json, artwork_version, year, cast_json, overview, rating, \
     community_rating, community_rating_votes FROM library_entries";

#[derive(sqlx::FromRow)]
struct EntryRow {
    entry_key: String,
    relative_path: String,
    kind: String,
    title: String,
    size: i64,
    modified_time: i64,
    fingerprint: String,
    artist: Option<String>,
    album: Option<String>,
    track_number: Option<i64>,
    show_title: Option<String>,
    season: Option<i64>,
    episode: Option<i64>,
    duration_secs: Option<f64>,
    video_json: Option<String>,
    audio_json: Option<String>,
    scraped_title: Option<String>,
    episode_title: Option<String>,
    genres_json: Option<String>,
    artwork_version: i64,
    year: Option<i64>,
    cast_json: Option<String>,
    overview: Option<String>,
    rating: Option<String>,
    community_rating: Option<f64>,
    community_rating_votes: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct MetadataHistoryRow {
    source_relative_path: String,
    #[allow(dead_code)]
    kind: String,
    scraped_title: Option<String>,
    episode_title: Option<String>,
    genres_json: Option<String>,
    cast_json: Option<String>,
    overview: Option<String>,
    rating: Option<String>,
    community_rating: Option<f64>,
    community_rating_votes: Option<i64>,
    poster_relative_path: Option<String>,
    season_poster_relative_path: Option<String>,
    backdrop_relative_path: Option<String>,
    cover_relative_path: Option<String>,
    artist_art_relative_path: Option<String>,
    tmdb_id: Option<i64>,
    skip_segments_json: Option<String>,
    scrape_version: i64,
}

fn remap_archived_artwork_path(
    artwork_path: &str,
    old_media_path: &str,
    new_media_path: &str,
) -> String {
    let old_parent = old_media_path.rsplit_once('/').map(|(parent, _)| parent);
    let new_parent = new_media_path.rsplit_once('/').map(|(parent, _)| parent);
    match (old_parent, new_parent) {
        (Some(old_parent), Some(new_parent)) if artwork_path == old_parent => {
            new_parent.to_string()
        }
        (Some(old_parent), Some(new_parent)) => artwork_path
            .strip_prefix(old_parent)
            .filter(|suffix| suffix.starts_with('/'))
            .map(|suffix| format!("{new_parent}{suffix}"))
            .unwrap_or_else(|| artwork_path.to_string()),
        _ => artwork_path.to_string(),
    }
}

impl From<EntryRow> for EntryRecord {
    fn from(row: EntryRow) -> Self {
        EntryRecord {
            entry_key: row.entry_key,
            relative_path: row.relative_path,
            kind: parse_kind(&row.kind),
            title: row.title,
            size: row.size as u64,
            modified_time: row.modified_time,
            fingerprint: row.fingerprint,
            artist: row.artist,
            album: row.album,
            track_number: row.track_number.map(|n| n as u32),
            show_title: row.show_title,
            season: row.season.map(|n| n as u32),
            episode: row.episode.map(|n| n as u32),
            duration_secs: row.duration_secs,
            video: row
                .video_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok()),
            audio: row
                .audio_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok()),
            // Empty string is the "scraped, no match found" marker (see
            // set_scrape_not_found) — surface it as no display overlay.
            scraped_title: row.scraped_title.filter(|t| !t.is_empty()),
            episode_title: row.episode_title.filter(|t| !t.is_empty()),
            genres: row
                .genres_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default(),
            artwork_version: row.artwork_version as u32,
            year: row.year.map(|n| n as u32),
            cast: row
                .cast_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default(),
            overview: row.overview,
            rating: row.rating,
            community_rating: row.community_rating,
            community_rating_votes: row.community_rating_votes.map(|votes| votes as u64),
        }
    }
}

impl EntryRecord {
    pub fn to_catalog_entry(&self) -> CatalogEntry {
        CatalogEntry {
            entry_key: self.entry_key.clone(),
            fingerprint: self.fingerprint.clone(),
            kind: self.kind,
            title: self.title.clone(),
            size: self.size,
            duration_secs: self.duration_secs,
            show_title: self.show_title.clone(),
            season: self.season,
            episode: self.episode,
            artist: self.artist.clone(),
            album: self.album.clone(),
            track_number: self.track_number,
            scraped_title: self.scraped_title.clone(),
            episode_title: self.episode_title.clone(),
            genres: self.genres.clone(),
            video: self.video.clone(),
            audio: self.audio.clone(),
            artwork_etag: (self.artwork_version > 0).then(|| format!("v{}", self.artwork_version)),
            year: self.year,
            cast: self
                .cast
                .iter()
                .map(|c| swarm_core::peer::CastMember {
                    name: c.name.clone(),
                    character: c.character.clone(),
                })
                .collect(),
            overview: self.overview.clone(),
            rating: self.rating.clone(),
            community_rating: self.community_rating,
            community_rating_votes: self.community_rating_votes,
            // Not derivable from a single row — `serve.rs`'s `manifest()`
            // overwrites this with a real count from `Library::like_counts`
            // after mapping every entry, the same reason `artwork_etag`
            // needs the row's own `artwork_version` but `like_count` needs a
            // separate, whole-table aggregate query instead.
            like_count: 0,
            // Loaded in `catalog_snapshot` alongside aggregate likes. Keeping
            // the JSON out of EntryRecord avoids inflating scan/probe paths
            // that never consume playback markers.
            skip_segments: Vec::new(),
        }
    }
}
