use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, params};

#[derive(Debug, Clone)]
pub struct SeenEntry {
    pub ani_id: String,
    pub title: String,
    pub last_episode: String,
    pub last_seen_at: String,
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
        Ok(Self { conn })
    }

    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS seen_progress (
                ani_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                last_episode TEXT NOT NULL,
                last_seen_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_seen_progress_seen_at ON seen_progress(last_seen_at DESC);
            "#,
        )?;
        Ok(())
    }

    pub fn upsert_seen(&self, ani_id: &str, title: &str, episode: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO seen_progress (ani_id, title, last_episode, last_seen_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(ani_id) DO UPDATE SET
                title = excluded.title,
                last_episode = excluded.last_episode,
                last_seen_at = excluded.last_seen_at
            "#,
            params![ani_id, title, episode, now],
        )?;
        Ok(())
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
            "SELECT ani_id, title, last_episode, last_seen_at FROM seen_progress ORDER BY last_seen_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(SeenEntry {
                ani_id: row.get(0)?,
                title: row.get(1)?,
                last_episode: row.get(2)?,
                last_seen_at: row.get(3)?,
            }));
        }
        Ok(None)
    }

    pub fn list_seen(&self) -> Result<Vec<SeenEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT ani_id, title, last_episode, last_seen_at FROM seen_progress ORDER BY last_seen_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SeenEntry {
                ani_id: row.get(0)?,
                title: row.get(1)?,
                last_episode: row.get(2)?,
                last_seen_at: row.get(3)?,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
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
}
