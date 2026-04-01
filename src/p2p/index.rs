use anyhow::{Context, Result};
use libsql::Connection;

/// A hit returned from the central index query.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexHit {
    pub peer_id: String,
    pub content_hash: String,
    pub similarity: f32,
}

/// Peer address information from the central index.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerInfo {
    pub peer_id: String,
    pub display_name: String,
    pub public_addr: String,
}

/// Client for the central Turso index.
///
/// The caller is responsible for opening the `libsql::Connection` with the
/// appropriate Turso URL and auth token. `IndexClient` only borrows the
/// connection for its operations.
///
/// # Central index schema (hosted on Turso — not created by this client)
///
/// ```sql
/// CREATE TABLE IF NOT EXISTS index_entries (
///     id INTEGER PRIMARY KEY AUTOINCREMENT,
///     peer_id TEXT NOT NULL,
///     content_hash TEXT NOT NULL,
///     model TEXT NOT NULL,
///     system_hash TEXT NOT NULL,
///     embedding F32_BLOB(384),
///     response_size INTEGER NOT NULL,
///     created_at INTEGER NOT NULL,
///     UNIQUE(peer_id, content_hash)
/// );
///
/// CREATE TABLE IF NOT EXISTS peers (
///     peer_id TEXT PRIMARY KEY,
///     display_name TEXT NOT NULL DEFAULT '',
///     public_addr TEXT NOT NULL,
///     last_seen INTEGER NOT NULL,
///     version TEXT NOT NULL DEFAULT ''
/// );
/// ```
pub struct IndexClient {
    conn: Connection,
}

