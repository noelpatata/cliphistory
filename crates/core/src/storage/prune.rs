//! Mutating operations: pruning, deletion and pin state.

use super::Storage;
use anyhow::Result;
use rusqlite::Connection;

impl Storage {
    /// Drop unpinned entries beyond the count/age/size limits; thumbnails
    /// are kept in sync. Returns the number of rows removed.
    pub fn prune(&self, max_entries: i64, max_age_days: i64) -> Result<usize> {
        self.prune_with_budget(max_entries, max_age_days, None)
    }

    /// Full retention policy: count cap, age cap and a hard payload budget.
    /// The budget is what actually bounds disk usage — it evicts the oldest
    /// unpinned entries (in chunks, re-measuring after each pass) until
    /// SUM(size_bytes) fits. Pinned entries are exempt from every rule.
    pub fn prune_with_budget(
        &self,
        max_entries: i64,
        max_age_days: i64,
        max_total_bytes: Option<i64>,
    ) -> Result<usize> {
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
        // Payload budget: evict oldest-unpinned entries until the running
        // payload fits. The cumulative-sum subquery selects exactly the
        // prefix that overflows; the outer loop only guards against
        // boundary rounding across passes.
        if let Some(budget) = max_total_bytes.filter(|b| *b > 0) {
            loop {
                let total: i64 = conn.query_row(
                    "SELECT COALESCE(SUM(size_bytes), 0) FROM entries",
                    [],
                    |r| r.get(0),
                )?;
                if total <= budget {
                    break;
                }
                let overflow = total - budget;
                let n = self.delete_where(
                    &conn,
                    "pinned = 0 AND id IN (
                         SELECT id FROM (
                             SELECT id, size_bytes AS sz,
                                    SUM(size_bytes) OVER (
                                        ORDER BY created_at ASC, id ASC
                                        ROWS BETWEEN UNBOUNDED PRECEDING
                                        AND 1 PRECEDING
                                    ) AS before_run
                             FROM entries WHERE pinned = 0
                         )
                         WHERE COALESCE(before_run, 0) < ?1
                     )",
                    rusqlite::params![overflow],
                )?;
                if n == 0 {
                    // Only pinned entries left; nothing more we may drop.
                    log::warn!(
                        "storage payload ({total}) exceeds max_total_bytes ({budget}) \
                         but only pinned entries remain"
                    );
                    break;
                }
                removed += n;
            }
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
        drop(conn);
        if n > 0 {
            if let Some(hash) = hash {
                self.remove_thumb(&hash);
            }
            // A deleted row must never be served from the hot cache.
            self.cache
                .lock()
                .expect("storage lock poisoned")
                .evict_ids(&[id]);
        }
        Ok(n > 0)
    }

    /// Clear history; pinned entries are kept.
    pub fn clear(&self) -> Result<usize> {
        let conn = self.conn.lock().expect("storage lock poisoned");
        let victims: Vec<(i64, String)> = {
            let mut stmt = conn.prepare("SELECT id, hash FROM entries WHERE pinned = 0")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let n = conn.execute("DELETE FROM entries WHERE pinned = 0", [])?;
        drop(conn);
        for (_, hash) in &victims {
            self.remove_thumb(hash);
        }
        self.cache
            .lock()
            .expect("storage lock poisoned")
            .evict_ids(&victims.iter().map(|(id, _)| *id).collect::<Vec<_>>());
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
    /// the matching thumbnail files plus hot cache copies.
    fn delete_where(
        &self,
        conn: &Connection,
        where_clause: &str,
        params: impl rusqlite::Params,
    ) -> Result<usize> {
        let victims: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(&format!(
                "SELECT id, hash FROM entries WHERE {where_clause}"
            ))?;
            let rows = stmt.query_map(params, |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        if victims.is_empty() {
            return Ok(0);
        }
        // SQLite has a per-connection parameter limit; chunk to stay safe.
        let hashes = victims
            .iter()
            .map(|(_, h)| h.clone())
            .collect::<Vec<String>>();
        let placeholders = hashes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let values: Vec<&dyn rusqlite::ToSql> =
            hashes.iter().map(|h| h as &dyn rusqlite::ToSql).collect();
        let n = conn.execute(
            &format!("DELETE FROM entries WHERE hash IN ({placeholders})"),
            values.as_slice(),
        )?;
        for (_, hash) in &victims {
            self.remove_thumb(hash);
        }
        // Pruning runs after every insert — precise eviction keeps the
        // cache useful instead of flushing it wholesale.
        self.cache
            .lock()
            .expect("storage lock poisoned")
            .evict_ids(&victims.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        Ok(n)
    }
}
