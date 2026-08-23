//! Mutating operations: pruning, deletion and pin state.

use super::Storage;
use anyhow::Result;
use rusqlite::Connection;

impl Storage {
    /// Drop unpinned entries beyond the count/age limits; thumbnails are
    /// kept in sync. Returns the number of rows removed.
    pub fn prune(&self, max_entries: i64, max_age_days: i64) -> Result<usize> {
        use crate::constants as c;
        let conn = self.conn.lock().expect("storage lock poisoned");
        let mut removed = 0;
        if max_age_days > 0 {
            let cutoff = super::unix_now() as i64 - max_age_days * c::SECS_PER_DAY as i64;
            removed += self.delete_where(
                &conn,
                "pinned = 0 AND created_at < ?1",
                rusqlite::params![cutoff],
            )?;
        }
        if max_entries > 0 {
            removed += self.delete_where(
                &conn,
                "pinned = 0 AND id NOT IN (
                     SELECT id FROM entries ORDER BY pinned DESC, created_at DESC LIMIT ?1
                 )",
                rusqlite::params![max_entries],
            )?;
        }
        Ok(removed)
    }

    /// Delete one entry by id (with its thumbnail).
    pub fn delete(&self, id: i64) -> Result<bool> {
        let conn = self.conn.lock().expect("storage lock poisoned");
        let hash: Option<String> = conn
            .query_row(
                "SELECT hash FROM entries WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .ok();
        let n = conn.execute("DELETE FROM entries WHERE id = ?1", rusqlite::params![id])?;
        if n > 0 {
            if let Some(hash) = hash {
                self.remove_thumb(&hash);
            }
        }
        Ok(n > 0)
    }

    /// Clear history; pinned entries are kept.
    pub fn clear(&self) -> Result<usize> {
        let conn = self.conn.lock().expect("storage lock poisoned");
        let hashes: Vec<String> = {
            let mut stmt = conn.prepare("SELECT hash FROM entries WHERE pinned = 0")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let n = conn.execute("DELETE FROM entries WHERE pinned = 0", [])?;
        for hash in &hashes {
            self.remove_thumb(hash);
        }
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
            rusqlite::params![id, super::unix_now() as i64],
        )?;
        Ok(())
    }

    /// Delete rows matching `where_clause`; returns rows removed and drops
    /// the matching thumbnail files.
    fn delete_where(
        &self,
        conn: &Connection,
        where_clause: &str,
        params: impl rusqlite::Params,
    ) -> Result<usize> {
        let hashes: Vec<String> = {
            let mut stmt =
                conn.prepare(&format!("SELECT hash FROM entries WHERE {where_clause}"))?;
            let rows = stmt.query_map(params, |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        if hashes.is_empty() {
            return Ok(0);
        }
        // SQLite has a per-connection parameter limit; chunk to stay safe.
        let placeholders = hashes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let values: Vec<&dyn rusqlite::ToSql> =
            hashes.iter().map(|h| h as &dyn rusqlite::ToSql).collect();
        let n = conn.execute(
            &format!("DELETE FROM entries WHERE hash IN ({placeholders})"),
            values.as_slice(),
        )?;
        for hash in &hashes {
            self.remove_thumb(hash);
        }
        Ok(n)
    }
}
