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

        let deleted = self
            .conn()
            .execute(
                "DELETE FROM cache WHERE created_at < ?1",
                libsql::params![cutoff],
            )
            .await
            .context("Failed to evict by TTL")?;

        Ok(deleted)
    }

    /// Delete least-recently-used entries to keep total entries at or below `max_entries`.
    /// Returns the number of deleted entries.
    pub async fn evict_lru(&self, max_entries: u64) -> Result<u64> {
        let count = self.entry_count().await?;
        if count <= max_entries {
            return Ok(0);
        }

        let to_delete = count - max_entries;

        // Delete entries with the oldest last_hit timestamps
        let deleted = self
            .conn()
            .execute(
                "DELETE FROM cache WHERE id IN (
                    SELECT id FROM cache ORDER BY last_hit ASC, created_at ASC LIMIT ?1
                )",
                libsql::params![to_delete as i64],
            )
            .await
            .context("Failed to evict LRU entries")?;

        Ok(deleted)
    }

    /// Delete all cache entries. Returns the number of deleted entries.
    pub async fn clear_all(&self) -> Result<u64> {
        let deleted = self
            .conn()
            .execute("DELETE FROM cache", ())
            .await
            .context("Failed to clear cache")?;

        Ok(deleted)
    }
}
