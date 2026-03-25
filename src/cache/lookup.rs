use anyhow::{Context, Result};

use super::db::CacheDb;

/// A cache hit result containing the matched entry data and similarity score.
pub struct CacheHit {
    pub id: i64,
    pub prompt_text: String,
    pub response_data: Vec<u8>,
    pub similarity: f32,
}

impl CacheDb {
    /// Look up a cache entry by vector similarity.
    ///
    /// Returns `Some(CacheHit)` if a match is found with similarity >= threshold,
    /// or `None` if no sufficiently similar entry exists.
    pub async fn lookup(
        &self,
        embedding: &[f32],
        system_hash: &str,
        model: &str,
        threshold: f32,
        match_system_prompt: bool,
    ) -> Result<Option<CacheHit>> {
        let embedding_json = format!(
            "[{}]",
            embedding
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        // cosine distance: 0 = identical, 1 = orthogonal
        // threshold is similarity (e.g. 0.95), so max distance = 1.0 - threshold
        let max_distance = 1.0 - threshold;

        // Acquire the mutex for the entire SELECT + UPDATE to avoid a race condition.
        let conn = self.conn.lock().await;

        // Use vector_top_k with k=10 so the WHERE clause on system_hash/model can
        // filter candidates; k=1 would miss valid matches if the globally nearest
        // neighbor belongs to a different system/model.
        let query = if match_system_prompt {
            "SELECT c.id, c.prompt_text, c.response_data,
                    vector_distance_cos(c.embedding, vector32(?1)) AS dist
             FROM vector_top_k('cache_vec_idx', vector32(?1), 10) AS v
             JOIN cache AS c ON c.rowid = v.id
             WHERE c.system_hash = ?2 AND c.model = ?3"
        } else {
            "SELECT c.id, c.prompt_text, c.response_data,
                    vector_distance_cos(c.embedding, vector32(?1)) AS dist
             FROM vector_top_k('cache_vec_idx', vector32(?1), 10) AS v
             JOIN cache AS c ON c.rowid = v.id
             WHERE c.model = ?3"
        };

        let mut rows = conn
            .query(
                query,
                libsql::params![embedding_json, system_hash, model],
            )
            .await
            .context("Failed to execute cache lookup query")?;

        let row = match rows.next().await? {
            Some(row) => row,
            None => return Ok(None),
        };

        let id: i64 = row.get(0)?;
        let prompt_text: String = row.get(1)?;
        let response_data = row.get::<libsql::Value>(2)?;
        let distance: f64 = row.get(3)?;
        let similarity = 1.0 - distance as f32;

        // Check if similarity meets threshold
        if distance > max_distance as f64 {
            return Ok(None);
        }

        let response_data = match response_data {
            libsql::Value::Blob(b) => b,
            _ => anyhow::bail!("Expected blob for response_data"),
        };

        // Update hit_count and last_hit within the same lock scope — no separate
        // transaction needed since we hold the Mutex throughout.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("System time before UNIX epoch")?
            .as_secs() as i64;

        conn.execute(
            "UPDATE cache SET hit_count = hit_count + 1, last_hit = ?1 WHERE id = ?2",
            libsql::params![now, id],
        )
        .await
        .context("Failed to update hit count")?;

        Ok(Some(CacheHit {
            id,
            prompt_text,
            response_data,
            similarity,
        }))
    }
}
