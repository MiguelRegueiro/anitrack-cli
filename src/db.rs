use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use rusqlite::{Connection, params};

const SCHEMA_VERSION: i64 = 3;

#[derive(Debug, Clone)]
pub struct SeenEntry {
    pub ani_id: String,
    pub title: String,
    pub last_episode: String,
    pub last_seen_at: String,
    pub total_episodes: Option<u32>,
    pub episodes_updated_at: Option<String>,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create database directory {}", parent.display())
            })?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("failed to configure sqlite busy timeout")?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        Ok(Self { conn })
    }

    pub fn migrate(&self) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .context("failed to start migration transaction")?;
        let mut user_version: i64 = tx
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .context("failed reading sqlite user_version")?;

        if user_version > SCHEMA_VERSION {
            return Err(anyhow!(
                "database schema version {user_version} is newer than supported {SCHEMA_VERSION}"
            ));
        }

        while user_version < SCHEMA_VERSION {
            let next_version = user_version + 1;
            match next_version {
                1 => {
                    tx.execute_batch(
                        r#"
                        CREATE TABLE IF NOT EXISTS seen_progress (
                            ani_id TEXT PRIMARY KEY,
                            title TEXT NOT NULL,
                            last_episode TEXT NOT NULL,
                            last_seen_at TEXT NOT NULL
                        );
                        "#,
                    )
                    .context("failed applying migration v1")?;
                }
                2 => {
                    tx.execute_batch(
                        r#"
                        CREATE INDEX IF NOT EXISTS idx_seen_progress_seen_at
                        ON seen_progress(last_seen_at DESC);
                        "#,
                    )
                    .context("failed applying migration v2")?;
                }
                3 => {
                    tx.execute_batch(
                        r#"
                        ALTER TABLE seen_progress
                        ADD COLUMN total_episodes INTEGER;

                        ALTER TABLE seen_progress
                        ADD COLUMN episodes_updated_at TEXT;
                        "#,
                    )
                    .context("failed applying migration v3")?;
                }
                _ => {
                    return Err(anyhow!(
                        "missing migration for schema version {next_version}"
                    ));
                }
            }

            tx.pragma_update(None, "user_version", next_version)
                .with_context(|| format!("failed setting sqlite user_version to {next_version}"))?;
            user_version = next_version;
        }

        tx.commit().context("failed to commit migrations")?;
        Ok(())
    }

    pub fn upsert_seen(&self, ani_id: &str, title: &str, episode: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let derived_total = parse_title_total_episodes(title)
            .into_iter()
            .chain(parse_episode_u32(episode))
            .max();
        self.conn.execute(
            r#"
            INSERT INTO seen_progress (
                ani_id,
                title,
                last_episode,
                last_seen_at,
                total_episodes,
                episodes_updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, CASE WHEN ?5 IS NULL THEN NULL ELSE ?4 END)
            ON CONFLICT(ani_id) DO UPDATE SET
                title = excluded.title,
                last_episode = excluded.last_episode,
                last_seen_at = excluded.last_seen_at,
                total_episodes = CASE
                    WHEN excluded.total_episodes IS NULL THEN seen_progress.total_episodes
                    WHEN seen_progress.total_episodes IS NULL THEN excluded.total_episodes
                    ELSE MAX(seen_progress.total_episodes, excluded.total_episodes)
                END,
                episodes_updated_at = CASE
                    WHEN excluded.total_episodes IS NULL THEN seen_progress.episodes_updated_at
                    WHEN seen_progress.total_episodes IS NOT NULL
                        AND seen_progress.total_episodes >= excluded.total_episodes
                        THEN seen_progress.episodes_updated_at
                    ELSE excluded.episodes_updated_at
                END
            "#,
            params![ani_id, title, episode, now, derived_total],
        )?;
        Ok(())
    }

    pub fn update_episode_metadata(&self, ani_id: &str, total_episodes: u32) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            r#"
            UPDATE seen_progress
            SET total_episodes = CASE
                    WHEN total_episodes IS NULL THEN ?2
                    ELSE MAX(total_episodes, ?2)
                END,
                episodes_updated_at = CASE
                    WHEN total_episodes IS NOT NULL AND total_episodes >= ?2 THEN episodes_updated_at
                    ELSE ?3
                END
            WHERE ani_id = ?1
            "#,
            params![ani_id, total_episodes, now],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_seen(&self, ani_id: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM seen_progress WHERE ani_id = ?1",
            params![ani_id],
        )?;
        Ok(changed > 0)
    }

    pub fn last_seen(&self) -> Result<Option<SeenEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT ani_id, title, last_episode, last_seen_at, total_episodes, episodes_updated_at FROM seen_progress ORDER BY last_seen_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(SeenEntry {
                ani_id: row.get(0)?,
                title: row.get(1)?,
                last_episode: row.get(2)?,
                last_seen_at: row.get(3)?,
                total_episodes: row.get(4)?,
                episodes_updated_at: row.get(5)?,
            }));
        }
        Ok(None)
    }

    pub fn list_seen(&self) -> Result<Vec<SeenEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT ani_id, title, last_episode, last_seen_at, total_episodes, episodes_updated_at FROM seen_progress ORDER BY last_seen_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SeenEntry {
                ani_id: row.get(0)?,
                title: row.get(1)?,
                last_episode: row.get(2)?,
                last_seen_at: row.get(3)?,
                total_episodes: row.get(4)?,
                episodes_updated_at: row.get(5)?,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

fn parse_title_total_episodes(title: &str) -> Option<u32> {
    let trimmed = title.trim();
    let open_idx = trimmed.rfind('(')?;
    if !trimmed.ends_with(')') {
        return None;
    }
    let inner = trimmed[open_idx + 1..trimmed.len() - 1].trim();
    let num_str = inner.strip_suffix(" episodes")?;
    num_str.trim().parse::<u32>().ok()
}

fn parse_episode_u32(episode: &str) -> Option<u32> {
    episode.trim().parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    fn in_memory_db() -> Database {
        Database {
            conn: Connection::open_in_memory().expect("failed to open in-memory db"),
        }
    }

    #[test]
    fn upsert_updates_existing_row() {
        let db = in_memory_db();
        db.migrate().expect("migration should succeed");

        db.upsert_seen("show-1", "Show One", "1")
            .expect("insert should succeed");
        thread::sleep(Duration::from_millis(2));
        db.upsert_seen("show-1", "Show One Renamed", "2")
            .expect("update should succeed");

        let latest = db
            .last_seen()
            .expect("query should succeed")
            .expect("row should exist");
        assert_eq!(latest.ani_id, "show-1");
        assert_eq!(latest.title, "Show One Renamed");
        assert_eq!(latest.last_episode, "2");
    }

    #[test]
    fn upsert_extracts_title_total_and_raises_it_to_watched_episode() {
        let db = in_memory_db();
        db.migrate().expect("migration should succeed");

        db.upsert_seen("show-1", "Show One (11 episodes)", "11")
            .expect("insert should succeed");
        db.upsert_seen("show-1", "Show One", "12")
            .expect("update should succeed");

        let latest = db
            .last_seen()
            .expect("query should succeed")
            .expect("row should exist");
        assert_eq!(latest.title, "Show One");
        assert_eq!(latest.last_episode, "12");
        assert_eq!(latest.total_episodes, Some(12));
        assert!(latest.episodes_updated_at.is_some());
    }

    #[test]
    fn update_episode_metadata_updates_total_without_touching_progress_order() {
        let db = in_memory_db();
        db.migrate().expect("migration should succeed");

        db.upsert_seen("show-1", "Show One (11 episodes)", "11")
            .expect("insert should succeed");
        let before = db
            .last_seen()
            .expect("query should succeed")
            .expect("row should exist");
        thread::sleep(Duration::from_millis(2));

        assert!(
            db.update_episode_metadata("show-1", 12)
                .expect("metadata update should succeed")
        );

        let after = db
            .last_seen()
            .expect("query should succeed")
            .expect("row should exist");
        assert_eq!(after.last_episode, "11");
        assert_eq!(after.last_seen_at, before.last_seen_at);
        assert_eq!(after.total_episodes, Some(12));
        assert!(after.episodes_updated_at > before.episodes_updated_at);
    }

    #[test]
    fn episode_total_does_not_move_backward() {
        let db = in_memory_db();
        db.migrate().expect("migration should succeed");

        db.upsert_seen("show-1", "Show One (12 episodes)", "12")
            .expect("insert should succeed");
        let before = db
            .last_seen()
            .expect("query should succeed")
            .expect("row should exist");
        thread::sleep(Duration::from_millis(2));

        db.upsert_seen("show-1", "Show One (11 episodes)", "11")
            .expect("lower title total should not reduce metadata");
        assert!(
            db.update_episode_metadata("show-1", 10)
                .expect("lower metadata update should still find row")
        );

        let after = db
            .last_seen()
            .expect("query should succeed")
            .expect("row should exist");
        assert_eq!(after.total_episodes, Some(12));
        assert_eq!(after.episodes_updated_at, before.episodes_updated_at);
    }

    #[test]
    fn list_seen_returns_most_recent_first() {
        let db = in_memory_db();
        db.migrate().expect("migration should succeed");

        db.upsert_seen("show-1", "Show One", "1")
            .expect("insert should succeed");
        thread::sleep(Duration::from_millis(2));
        db.upsert_seen("show-2", "Show Two", "3")
            .expect("insert should succeed");

        let rows = db.list_seen().expect("list should succeed");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ani_id, "show-2");
        assert_eq!(rows[1].ani_id, "show-1");
    }

    #[test]
    fn migrate_sets_user_version_and_is_idempotent() {
        let db = in_memory_db();

        db.migrate().expect("first migration should succeed");
        db.migrate().expect("second migration should be idempotent");

        let user_version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version should be queryable");
        assert_eq!(user_version, SCHEMA_VERSION);
    }

    #[test]
    fn migrate_upgrades_legacy_schema_without_user_version() {
        let db = in_memory_db();
        db.conn
            .execute_batch(
                r#"
                CREATE TABLE seen_progress (
                    ani_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    last_episode TEXT NOT NULL,
                    last_seen_at TEXT NOT NULL
                );
                INSERT INTO seen_progress (ani_id, title, last_episode, last_seen_at)
                VALUES ('show-1', 'Show One', '1', '2026-03-01T00:00:00+00:00');
                "#,
            )
            .expect("legacy schema should be created");

        let before_version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("legacy user_version should be queryable");
        assert_eq!(before_version, 0);

        db.migrate().expect("legacy schema should migrate");

        let after_version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("upgraded user_version should be queryable");
        assert_eq!(after_version, SCHEMA_VERSION);

        let index_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(1) FROM sqlite_master WHERE type='index' AND name='idx_seen_progress_seen_at'",
                [],
                |row| row.get(0),
            )
            .expect("index lookup should succeed");
        assert_eq!(index_count, 1);

        let existing_row: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(1) FROM seen_progress WHERE ani_id='show-1'",
                [],
                |row| row.get(0),
            )
            .expect("legacy row should survive migration");
        assert_eq!(existing_row, 1);

        let metadata_columns: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(1) FROM pragma_table_info('seen_progress') WHERE name IN ('total_episodes', 'episodes_updated_at')",
                [],
                |row| row.get(0),
            )
            .expect("metadata columns should be queryable");
        assert_eq!(metadata_columns, 2);
    }

    #[test]
    fn migrate_upgrades_from_v1_to_latest() {
        let db = in_memory_db();
        db.conn
            .execute_batch(
                r#"
                CREATE TABLE seen_progress (
                    ani_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    last_episode TEXT NOT NULL,
                    last_seen_at TEXT NOT NULL
                );
                INSERT INTO seen_progress (ani_id, title, last_episode, last_seen_at)
                VALUES ('show-2', 'Show Two', '3', '2026-03-01T00:00:00+00:00');
                "#,
            )
            .expect("v1 schema should be created");
        db.conn
            .pragma_update(None, "user_version", 1)
            .expect("v1 user_version should be set");

        db.migrate().expect("v1 schema should migrate to latest");

        let after_version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("upgraded user_version should be queryable");
        assert_eq!(after_version, SCHEMA_VERSION);

        let index_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(1) FROM sqlite_master WHERE type='index' AND name='idx_seen_progress_seen_at'",
                [],
                |row| row.get(0),
            )
            .expect("index lookup should succeed");
        assert_eq!(index_count, 1);

        let existing_row: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(1) FROM seen_progress WHERE ani_id='show-2'",
                [],
                |row| row.get(0),
            )
            .expect("v1 row should survive migration");
        assert_eq!(existing_row, 1);

        let metadata_columns: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(1) FROM pragma_table_info('seen_progress') WHERE name IN ('total_episodes', 'episodes_updated_at')",
                [],
                |row| row.get(0),
            )
            .expect("metadata columns should be queryable");
        assert_eq!(metadata_columns, 2);
    }

    #[test]
    fn migrate_rejects_future_schema_versions() {
        let db = in_memory_db();
        db.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("should set future user_version");

        let err = db
            .migrate()
            .expect_err("future schema version should be rejected");
        assert!(
            err.to_string().contains("newer than supported"),
            "unexpected error: {err}"
        );
    }
}
