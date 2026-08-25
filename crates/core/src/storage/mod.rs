//! SQLite-backed clipboard history.
//!
//! Deduplication is content-hash based: re-copying an existing entry bumps it
//! to the top instead of duplicating. Pinned entries survive all pruning.
//!
//! Split by concern: [`prune`] owns mutations, [`thumbs`] the preview cache,
//! [`cache`] the hot payload LRU, [`model`] the insert types; this file
//! holds the connection, insert and read queries.

mod cache;
mod model;
mod prune;
mod queries;
mod thumbs;

pub use model::{ContentHead, InsertOpts, InsertOutcome};

use anyhow::{Context, Result};
use cache::ContentCache;
use cliphistory_proto::Content;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
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
    pinned_at   INTEGER,
    use_count   INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_entries_created ON entries (created_at DESC);
"#;

/// Add columns introduced after a database was created. Idempotent: each
/// statement runs only when the column is absent from `entries`.
fn migrate(conn: &Connection) -> Result<()> {
    let has = |col: &str| -> Result<bool> {
        let mut stmt = conn.prepare("PRAGMA table_info(entries)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(cols.iter().any(|c| c == col))
    };
    if !has("pinned_at")? {
        conn.execute_batch("ALTER TABLE entries ADD COLUMN pinned_at INTEGER;")?;
    }
    Ok(())
}

pub struct Storage {
    conn: Mutex<Connection>,
    /// Hot payload LRU in front of the blob reads. Guarded by its own
    /// mutex, always taken *after* `conn` — the two are never nested the
    /// other way around.
    cache: Mutex<ContentCache>,
    path: PathBuf,
}

impl Storage {
    pub fn open(path: &Path, max_cache_bytes: i64) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA).context("initialising schema")?;
        migrate(&conn).context("running schema migrations")?;
        Ok(Self {
            conn: Mutex::new(conn),
            cache: Mutex::new(ContentCache::new(max_cache_bytes.max(0) as usize)),
            path: path.to_path_buf(),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::open_in_memory_with(crate::constants::DEFAULT_MAX_CACHE_BYTES)
    }

    #[cfg(test)]
    pub fn open_in_memory_with(max_cache_bytes: i64) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            cache: Mutex::new(ContentCache::new(max_cache_bytes.max(0) as usize)),
            path: PathBuf::from(":memory:"),
        })
    }

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

    /// Store `content`, deduplicating by hash. Thumbnails form a cache keyed
    /// by content hash and never block or fail the entry itself.
    pub fn insert(&self, content: &Content, opts: InsertOpts) -> Result<InsertOutcome> {
        let bytes = content.bytes();
        if bytes.len() as i64 > opts.max_item_size {
            return Ok(InsertOutcome::TooLarge {
                size: bytes.len() as i64,
                limit: opts.max_item_size,
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
        let outcome = if updated > 0 {
            InsertOutcome::Duplicate(conn.query_row(
                "SELECT id FROM entries WHERE hash = ?1",
                rusqlite::params![hash],
                |r| r.get(0),
            )?)
        } else {
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
            InsertOutcome::Inserted(conn.last_insert_rowid())
        };
        drop(conn);

        if let Some(id) = outcome.id() {
            // A fresh copy is the most likely next paste; keep it hot.
            self.cache
                .lock()
                .expect("storage lock poisoned")
                .put(id, content.clone());
        }

        if matches!(content, Content::Image { .. }) && opts.thumbnail_size > 0 {
            if let Err(e) = self.ensure_thumbnail(&hash, &bytes, opts.thumbnail_size) {
                log::warn!("thumbnail generation failed: {e:#}");
            }
        }
        Ok(outcome)
    }
}

pub fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

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
            s.insert(&text("a"), InsertOpts::sized(100)).unwrap(),
            InsertOutcome::Inserted(_)
        ));
        assert!(matches!(
            s.insert(&text("b"), InsertOpts::sized(100)).unwrap(),
            InsertOutcome::Inserted(_)
        ));
        assert!(matches!(
            s.insert(&text("a"), InsertOpts::sized(100)).unwrap(),
            InsertOutcome::Duplicate(_)
        ));
        assert_eq!(s.count().unwrap(), 2);
        assert_eq!(s.history_items(None, None).unwrap()[0].preview, "a");
    }

    #[test]
    fn too_large_rejected() {
        let s = Storage::open_in_memory().unwrap();
        assert_eq!(
            s.insert(&text("big"), InsertOpts::sized(2)).unwrap(),
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
        let id = match s.insert(&img, InsertOpts::sized(1024)).unwrap() {
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
            s.insert(&text(t), InsertOpts::sized(1000)).unwrap();
        }
        assert_eq!(s.history_items(Some(2), None).unwrap().len(), 2);
        let hits = s.history_items(None, Some("alpha")).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn pruning_respects_pins() {
        let s = Storage::open_in_memory().unwrap();
        for i in 0..10 {
            s.insert(&text(&format!("e{i}")), InsertOpts::sized(1000))
                .unwrap();
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
        s.insert(&text("keep"), InsertOpts::sized(1000)).unwrap();
        s.insert(&text("drop"), InsertOpts::sized(1000)).unwrap();
        s.set_pinned(1, true).unwrap();
        assert_eq!(s.clear().unwrap(), 1);
        assert_eq!(s.count().unwrap(), 1);
    }

    #[test]
    fn delete_reports_existence() {
        let s = Storage::open_in_memory().unwrap();
        let id = match s.insert(&text("x"), InsertOpts::sized(100)).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };
        assert!(s.delete(id).unwrap());
        assert!(!s.delete(id).unwrap());
    }

    #[test]
    fn content_is_cached_and_invalidation_is_precise() {
        // Tiny budget so eviction behaviour is observable through reads.
        let s = Storage::open_in_memory_with(64).unwrap();
        let id = match s.insert(&text("hot"), InsertOpts::sized(100)).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };
        // insert warmed the cache; the read is served from it.
        assert_eq!(s.content(id).unwrap().unwrap(), text("hot"));

        // Deleting a cached row must invalidate its copy.
        assert!(s.delete(id).unwrap());
        assert_eq!(s.content(id).unwrap(), None);
    }

    #[test]
    fn cache_survives_prune_of_other_entries() {
        // Budget holds exactly one payload: pruning older rows must not
        // flush the survivor's cached copy (prune runs after each insert).
        let s = Storage::open_in_memory_with(8).unwrap();
        s.insert(&text("old"), InsertOpts::sized(100)).unwrap();
        s.insert(&text("new1"), InsertOpts::sized(100)).unwrap();
        let keep_id = match s.insert(&text("keep"), InsertOpts::sized(100)).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };
        // Warm the cache for `keep` explicitly (insert of 3rd evicted it).
        assert_eq!(s.content(keep_id).unwrap().unwrap(), text("keep"));
        // Age/count prune removes nothing here; force budget pressure via
        // max_total_bytes: evicts oldest unpinned ("old", maybe "new1").
        let removed = s
            .prune_with_budget(0, 0, Some(10))
            .expect("prune with budget");
        assert!(removed >= 1);
        assert_eq!(
            s.content(keep_id).unwrap().unwrap(),
            text("keep"),
            "survivor must stay hot across prune"
        );
    }

    #[test]
    fn clear_flushes_cache_too() {
        let s = Storage::open_in_memory_with(100).unwrap();
        let id = match s.insert(&text("doomed"), InsertOpts::sized(100)).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };
        assert!(s.content(id).unwrap().is_some());
        s.clear().unwrap();
        assert_eq!(s.content(id).unwrap(), None);
    }

    #[test]
    fn thumbnails_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(
            &tmp.path().join("history.db"),
            crate::constants::DEFAULT_MAX_CACHE_BYTES,
        )
        .unwrap();

        let png: Content = {
            // 4x2 red PNG built through the image crate.
            let img = image::RgbImage::from_fn(4, 2, |_, _| image::Rgb([255, 0, 0]));
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgb8(img)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            Content::Image {
                mime: "image/png".into(),
                data: bytes,
                width: Some(4),
                height: Some(2),
            }
        };

        let opts = InsertOpts {
            max_item_size: i64::MAX,
            thumbnail_size: 32,
        };
        let id = match s.insert(&png, opts).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };

        let thumb_path = s
            .thumbs_dir()
            .unwrap()
            .join(format!("{}.png", content_hash(&png.bytes())));
        assert!(thumb_path.exists(), "thumbnail cached on disk");

        let items = s.history_items(None, None).unwrap();
        assert_eq!(
            items[0].thumbnail.as_deref(),
            Some(thumb_path.to_str().unwrap())
        );

        // Deleting the entry removes its preview too.
        s.delete(id).unwrap();
        assert!(!thumb_path.exists());

        // Re-inserting regenerates the cache.
        let id2 = match s.insert(&png, opts).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };
        assert!(thumb_path.exists());
        s.clear().unwrap();
        assert!(!thumb_path.exists());
        let _ = id2;
    }

    #[test]
    fn mark_used_bumps_counter() {
        let s = Storage::open_in_memory().unwrap();
        let id = match s.insert(&text("x"), InsertOpts::sized(100)).unwrap() {
            InsertOutcome::Inserted(id) => id,
            o => panic!("{o:?}"),
        };
        s.mark_used(id).unwrap();
        assert_eq!(s.history_items(None, None).unwrap()[0].use_count, 1);
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn payload_budget_evicts_oldest_unpinned() {
        let s = Storage::open_in_memory().unwrap();
        // Three ~120-byte entries; a 250-byte budget admits only the two
        // newest.
        for t in [
            format!("old-{}", "x".repeat(116)),
            format!("mid-{}", "x".repeat(116)),
            "newest-entry".to_string(),
        ] {
            s.insert(
                &Content::Text { text: t },
                InsertOpts {
                    max_item_size: 1000,
                    thumbnail_size: 0,
                },
            )
            .unwrap();
        }
        s.prune_with_budget(0, 0, Some(250)).unwrap();
        let left = s.history_items(None, None).unwrap();
        assert_eq!(left.len(), 2, "oldest evicted to fit the budget");
        assert_eq!(left.last().unwrap().preview, "newest-entry");
    }

    #[test]
    fn pinned_entries_are_budget_exempt() {
        let s = Storage::open_in_memory().unwrap();
        for i in 0..3 {
            let id = match s.insert(
                &Content::Text {
                    text: format!("e{i}"),
                },
                InsertOpts {
                    max_item_size: 1000,
                    thumbnail_size: 0,
                },
            ) {
                Ok(InsertOutcome::Inserted(id)) => id,
                o => panic!("{o:?}"),
            };
            if i == 0 {
                s.set_pinned(id, true).unwrap();
            }
        }
        // Budget fits nothing beyond pinned; eviction must stop there.
        s.prune_with_budget(0, 0, Some(1)).unwrap();
        assert_eq!(s.count().unwrap(), 1);
    }

    #[test]
    fn zero_budget_disables_size_cap() {
        let s = Storage::open_in_memory().unwrap();
        s.insert(&Content::Text { text: "x".into() }, InsertOpts::sized(10))
            .unwrap();
        s.prune_with_budget(0, 0, Some(0)).unwrap();
        assert_eq!(s.count().unwrap(), 1);
    }
}
