//! Server-local relational state: which STUN server/swarms this device is
//! linked to, and the periodic real upload-bandwidth measurement history
//! (see `bandwidth.rs`). SQLite via sqlx, same idempotent
//! `CREATE TABLE IF NOT EXISTS` + additive-`ensure_column` convention
//! `swarm-media::store::Library` already uses — not a separate migration
//! tool, kept consistent with the one already established in this
//! workspace. Replaces the old plain `stun-link.json` file: this data was
//! always structured (one link, many swarms) and a flat JSON blob couldn't
//! express that relation or be queried — `stun_link` is a singleton row
//! (`id` always 1, matching `swarm_media::store`'s style for "the current
//! X" rows) and `stun_link_swarm` is its one-to-many child, FK'd with
//! `ON DELETE CASCADE` so relinking to a different server can't leave
//! orphaned swarm rows behind.
//!
//! The access token and managed-swarm owner claim still live in separate
//! `TokenStore` entries (OS keyring or permission-restricted fallback
//! files), never here — this database contains no bearer secrets and is
//! safe to inspect while debugging.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;
use swarm_core::rest::SwarmSummary;

const STUN_LINK_ROW_ID: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct StunLinkRecord {
    pub base_url: String,
    pub device_id: String,
    pub swarms: Vec<SwarmSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LocalPeerRecord {
    pub fingerprint: String,
    pub name: String,
    pub paired_at: i64,
}

/// An HTTP-only paired device (no cert/mTLS — Roku-class clients, see
/// `http_media.rs`). `token_hash` mirrors `local_peer.fingerprint`'s role as
/// primary key/lookup credential; only the hash is ever stored, matching
/// this module's "no bearer secrets" invariant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HttpMediaDeviceRecord {
    pub token_hash: String,
    pub name: String,
    pub paired_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSwarmIdentity {
    pub base_url: String,
    pub swarm_id: String,
}

pub struct StateDb {
    pool: SqlitePool,
}

impl StateDb {
    pub async fn open(data_dir: &Path) -> sqlx::Result<Self> {
        let path = data_dir.join("server-state.sqlite");
        let options = SqliteConnectOptions::from_str(&format!(
            "sqlite://{}",
            path.to_str().unwrap_or_default()
        ))?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::query("PRAGMA foreign_keys = ON;")
            .execute(&pool)
            .await?;
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS stun_link (
                id INTEGER PRIMARY KEY,
                base_url TEXT NOT NULL,
                device_id TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stun_link_swarm (
                id TEXT PRIMARY KEY,
                stun_link_id INTEGER NOT NULL REFERENCES stun_link(id) ON DELETE CASCADE,
                name TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_stun_link_swarm_link ON stun_link_swarm(stun_link_id);
            CREATE TABLE IF NOT EXISTS bandwidth_measurement (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                measured_at INTEGER NOT NULL,
                upload_bps INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS local_peer (
                fingerprint TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                paired_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_local_peer_paired_at ON local_peer(paired_at DESC);
            CREATE TABLE IF NOT EXISTS http_media_device (
                token_hash TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                paired_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_http_media_device_paired_at ON http_media_device(paired_at DESC);
            CREATE TABLE IF NOT EXISTS swarm_dependent_device (
                fingerprint TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS managed_swarm_identity (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                base_url TEXT NOT NULL,
                swarm_id TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    pub async fn load_stun_link(&self) -> sqlx::Result<Option<StunLinkRecord>> {
        let Some((base_url, device_id)): Option<(String, String)> =
            sqlx::query_as("SELECT base_url, device_id FROM stun_link WHERE id = ?")
                .bind(STUN_LINK_ROW_ID)
                .fetch_optional(&self.pool)
                .await?
        else {
            return Ok(None);
        };
        let swarms: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, name FROM stun_link_swarm WHERE stun_link_id = ? ORDER BY name",
        )
        .bind(STUN_LINK_ROW_ID)
        .fetch_all(&self.pool)
        .await?;
        Ok(Some(StunLinkRecord {
            base_url,
            device_id,
            swarms: swarms
                .into_iter()
                .map(|(id, name)| SwarmSummary { id, name })
                .collect(),
        }))
    }

    pub async fn save_stun_link(&self, record: &StunLinkRecord) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO stun_link (id, base_url, device_id, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET base_url = excluded.base_url, device_id = excluded.device_id, updated_at = excluded.updated_at",
        )
        .bind(STUN_LINK_ROW_ID)
        .bind(&record.base_url)
        .bind(&record.device_id)
        .bind(now())
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM stun_link_swarm WHERE stun_link_id = ?")
            .bind(STUN_LINK_ROW_ID)
            .execute(&mut *tx)
            .await?;
        for swarm in &record.swarms {
            sqlx::query("INSERT INTO stun_link_swarm (id, stun_link_id, name) VALUES (?, ?, ?)")
                .bind(&swarm.id)
                .bind(STUN_LINK_ROW_ID)
                .bind(&swarm.name)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }

    pub async fn record_bandwidth_measurement(&self, upload_bps: u64) -> sqlx::Result<()> {
        sqlx::query("INSERT INTO bandwidth_measurement (measured_at, upload_bps) VALUES (?, ?)")
            .bind(now())
            .bind(upload_bps as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The most recent successful measurement, if any — the auto-measured
    /// baseline `bandwidth.rs`'s periodic loop restores on startup so a
    /// restart doesn't fall back to the static default for a full interval.
    pub async fn latest_bandwidth_measurement(&self) -> sqlx::Result<Option<u64>> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT upload_bps FROM bandwidth_measurement ORDER BY id DESC LIMIT 1")
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(bps,)| bps as u64))
    }

    pub async fn save_local_peer(&self, fingerprint: &str, name: &str) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO local_peer (fingerprint, name, paired_at) VALUES (?, ?, ?) \
             ON CONFLICT(fingerprint) DO UPDATE SET name = excluded.name, paired_at = excluded.paired_at",
        )
        .bind(fingerprint)
        .bind(name)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn local_peers(&self) -> sqlx::Result<Vec<LocalPeerRecord>> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT fingerprint, name, paired_at FROM local_peer ORDER BY paired_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(fingerprint, name, paired_at)| LocalPeerRecord {
                fingerprint,
                name,
                paired_at,
            })
            .collect())
    }

    pub async fn remove_local_peer(&self, fingerprint: &str) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM local_peer WHERE fingerprint = ?")
            .bind(fingerprint)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn save_http_media_device(&self, token_hash: &str, name: &str) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO http_media_device (token_hash, name, paired_at) VALUES (?, ?, ?) \
             ON CONFLICT(token_hash) DO UPDATE SET name = excluded.name, paired_at = excluded.paired_at",
        )
        .bind(token_hash)
        .bind(name)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Bearer-auth check for the HTTP media surface (`http_media.rs`):
    /// exists at all == authorized, matching `local_peer`'s hard-delete
    /// revocation model rather than the STUN server's separate
    /// soft-`revoked_at` one — this table has no other service depending on
    /// distinguishing "revoked" from "never existed."
    pub async fn http_media_device_name(&self, token_hash: &str) -> sqlx::Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT name FROM http_media_device WHERE token_hash = ?")
                .bind(token_hash)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(name,)| name))
    }

    pub async fn http_media_devices(&self) -> sqlx::Result<Vec<HttpMediaDeviceRecord>> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT token_hash, name, paired_at FROM http_media_device ORDER BY paired_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(token_hash, name, paired_at)| HttpMediaDeviceRecord {
                token_hash,
                name,
                paired_at,
            })
            .collect())
    }

    pub async fn remove_http_media_device(&self, token_hash: &str) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM http_media_device WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Devices that reach this server through SWARM, as of the last roster
    /// that could be fetched. Kept on disk because the answer is needed
    /// precisely when the roster *cannot* be fetched: it decides whether an
    /// outage affects anyone. Replaced wholesale so a device removed from the
    /// swarm stops counting.
    pub async fn replace_swarm_dependents(
        &self,
        devices: &[(String, String)],
    ) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM swarm_dependent_device")
            .execute(&mut *tx)
            .await?;
        for (fingerprint, name) in devices {
            sqlx::query(
                "INSERT OR REPLACE INTO swarm_dependent_device (fingerprint, name, updated_at) VALUES (?, ?, ?)",
            )
            .bind(fingerprint)
            .bind(name)
            .bind(now())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    /// `(fingerprint, name)` pairs, ordered by name.
    pub async fn swarm_dependents(&self) -> sqlx::Result<Vec<(String, String)>> {
        sqlx::query_as("SELECT fingerprint, name FROM swarm_dependent_device ORDER BY name, fingerprint")
            .fetch_all(&self.pool)
            .await
    }

    /// Forgets the saved SWARM service link and its swarm memberships.
    pub async fn clear_stun_link(&self) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM stun_link_swarm WHERE stun_link_id = ?")
            .bind(STUN_LINK_ROW_ID)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM stun_link WHERE id = ?")
            .bind(STUN_LINK_ROW_ID)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }

    /// Forgets the managed-swarm identity. The owner claim that pairs with it
    /// lives in the credential store and must be deleted alongside it.
    pub async fn clear_managed_swarm_identity(&self) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM managed_swarm_identity WHERE id = 1")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn load_managed_swarm_identity(&self) -> sqlx::Result<Option<ManagedSwarmIdentity>> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT base_url, swarm_id FROM managed_swarm_identity WHERE id = 1")
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(base_url, swarm_id)| ManagedSwarmIdentity { base_url, swarm_id }))
    }

    pub async fn save_managed_swarm_identity(
        &self,
        identity: &ManagedSwarmIdentity,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO managed_swarm_identity (id, base_url, swarm_id, created_at, updated_at) VALUES (1, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET base_url = excluded.base_url, updated_at = excluded.updated_at",
        )
        .bind(identity.base_url.trim_end_matches('/'))
        .bind(&identity.swarm_id)
        .bind(now())
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stun_link_round_trips_with_its_swarms() {
        let dir = std::env::temp_dir().join(format!("swarm-state-db-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir).await.unwrap();
        assert!(db.load_stun_link().await.unwrap().is_none());

        let record = StunLinkRecord {
            base_url: "https://swarm.example.com".into(),
            device_id: "dev-1".into(),
            swarms: vec![
                SwarmSummary {
                    id: "sw-1".into(),
                    name: "Home".into(),
                },
                SwarmSummary {
                    id: "sw-2".into(),
                    name: "Cabin".into(),
                },
            ],
        };
        db.save_stun_link(&record).await.unwrap();
        let loaded = db.load_stun_link().await.unwrap().unwrap();
        assert_eq!(loaded.device_id, "dev-1");
        assert_eq!(loaded.swarms.len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A relink (leaving one swarm, or replacing the whole link with a
    /// fresh one) must not leave orphaned `stun_link_swarm` rows behind —
    /// confirms the ON DELETE CASCADE / explicit delete-then-reinsert
    /// actually prevents drift between the two tables.
    #[tokio::test]
    async fn saving_a_smaller_swarm_list_drops_the_removed_rows() {
        let dir =
            std::env::temp_dir().join(format!("swarm-state-db-shrink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir).await.unwrap();

        let mut record = StunLinkRecord {
            base_url: "https://swarm.example.com".into(),
            device_id: "dev-1".into(),
            swarms: vec![
                SwarmSummary {
                    id: "sw-1".into(),
                    name: "Home".into(),
                },
                SwarmSummary {
                    id: "sw-2".into(),
                    name: "Cabin".into(),
                },
            ],
        };
        db.save_stun_link(&record).await.unwrap();
        record.swarms.retain(|s| s.id != "sw-2");
        db.save_stun_link(&record).await.unwrap();

        let loaded = db.load_stun_link().await.unwrap().unwrap();
        assert_eq!(loaded.swarms.len(), 1);
        assert_eq!(loaded.swarms[0].id, "sw-1");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn latest_bandwidth_measurement_is_the_most_recent_row() {
        let dir = std::env::temp_dir().join(format!("swarm-state-db-bw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir).await.unwrap();
        assert_eq!(db.latest_bandwidth_measurement().await.unwrap(), None);

        db.record_bandwidth_measurement(5_000_000).await.unwrap();
        db.record_bandwidth_measurement(7_500_000).await.unwrap();
        assert_eq!(
            db.latest_bandwidth_measurement().await.unwrap(),
            Some(7_500_000)
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn local_peers_persist_and_can_be_revoked() {
        let dir =
            std::env::temp_dir().join(format!("swarm-state-db-local-peer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir).await.unwrap();
        let fingerprint = "ab".repeat(32);

        db.save_local_peer(&fingerprint, "Living Room TV")
            .await
            .unwrap();
        let peers = db.local_peers().await.unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].fingerprint, fingerprint);
        assert_eq!(peers[0].name, "Living Room TV");

        db.remove_local_peer(&fingerprint).await.unwrap();
        assert!(db.local_peers().await.unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn http_media_devices_persist_authorize_and_can_be_revoked() {
        let dir = std::env::temp_dir()
            .join(format!("swarm-state-db-http-device-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir).await.unwrap();
        let token_hash = "cd".repeat(32);

        assert_eq!(db.http_media_device_name(&token_hash).await.unwrap(), None);

        db.save_http_media_device(&token_hash, "Living Room Roku")
            .await
            .unwrap();
        assert_eq!(
            db.http_media_device_name(&token_hash).await.unwrap(),
            Some("Living Room Roku".to_string())
        );
        let devices = db.http_media_devices().await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].token_hash, token_hash);
        assert_eq!(devices[0].name, "Living Room Roku");

        db.remove_http_media_device(&token_hash).await.unwrap();
        assert_eq!(db.http_media_device_name(&token_hash).await.unwrap(), None);
        assert!(db.http_media_devices().await.unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn managed_swarm_identity_is_stable_while_its_service_url_can_refresh() {
        let dir =
            std::env::temp_dir().join(format!("swarm-state-db-managed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir).await.unwrap();
        let mut identity = ManagedSwarmIdentity {
            base_url: "https://swarm.example.com/".into(),
            swarm_id: "ab".repeat(32),
        };
        db.save_managed_swarm_identity(&identity).await.unwrap();
        identity.base_url = "https://swarm.example.com".into();
        db.save_managed_swarm_identity(&identity).await.unwrap();
        assert_eq!(
            db.load_managed_swarm_identity().await.unwrap(),
            Some(identity)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn swarm_dependents_are_replaced_wholesale_and_ordered() {
        let dir = std::env::temp_dir().join(format!("swarm-state-db-deps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir).await.unwrap();
        assert!(db.swarm_dependents().await.unwrap().is_empty());

        db.replace_swarm_dependents(&[
            ("bb".repeat(32), "Den TV".into()),
            ("aa".repeat(32), "Michael's TV".into()),
        ])
        .await
        .unwrap();
        let names: Vec<String> = db
            .swarm_dependents()
            .await
            .unwrap()
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, ["Den TV", "Michael's TV"]);

        // A device that left the swarm must stop counting.
        db.replace_swarm_dependents(&[("aa".repeat(32), "Michael's TV".into())])
            .await
            .unwrap();
        assert_eq!(db.swarm_dependents().await.unwrap().len(), 1);

        db.replace_swarm_dependents(&[]).await.unwrap();
        assert!(db.swarm_dependents().await.unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// "Forget this SWARM service" must leave nothing behind that could make
    /// the next startup dial the old address again — not the link, not its
    /// swarm rows, and not the managed identity that also carries a URL.
    #[tokio::test]
    async fn clearing_the_link_and_identity_removes_every_saved_service_address() {
        let dir = std::env::temp_dir().join(format!("swarm-state-db-clear-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = StateDb::open(&dir).await.unwrap();
        db.save_stun_link(&StunLinkRecord {
            base_url: "http://192.168.0.235:8080".into(),
            device_id: "device".into(),
            swarms: vec![SwarmSummary {
                id: "s1".into(),
                name: "Home".into(),
            }],
        })
        .await
        .unwrap();
        db.save_managed_swarm_identity(&ManagedSwarmIdentity {
            base_url: "http://192.168.0.235:8080".into(),
            swarm_id: "cd".repeat(32),
        })
        .await
        .unwrap();

        db.clear_stun_link().await.unwrap();
        db.clear_managed_swarm_identity().await.unwrap();

        assert_eq!(db.load_stun_link().await.unwrap(), None);
        assert_eq!(db.load_managed_swarm_identity().await.unwrap(), None);
        // Clearing an already-empty store is a no-op, not an error.
        db.clear_stun_link().await.unwrap();
        db.clear_managed_swarm_identity().await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
