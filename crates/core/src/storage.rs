//! SQLite-backed clipboard history.
//!
//! Deduplication is content-hash based: re-copying an existing entry bumps it
//! to the top instead of duplicating. Pinned entries survive all pruning.

use crate::constants as c;
use anyhow::{Context, Result};
use cliphistory_proto::{Content, HistoryItem};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    hash        TEXT    NOT NULL UNIQUE,
    kind        TEXT    NOT NULL CHECK (kind IN ('text','image')),
    mime        TEXT    NOT NULL DEFAULT 'text/plain;charset=utf-8',
    size_bytes  INTEGER NOT NULL,
    data        BLOB    NOT NULL,
    preview     TEXT    NOT NULL,
    width       INTEGER,
    height      INTEGER,
    pinned      INTEGER NOT NULL DEFAULT 0,
    use_count   INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_entries_created ON entries (created_at DESC);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// New entry stored with this id.
    Inserted(i64),
    /// Already existed; promoted to top, id unchanged.
    Duplicate(i64),
    /// Larger than `max_item_size`; ignored.
    TooLarge { size: i64, limit: i64 },
}

pub struct Storage {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA).context("initialising schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
        })
    }
    // (test helper; kept out of the shipped API surface on purpose)

    pub fn db_path(&self) -> &Path {
        &self.path
    }

    pub fn db_size_bytes(&self) -> u64 {
        let mut total = 0;
        for suffix in ["", "-wal", "-shm"] {
            if let Ok(md) = std::fs::metadata(format!("{}{suffix}", self.path.display())) {
                total += md.len();
            }
        }
        total
    }

    // -- writes -------------------------------------------------------------

    pub fn insert(&self, content: &Content, max_item_size: i64) -> Result<InsertOutcome> {
        let bytes = content.bytes();
        if bytes.len() as i64 > max_item_size {
            return Ok(InsertOutcome::TooLarge {
                size: bytes.len() as i64,
                limit: max_item_size,
            });
        }

        let hash = content_hash(&bytes);
        let preview = content.preview();
        let now = unix_now() as i64;
        let conn = self.conn.lock().expect("storage lock poisoned");

        let updated = conn.execute(
            "UPDATE entries SET created_at = ?2, last_used_at = ?2 WHERE hash = ?1",
            rusqlite::params![hash, now],
        )?;
        if updated > 0 {
            let id: i64 = conn.query_row(
                "SELECT id FROM entries WHERE hash = ?1",
                rusqlite::params![hash],
                |r| r.get(0),
            )?;
            return Ok(InsertOutcome::Duplicate(id));
        }

        let (width, height) = match content {
            Content::Image { width, height, .. } => (*width, *height),
            Content::Text { .. } => (None, None),
        };
        conn.execute(
            "INSERT INTO entries
                 (hash, kind, mime, size_bytes, data, preview, width, height,
                  pinned, use_count, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, ?9, ?9)",
            rusqlite::params![
                hash,
                content.kind(),
                content.mime(),
                bytes.len() as i64,
                bytes.as_ref(),
                preview,
                width.map(|w| w as i64),
                height.map(|h| h as i64),
                now,
            ],
        )
        .context("inserting entry")?;
        Ok(InsertOutcome::Inserted(conn.last_insert_rowid()))
    }

    /// Enforce retention limits. Pinned entries are never pruned.
    pub fn prune(&self, max_entries: i64, max_age_days: i64) -> Result<usize> {
        let mut removed = 0;
        let conn = self.conn.lock().expect("storage lock poisoned");
        if max_age_days > 0 {
            let cutoff = unix_now() as i64 - max_age_days * c::SECS_PER_DAY as i64;
            removed += conn.execute(
                "DELETE FROM entries WHERE pinned = 0 AND created_at < ?1",
                rusqlite::params![cutoff],
            )?;
        }
        if max_entries > 0 {
            removed += conn.execute(
                "DELETE FROM entries WHERE pinned = 0 AND id NOT IN (
                     SELECT id FROM entries ORDER BY pinned DESC, created_at DESC LIMIT ?1
                 )",
                rusqlite::params![max_entries],
            )?;
        }
        Ok(removed)
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        let n = self
            .conn
            .lock()
            .expect("storage lock poisoned")
            .execute("DELETE FROM entries WHERE id = ?1", rusqlite::params![id])?;
        Ok(n > 0)
    }

    /// Clear history; pinned entries are kept.
    pub fn clear(&self) -> Result<usize> {
        let n = self
            .conn
            .lock()
            .expect("storage lock poisoned")
            .execute("DELETE FROM entries WHERE pinned = 0", [])?;
        Ok(n)
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<bool> {
        let n = self.conn.lock().expect("storage lock poisoned").execute(
            "UPDATE entries SET pinned = ?2 WHERE id = ?1",
            rusqlite::params![id, pinned as i64],
        )?;
        Ok(n > 0)
    }

    pub fn mark_used(&self, id: i64) -> Result<()> {
        self.conn.lock().expect("storage lock poisoned").execute(
            "UPDATE entries SET use_count = use_count + 1, last_used_at = ?2 WHERE id = ?1",
            rusqlite::params![id, unix_now() as i64],
        )?;
        Ok(())
    }

    // -- reads ---------------------------------------------------------------

    pub fn count(&self) -> Result<i64> {
        let n = self.conn.lock().expect("storage lock poisoned").query_row(
            "SELECT COUNT(*) FROM entries",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    pub fn history_items(
        &self,
        limit: Option<usize>,
        query: Option<&str>,
    ) -> Result<Vec<HistoryItem>> {
        let limit_sql: i64 = limit.map_or(-1, |n| n.min(i64::MAX as usize) as i64);
        let sql = format!(
            "SELECT id, kind, mime, size_bytes, preview, created_at, use_count, pinned
             FROM entries
             WHERE (?1 IS NULL OR preview LIKE '%' || ?1 || '%')
             ORDER BY pinned DESC, created_at DESC
             LIMIT {limit_sql}"
        );
        let conn = self.conn.lock().expect("storage lock poisoned");
        let mut stmt = conn.prepare(&sql)?;
        let like = query.map(|q| format!("%{q}%"));
        let rows = stmt.query_map(rusqlite::params![like], |r| {
            Ok(HistoryItem {
                id: r.get(0)?,
                kind: r.get(1)?,
                mime: r.get(2)?,
                size_bytes: r.get::<_, i64>(3)? as u64,
                preview: r.get(4)?,
                created_at: r.get::<_, i64>(5)? as u64,
                use_count: r.get::<_, i64>(6)? as u64,
                pinned: r.get::<_, i64>(7)? != 0,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Full payload of an entry, reconstructed as [`Content`].
    pub fn content(&self, id: i64) -> Result<Option<Content>> {
        let conn = self.conn.lock().expect("storage lock poisoned");
        let mut stmt =
            conn.prepare("SELECT kind, mime, data, width, height FROM entries WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let kind: String = row.get(0)?;
        let mime: String = row.get(1)?;
        let mut blob: Vec<u8> = Vec::new();
        row.get_ref(2)?.as_blob()?.read_to_end(&mut blob)?;
        match kind.as_str() {
            "text" => Ok(Some(Content::Text {
                text: String::from_utf8_lossy(&blob).into_owned(),
            })),
            _ => Ok(Some(Content::Image {
                mime,
                data: blob,
                width: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                height: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
            })),
        }
    }

    /// Newest entry payload.
    #[allow(dead_code)]
    pub fn newest_content(&self) -> Result<Option<Content>> {
        let items = self.history_items(Some(1), None)?;
        match items.first() {
            Some(it) => self.content(it.id),
            None => Ok(None),
        }
    }
}

pub fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Content {
        Content::Text { text: s.into() }
    }

    #[test]
    fn insert_dedup_promotes() {
        let s = Storage::open_in_memory().unwrap();
        assert!(matches!(
            s.insert(&text("a"), 100).unwrap(),
            InsertOutcome::Inserted(_)
        ));
        assert!(matches!(
            s.insert(&text("b"), 100).unwrap(),
            InsertOutcome::Inserted(_)
        ));
        assert!(matches!(
            s.insert(&text("a"), 100).unwrap(),
            InsertOutcome::Duplicate(_)
        ));
        assert_eq!(s.count().unwrap(), 2);
        assert_eq!(s.history_items(None, None).unwrap()[0].preview, "a");
    }

    #[test]
    fn too_large_rejected() {
        let s = Storage::open_in_memory().unwrap();
        assert_eq!(
            s.insert(&text("big"), 2).unwrap(),
            InsertOutcome::TooLarge { size: 3, limit: 2 }
        );
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn images_roundtrip() {
        let s = Storage::open_in_memory().unwrap();
        let img = Content::Image {
            mime: "image/png".into(),
            data: vec![1, 2, 3, 4],
            width: Some(2),
            height: Some(2),
        };
        let id = match s.insert(&img, 1024).unwrap() {
            InsertOutcome::Inserted(id) => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(s.content(id).unwrap().unwrap(), img);
        assert_eq!(s.history_items(None, None).unwrap()[0].kind, "image");
    }

    #[test]
    fn search_and_limits() {
        let s = Storage::open_in_memory().unwrap();
        for t in ["alpha one", "beta two", "alpha three"] {
            s.insert(&text(t), 1000).unwrap();
        }
        assert_eq!(s.history_items(Some(2), None).unwrap().len(), 2);
        let hits = s.history_items(None, Some("alpha")).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn pruning_respects_pins() {
        let s = Storage::open_in_memory().unwrap();
        for i in 0..10 {
            s.insert(&text(&format!("e{i}")), 1000).unwrap();
        }
        s.set_pinned(1, true).unwrap();
        assert_eq!(s.prune(3, 0).unwrap(), 7);
        let left = s.history_items(None, None).unwrap();
        assert_eq!(left.len(), 3);
        assert!(left.iter().any(|i| i.id == 1 && i.pinned));
    }

    #[test]
    fn clear_keeps_pinned() {
        let s = Storage::open_in_memory().unwrap();
        s.insert(&text("keep"), 1000).unwrap();
        s.insert(&text("drop"), 1000).unwrap();
        s.set_pinned(1, true).unwrap();
        assert_eq!(s.clear().unwrap(), 1);
        assert_eq!(s.count().unwrap(), 1);
    }

    #[test]
    fn delete_reports_existence() {
        let s = Storage::open_in_memory().unwrap();
        let id = match s.insert(&text("x"), 100).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };
        assert!(s.delete(id).unwrap());
        assert!(!s.delete(id).unwrap());
    }

    #[test]
    fn mark_used_bumps_counter() {
        let s = Storage::open_in_memory().unwrap();
        let id = match s.insert(&text("x"), 100).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };
        s.mark_used(id).unwrap();
        assert_eq!(s.history_items(None, None).unwrap()[0].use_count, 1);
    }
}