impl IndexClient {
    /// Create an `IndexClient` wrapping an already-opened `libsql::Connection`.
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }

    /// Publish (or replace) a cached entry in the central index.
    ///
    /// Uses `INSERT OR REPLACE` so re-publishing the same
    /// `(peer_id, content_hash)` pair is idempotent.
    pub async fn publish(
        &self,
        peer_id: &str,
        content_hash: &str,
        model: &str,
        system_hash: &str,
        embedding: &[f32],
        response_size: usize,
    ) -> Result<()> {
        let embedding_json = encode_embedding_json(embedding);
        let now = unix_now()?;

        self.conn
            .execute(
                "INSERT OR REPLACE INTO index_entries
                 (peer_id, content_hash, model, system_hash, embedding, response_size, created_at)
                 VALUES (?1, ?2, ?3, ?4, vector32(?5), ?6, ?7)",
                libsql::params![
                    peer_id,
                    content_hash,
                    model,
                    system_hash,
                    embedding_json,
                    response_size as i64,
                    now
                ],
            )
            .await
            .context("Failed to publish entry to central index")?;

        Ok(())
    }

    /// Query the central index for a semantically similar cached entry.
    ///
    /// Excludes entries owned by `self_peer_id` so peers don't fetch from
    /// themselves. Returns the best hit whose similarity is >= `threshold`,
    /// or `None` if no match is found.
    pub async fn query(
        &self,
        self_peer_id: &str,
        embedding: &[f32],
        model: &str,
        threshold: f32,
        match_system_prompt: bool,
        system_hash: &str,
    ) -> Result<Option<IndexHit>> {
        let embedding_json = encode_embedding_json(embedding);
        let max_distance = 1.0 - threshold;

        // Two query variants: with or without system_hash filtering.
        // vector_distance_cos returns 0 for identical, 1 for orthogonal.
        let mut rows = if match_system_prompt {
            self.conn
                .query(
                    "SELECT peer_id, content_hash,
                            vector_distance_cos(embedding, vector32(?1)) AS dist
                     FROM index_entries
                     WHERE model = ?2 AND peer_id != ?3 AND system_hash = ?4
                     ORDER BY dist ASC
                     LIMIT 5",
                    libsql::params![embedding_json, model, self_peer_id, system_hash],
                )
                .await
                .context("Failed to query central index (match_system_prompt=true)")?
        } else {
            self.conn
                .query(
                    "SELECT peer_id, content_hash,
                            vector_distance_cos(embedding, vector32(?1)) AS dist
                     FROM index_entries
                     WHERE model = ?2 AND peer_id != ?3
                     ORDER BY dist ASC
                     LIMIT 5",
                    libsql::params![embedding_json, model, self_peer_id],
                )
                .await
                .context("Failed to query central index (match_system_prompt=false)")?
        };

        // Iterate results (already ordered by distance ASC) and return the
        // first one that passes the threshold.
        while let Some(row) = rows.next().await? {
            let peer_id: String = row.get(0)?;
            let content_hash: String = row.get(1)?;
            let distance: f64 = row.get(2)?;

            if distance <= max_distance as f64 {
                let similarity = 1.0 - distance as f32;
                return Ok(Some(IndexHit { peer_id, content_hash, similarity }));
            }
        }

        Ok(None)
    }

    /// Send a heartbeat to the central index, registering or refreshing this
    /// peer's presence in the `peers` table.
    pub async fn heartbeat(
        &self,
        peer_id: &str,
        display_name: &str,
        public_addr: &str,
        version: &str,
    ) -> Result<()> {
        let now = unix_now()?;

        self.conn
            .execute(
                "INSERT INTO peers (peer_id, display_name, public_addr, last_seen, version)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(peer_id) DO UPDATE SET
                     display_name = excluded.display_name,
                     public_addr  = excluded.public_addr,
                     last_seen    = excluded.last_seen,
                     version      = excluded.version",
                libsql::params![peer_id, display_name, public_addr, now, version],
            )
            .await
            .context("Failed to send heartbeat to central index")?;

        Ok(())
    }

    /// Look up a peer's address information from the `peers` table.
    pub async fn resolve_peer(&self, peer_id: &str) -> Result<Option<PeerInfo>> {
        let mut rows = self
            .conn
            .query(
                "SELECT peer_id, display_name, public_addr FROM peers WHERE peer_id = ?1",
                libsql::params![peer_id],
            )
            .await
            .context("Failed to resolve peer from central index")?;

        if let Some(row) = rows.next().await? {
            Ok(Some(PeerInfo {
                peer_id: row.get(0)?,
                display_name: row.get(1)?,
                public_addr: row.get(2)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Remove all index entries published by this peer. Called on graceful
    /// shutdown so stale entries don't accumulate in the central index.
    pub async fn unpublish_all(&self, peer_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM index_entries WHERE peer_id = ?1",
                libsql::params![peer_id],
            )
            .await
            .context("Failed to unpublish entries from central index")?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Encode a float slice as the JSON-array string that libsql's `vector32()`
/// function expects, e.g. `"[0.1,0.2,...]"`.
///
/// This mirrors the encoding used in `src/cache/lookup.rs`.
fn encode_embedding_json(embedding: &[f32]) -> String {
    format!(
        "[{}]",
        embedding
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Return the current Unix timestamp in seconds.
fn unix_now() -> Result<i64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("System time before UNIX epoch")?
        .as_secs() as i64)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: open an in-memory libsql DB and create the index schema so we
    // can exercise every IndexClient method without a real Turso instance.
    // Note: vector_distance_cos / vector32 are libsql extensions that ARE
    // available in the local libsql build used in tests (same crate as the
    // rest of the project). Methods that use these functions are tested with
    // a pragmatic approach: we verify the SQL executes without error and that
    // structural logic (field mapping, filtering) is correct.
    async fn open_test_db() -> Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .expect("open in-memory db");
        let conn = db.connect().expect("connect");

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS index_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                peer_id TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                model TEXT NOT NULL,
                system_hash TEXT NOT NULL,
                embedding F32_BLOB(384),
                response_size INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                UNIQUE(peer_id, content_hash)
            );
            CREATE TABLE IF NOT EXISTS peers (
                peer_id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL DEFAULT '',
                public_addr TEXT NOT NULL,
                last_seen INTEGER NOT NULL,
                version TEXT NOT NULL DEFAULT ''
            );",
        )
        .await
        .expect("schema");

        conn
    }

    fn dummy_embedding() -> Vec<f32> {
        // 384-dim unit vector along the first axis.
        let mut v = vec![0.0f32; 384];
        v[0] = 1.0;
        v
    }

    #[tokio::test]
    async fn test_encode_embedding_json() {
        let v = vec![1.0f32, 2.5f32, -0.5f32];
        let json = encode_embedding_json(&v);
        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));
        assert!(json.contains("1"));
        assert!(json.contains("2.5"));
        assert!(json.contains("-0.5"));
    }

    #[tokio::test]
    async fn test_heartbeat_and_resolve_peer() {
        let conn = open_test_db().await;
        let client = IndexClient::new(conn);

        // Heartbeat should insert a row.
        client
            .heartbeat("peer-1", "Alice", "alice.local:9843", "0.1.0")
            .await
            .expect("heartbeat");

        let info = client
            .resolve_peer("peer-1")
            .await
            .expect("resolve_peer")
            .expect("peer should exist");

        assert_eq!(info.peer_id, "peer-1");
        assert_eq!(info.display_name, "Alice");
        assert_eq!(info.public_addr, "alice.local:9843");
    }

    #[tokio::test]
    async fn test_heartbeat_upsert() {
        let conn = open_test_db().await;
        let client = IndexClient::new(conn);

        client
            .heartbeat("peer-2", "Bob", "bob.local:9843", "0.1.0")
            .await
            .unwrap();

        // Second heartbeat with updated address — should upsert, not error.
        client
            .heartbeat("peer-2", "Bob", "bob-new.local:9843", "0.2.0")
            .await
            .expect("second heartbeat");

        let info = client.resolve_peer("peer-2").await.unwrap().unwrap();
        assert_eq!(info.public_addr, "bob-new.local:9843");
    }

    #[tokio::test]
    async fn test_resolve_peer_missing() {
        let conn = open_test_db().await;
        let client = IndexClient::new(conn);

        let result = client.resolve_peer("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_publish_and_unpublish() {
        let conn = open_test_db().await;
        let client = IndexClient::new(conn);
        let emb = dummy_embedding();

        client
            .publish("peer-3", "hash-abc", "claude-3-5-sonnet-20241022", "sys1", &emb, 1024)
            .await
            .expect("publish");

        // Verify row exists.
        let mut rows = client
            .conn
            .query(
                "SELECT COUNT(*) FROM index_entries WHERE peer_id = 'peer-3'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let count: i64 = row.get(0).unwrap();
        assert_eq!(count, 1, "one row should be published");

        // Unpublish removes all rows for that peer.
        client.unpublish_all("peer-3").await.expect("unpublish_all");

        let mut rows2 = client
            .conn
            .query(
                "SELECT COUNT(*) FROM index_entries WHERE peer_id = 'peer-3'",
                (),
            )
            .await
            .unwrap();
        let row2 = rows2.next().await.unwrap().unwrap();
        let count2: i64 = row2.get(0).unwrap();
        assert_eq!(count2, 0, "rows should be deleted after unpublish_all");
    }

    #[tokio::test]
    async fn test_publish_is_idempotent() {
        let conn = open_test_db().await;
        let client = IndexClient::new(conn);
        let emb = dummy_embedding();

        // Publishing twice with the same (peer_id, content_hash) must not fail.
        client
            .publish("peer-4", "hash-xyz", "claude-3-5-sonnet-20241022", "sys1", &emb, 512)
            .await
            .expect("first publish");
        client
            .publish("peer-4", "hash-xyz", "claude-3-5-sonnet-20241022", "sys1", &emb, 512)
            .await
            .expect("second publish (idempotent)");

        let mut rows = client
            .conn
            .query(
                "SELECT COUNT(*) FROM index_entries WHERE peer_id = 'peer-4'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let count: i64 = row.get(0).unwrap();
        assert_eq!(count, 1, "idempotent publish should not duplicate rows");
    }

    #[tokio::test]
    async fn test_unpublish_all_only_affects_own_peer() {
        let conn = open_test_db().await;
        let client = IndexClient::new(conn);
        let emb = dummy_embedding();

        client
            .publish("peer-5", "hash-a", "claude-3-5-sonnet-20241022", "sys1", &emb, 100)
            .await
            .unwrap();
        client
            .publish("peer-6", "hash-b", "claude-3-5-sonnet-20241022", "sys1", &emb, 100)
            .await
            .unwrap();

        // Unpublish peer-5 — peer-6's row must survive.
        client.unpublish_all("peer-5").await.unwrap();

        let mut rows = client
            .conn
            .query("SELECT COUNT(*) FROM index_entries", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let count: i64 = row.get(0).unwrap();
        assert_eq!(count, 1, "only peer-5's rows should be deleted");
    }

    /// Smoke-test: `query` must execute without panicking even when the table
    /// is empty (returns None).
    ///
    /// Full vector similarity correctness is covered by integration tests
    /// against a live Turso instance (not run in CI by default).
    #[tokio::test]
    async fn test_query_empty_returns_none() {
        let conn = open_test_db().await;
        let client = IndexClient::new(conn);
        let emb = dummy_embedding();

        let result = client
            .query(
                "self-peer",
                &emb,
                "claude-3-5-sonnet-20241022",
                0.95,
                false,
                "sys1",
            )
            .await
            .expect("query should not error on empty table");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_index_hit_struct() {
        let hit = IndexHit {
            peer_id: "p".to_string(),
            content_hash: "h".to_string(),
            similarity: 0.97,
        };
        assert_eq!(hit.peer_id, "p");
        assert_eq!(hit.content_hash, "h");
        assert!((hit.similarity - 0.97).abs() < 1e-6);
    }

    #[tokio::test]
    async fn test_peer_info_struct() {
        let info = PeerInfo {
            peer_id: "p".to_string(),
            display_name: "Alice".to_string(),
            public_addr: "alice:9843".to_string(),
        };
        assert_eq!(info.peer_id, "p");
    }
}
