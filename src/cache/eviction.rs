use anyhow::{Context, Result};

use super::db::CacheDb;

impl CacheDb {
    /// Delete cache entries older than `max_age_secs` seconds.
    /// Returns the number of deleted entries.
    pub async fn evict_by_ttl(&self, max_age_secs: u64) -> Result<u64> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("System time before UNIX epoch")?
            .as_secs() as i64
            - max_age_secs as i64;

        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM cache WHERE created_at < ?1",
                libsql::params![cutoff],
            )
            .await
            .context("Failed to evict by TTL")?;

        Ok(deleted)
    }

    /// Delete least-recently-used entries to keep total entries at or below `max_entries`.
    /// The count and delete are combined into a single atomic SQL statement.
    /// Returns the number of deleted entries.
    #[allow(dead_code)]
    pub async fn evict_lru(&self, max_entries: u64) -> Result<u64> {
        let conn = self.conn.lock().await;

        // A single atomic statement: delete all entries beyond the max_entries limit,
        // ordered by least-recently-used. MAX(0, count - max_entries) ensures we
        // delete nothing when already within the limit.
        let deleted = conn
            .execute(
                "DELETE FROM cache WHERE id IN (
                    SELECT id FROM cache ORDER BY last_hit ASC, created_at ASC
                    LIMIT MAX(0, (SELECT COUNT(*) FROM cache) - ?1)
                )",
                libsql::params![max_entries as i64],
            )
            .await
            .context("Failed to evict LRU entries")?;

        Ok(deleted)
    }

    /// Delete all cache entries. Returns the number of deleted entries.
    pub async fn clear_all(&self) -> Result<u64> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute("DELETE FROM cache", ())
            .await
            .context("Failed to clear cache")?;

        Ok(deleted)
    }
}
