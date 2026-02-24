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
