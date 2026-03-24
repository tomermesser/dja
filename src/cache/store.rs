use anyhow::{Context, Result};

use super::db::CacheDb;

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

        let mut rows = self
            .conn()
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
}
