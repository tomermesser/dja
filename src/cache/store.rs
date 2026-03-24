use anyhow::{Context, Result};

use super::db::CacheDb;

/// A cache entry for export/import.
pub struct CacheEntry {
    pub prompt_text: String,
    pub model: String,
    pub system_hash: String,
    pub response_data: Vec<u8>,
}

impl CacheDb {
    /// Store a new cache entry. Returns the inserted row ID.
    pub async fn store(
        &self,
        prompt: &str,
        system_hash: &str,
        model: &str,
        embedding: &[f32],
        response_data: &[u8],
    ) -> Result<i64> {
        let response_size = response_data.len() as i64;
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("System time before UNIX epoch")?
            .as_secs() as i64;

        // Format embedding as JSON array for vector32()
        let embedding_json = format!(
            "[{}]",
            embedding
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "INSERT INTO cache (prompt_text, system_hash, model, embedding, response_data, response_size, created_at)
                 VALUES (?1, ?2, ?3, vector32(?4), ?5, ?6, ?7)
                 RETURNING id",
                libsql::params![
                    prompt,
                    system_hash,
                    model,
                    embedding_json,
                    response_data.to_vec(),
                    response_size,
                    created_at,
                ],
            )
            .await
            .context("Failed to insert cache entry")?;

        let row = rows.next().await?.context("No row returned from INSERT")?;
        let id: i64 = row.get(0)?;
        Ok(id)
    }

    /// Export all cache entries (without embeddings).
    pub async fn export_all(&self) -> Result<Vec<CacheEntry>> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT prompt_text, model, system_hash, response_data FROM cache ORDER BY id",
                (),
            )
            .await
            .context("Failed to query cache entries for export")?;

        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            let prompt_text: String = row.get(0)?;
            let model: String = row.get(1)?;
            let system_hash: String = row.get(2)?;
            let response_data = match row.get::<libsql::Value>(3)? {
                libsql::Value::Blob(b) => b,
                _ => continue,
            };
            entries.push(CacheEntry {
                prompt_text,
                model,
                system_hash,
                response_data,
            });
        }
        Ok(entries)
    }

    /// Check if an entry with the given prompt_text, system_hash, and model already exists.
    pub async fn exists(&self, prompt_text: &str, system_hash: &str, model: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT 1 FROM cache WHERE prompt_text = ?1 AND system_hash = ?2 AND model = ?3 LIMIT 1",
                libsql::params![prompt_text, system_hash, model],
            )
            .await
            .context("Failed to check for existing cache entry")?;
        Ok(rows.next().await?.is_some())
    }

    /// Return cache hit counts grouped by day for the last N days.
    /// Returns a vec of (date_string, hit_count) tuples, ordered by date.
    pub async fn hits_by_day(&self, days: u32) -> Result<Vec<(String, u64)>> {
        let conn = self.conn.lock().await;
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("System time before UNIX epoch")?
            .as_secs() as i64
            - (days as i64 * 86400);

        let mut rows = conn
            .query(
                "SELECT date(last_hit, 'unixepoch') AS day, SUM(hit_count) AS hits
                 FROM cache
                 WHERE last_hit > ?1 AND hit_count > 0
                 GROUP BY day
                 ORDER BY day",
                libsql::params![cutoff],
            )
            .await
            .context("Failed to query hits by day")?;

        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            let day: String = row.get(0)?;
            let hits: i64 = row.get(1)?;
            result.push((day, hits as u64));
        }
        Ok(result)
    }
}
