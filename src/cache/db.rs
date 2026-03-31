use std::path::Path;

use anyhow::{Context, Result};
use libsql::{Connection, Database};
use tokio::sync::Mutex;

/// Embedding dimension for the cache vector index.
pub const EMBEDDING_DIM: usize = 384;

pub struct CacheDb {
    #[allow(dead_code)]
    db: Database,
    pub(crate) conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cache (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    prompt_text TEXT NOT NULL,
    system_hash TEXT NOT NULL,
    model TEXT NOT NULL,
    embedding F32_BLOB(384),
    response_data BLOB NOT NULL,
    response_size INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    hit_count INTEGER DEFAULT 0,
    last_hit INTEGER DEFAULT 0,
    source TEXT NOT NULL DEFAULT 'local'
);

CREATE INDEX IF NOT EXISTS cache_vec_idx ON cache (
    libsql_vector_idx(embedding, 'metric=cosine')
);

CREATE INDEX IF NOT EXISTS cache_created_idx ON cache (created_at);

CREATE INDEX IF NOT EXISTS cache_last_hit_idx ON cache (last_hit);
"#;

async fn apply_schema(conn: &Connection) -> Result<()> {
    for statement in SCHEMA.split(';') {
        let statement = statement.trim();
        if !statement.is_empty() {
            conn.execute(statement, ())
                .await
                .with_context(|| format!("Failed to execute schema statement: {statement}"))?;
        }
    }
    // Migration: add 'source' column to existing databases (ignore error if already present)
    let _ = conn
        .execute(
            "ALTER TABLE cache ADD COLUMN source TEXT NOT NULL DEFAULT 'local'",
            (),
        )
        .await;
    Ok(())
}

impl CacheDb {
    /// Open the cache database at the given path, creating schema if needed.
    pub async fn open(db_path: &Path) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let db_path_str = db_path.to_str().context("Invalid database path")?;
        let db = libsql::Builder::new_local(db_path_str)
            .build()
            .await
            .context("Failed to open libSQL database")?;

        let conn = db.connect().context("Failed to connect to database")?;
        apply_schema(&conn).await?;

        Ok(Self { db, conn: Mutex::new(conn) })
    }

    /// Open an in-memory database (for tests).
    #[cfg(test)]
    pub async fn open_in_memory() -> Result<Self> {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .context("Failed to open in-memory database")?;

        let conn = db.connect().context("Failed to connect to database")?;
        apply_schema(&conn).await?;

        Ok(Self { db, conn: Mutex::new(conn) })
    }

    /// Return the number of cached entries.
    pub async fn entry_count(&self) -> Result<u64> {
        let conn = self.conn.lock().await;
        let mut rows = conn.query("SELECT COUNT(*) FROM cache", ()).await?;
        let row = rows.next().await?.context("No result from COUNT")?;
        let count: i64 = row.get(0)?;
        Ok(count as u64)
    }

    /// Return the total size of all cached response data.
    pub async fn total_size(&self) -> Result<u64> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query("SELECT COALESCE(SUM(response_size), 0) FROM cache", ())
            .await?;
        let row = rows.next().await?.context("No result from SUM")?;
        let size: i64 = row.get(0)?;
        Ok(size as u64)
    }

    /// Return the total number of cache hits across all entries.
    pub async fn total_hits(&self) -> Result<u64> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query("SELECT COALESCE(SUM(hit_count), 0) FROM cache", ())
            .await?;
        let row = rows.next().await?.context("No result from SUM")?;
        let hits: i64 = row.get(0)?;
        Ok(hits as u64)
    }
}
