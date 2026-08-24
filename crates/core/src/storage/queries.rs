//! Read-only queries: counts, history listings and payload reconstruction.

use super::{model::ContentHead, Storage};
use anyhow::Result;
use std::io::Read;

use cliphistory_proto::{Content, HistoryItem};

impl Storage {
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
            "SELECT id, kind, mime, size_bytes, preview, created_at, use_count, pinned, hash
             FROM entries
             WHERE (?1 IS NULL OR preview LIKE '%' || ?1 || '%')
             ORDER BY pinned DESC, created_at DESC
             LIMIT {limit_sql}"
        );
        let conn = self.conn.lock().expect("storage lock poisoned");
        let mut stmt = conn.prepare(&sql)?;
        let like = query.map(|q| format!("%{q}%"));
        let rows = stmt.query_map(rusqlite::params![like], |r| {
            Ok((
                HistoryItem {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    mime: r.get(2)?,
                    size_bytes: r.get::<_, i64>(3)? as u64,
                    preview: r.get(4)?,
                    created_at: r.get::<_, i64>(5)? as u64,
                    use_count: r.get::<_, i64>(6)? as u64,
                    pinned: r.get::<_, i64>(7)? != 0,
                    thumbnail: None,
                },
                r.get::<_, String>(8)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (mut item, hash) = row?;
            if item.kind == "image" {
                item.thumbnail = self
                    .thumb_path(&hash)
                    .filter(|p| p.exists())
                    .map(|p| p.canonicalize().unwrap_or(p).display().to_string());
            }
            items.push(item);
        }
        Ok(items)
    }

    /// First bytes of a stored payload, without loading the whole blob.
    ///
    /// Lets frontends render multi-line previews for text entries while
    /// keeping multi-megabyte payloads out of the IPC path entirely.
    pub fn content_head(&self, id: i64, max_bytes: usize) -> Result<Option<ContentHead>> {
        let conn = self.conn.lock().expect("storage lock poisoned");
        // substr() on a BLOB slices bytes, so `max_bytes` bounds the read.
        let mut stmt = conn.prepare(
            "SELECT kind, substr(data, 1, ?2) FROM entries WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![id, max_bytes as i64])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let kind: String = row.get(0)?;
        let head: Vec<u8> = row.get_ref(1)?.as_blob()?.to_vec();
        Ok(Some(ContentHead {
            kind,
            text: String::from_utf8_lossy(&head).into_owned(),
        }))
    }

    /// Full payload of an entry, reconstructed as [`Content`].
    ///
    /// Served from the hot payload cache when possible; disk reads
    /// repopulate it. Pasting is the hot path here.
    pub fn content(&self, id: i64) -> Result<Option<Content>> {
        {
            let mut cache = self.cache.lock().expect("storage lock poisoned");
            if let Some(cached) = cache.get(id) {
                log::debug!("content {id}: cache hit, {}B", cached.bytes().len());
                return Ok(Some(cached));
            }
        }
        let loaded = self.content_from_disk(id)?;
        log::debug!("content {id}: cache miss, reading blob from db");
        if let Some(content) = &loaded {
            self.cache
                .lock()
                .expect("storage lock poisoned")
                .put(id, content.clone());
        }
        Ok(loaded)
    }

    fn content_from_disk(&self, id: i64) -> Result<Option<Content>> {
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
}
