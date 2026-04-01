use sha2::{Digest, Sha256};

use super::*;
use db::EMBEDDING_DIM;

/// Helper: create a dummy 384-dim embedding with a specific pattern
fn make_embedding(seed: f32) -> Vec<f32> {
    let mut emb = vec![0.0f32; EMBEDDING_DIM];
    for (i, v) in emb.iter_mut().enumerate() {
        *v = ((i as f32 + seed) * 0.01).sin();
    }
    // Normalize to unit vector for cosine similarity
    let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut emb {
            *v /= norm;
        }
    }
    emb
}

#[tokio::test]
async fn test_schema_creation() {
    let db = CacheDb::open_in_memory().await.expect("Failed to open in-memory DB");
    // Verify the cache table exists by querying it
    let count = db.entry_count().await.expect("entry_count failed");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_store_and_stats() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);
    let data = b"response payload";

    let id = db
        .store("test prompt", "sys123", "gpt-4", &emb, data, "local")
        .await
        .expect("store failed");
    assert!(id > 0);

    assert_eq!(db.entry_count().await.unwrap(), 1);
    assert_eq!(db.total_size().await.unwrap(), data.len() as u64);
    assert_eq!(db.total_hits().await.unwrap(), 0);
}

#[tokio::test]
async fn test_lookup_no_match() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);

    // Empty database should return None
    let hit = db
        .lookup(&emb, "sys123", "gpt-4", 0.95, true)
        .await
        .expect("lookup failed");
    assert!(hit.is_none());
}

#[tokio::test]
async fn test_store_and_lookup_hit() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);
    let data = b"cached response";

    db.store("hello world", "syshash", "gpt-4", &emb, data, "local")
        .await
        .expect("store failed");

    // Same embedding should be a perfect match
    let hit = db
        .lookup(&emb, "syshash", "gpt-4", 0.95, true)
        .await
        .expect("lookup failed");
    let hit = hit.expect("Expected a cache hit");
    assert_eq!(hit.prompt_text, "hello world");
    assert_eq!(hit.response_data, data);
    assert!(hit.similarity > 0.99, "similarity={}", hit.similarity);

    // Verify hit_count was updated
    assert_eq!(db.total_hits().await.unwrap(), 1);
}

#[tokio::test]
async fn test_lookup_wrong_model_no_hit() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);

    db.store("prompt", "syshash", "gpt-4", &emb, b"data", "local")
        .await
        .unwrap();

    // Different model should not match
    let hit = db
        .lookup(&emb, "syshash", "claude-3", 0.95, true)
        .await
        .expect("lookup failed");
    assert!(hit.is_none());
}

#[tokio::test]
async fn test_lookup_wrong_system_hash_no_hit() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);

    db.store("prompt", "hash_a", "gpt-4", &emb, b"data", "local")
        .await
        .unwrap();

    // Different system_hash should not match
    let hit = db
        .lookup(&emb, "hash_b", "gpt-4", 0.95, true)
        .await
        .expect("lookup failed");
    assert!(hit.is_none());
}

#[tokio::test]
async fn test_evict_by_ttl() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);

    db.store("old prompt", "sys", "gpt-4", &emb, b"old data", "local")
        .await
        .unwrap();

    // Manually set created_at to the past
    db.conn
        .lock()
        .await
        .execute("UPDATE cache SET created_at = 1000", ())
        .await
        .unwrap();

    // Store a fresh entry
    let emb2 = make_embedding(2.0);
    db.store("new prompt", "sys", "gpt-4", &emb2, b"new data", "local")
        .await
        .unwrap();

    assert_eq!(db.entry_count().await.unwrap(), 2);

    // Evict entries older than 1 hour -- the old entry has created_at=1000
    let deleted = db.evict_by_ttl(3600).await.expect("evict_by_ttl failed");
    assert_eq!(deleted, 1);
    assert_eq!(db.entry_count().await.unwrap(), 1);
}

#[tokio::test]
async fn test_evict_lru() {
    let db = CacheDb::open_in_memory().await.expect("open failed");

    for i in 0..5 {
        let emb = make_embedding(i as f32 * 10.0);
        db.store(
            &format!("prompt {i}"),
            "sys",
            "gpt-4",
            &emb,
            format!("data {i}").as_bytes(),
            "local",
        )
        .await
        .unwrap();
    }

    assert_eq!(db.entry_count().await.unwrap(), 5);

    // Keep only 3 entries
    let deleted = db.evict_lru(3).await.expect("evict_lru failed");
    assert_eq!(deleted, 2);
    assert_eq!(db.entry_count().await.unwrap(), 3);
}

#[tokio::test]
async fn test_evict_lru_no_op_when_under_limit() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);
    db.store("prompt", "sys", "gpt-4", &emb, b"data", "local")
        .await
        .unwrap();

    let deleted = db.evict_lru(10).await.expect("evict_lru failed");
    assert_eq!(deleted, 0);
    assert_eq!(db.entry_count().await.unwrap(), 1);
}

#[tokio::test]
async fn test_clear_all() {
    let db = CacheDb::open_in_memory().await.expect("open failed");

    for i in 0..3 {
        let emb = make_embedding(i as f32 * 10.0);
        db.store("prompt", "sys", "gpt-4", &emb, b"data", "local")
            .await
            .unwrap();
    }

    assert_eq!(db.entry_count().await.unwrap(), 3);

    let deleted = db.clear_all().await.expect("clear_all failed");
    assert_eq!(deleted, 3);
    assert_eq!(db.entry_count().await.unwrap(), 0);
}

#[tokio::test]
async fn test_stats_functions() {
    let db = CacheDb::open_in_memory().await.expect("open failed");

    // Empty database
    assert_eq!(db.entry_count().await.unwrap(), 0);
    assert_eq!(db.total_size().await.unwrap(), 0);
    assert_eq!(db.total_hits().await.unwrap(), 0);

    // Add entries
    let emb1 = make_embedding(1.0);
    let emb2 = make_embedding(2.0);
    db.store("p1", "sys", "gpt-4", &emb1, b"aaaa", "local").await.unwrap();
    db.store("p2", "sys", "gpt-4", &emb2, b"bbbbbb", "local").await.unwrap();

    assert_eq!(db.entry_count().await.unwrap(), 2);
    assert_eq!(db.total_size().await.unwrap(), 10); // 4 + 6
    assert_eq!(db.total_hits().await.unwrap(), 0);

    // Trigger a hit
    db.lookup(&emb1, "sys", "gpt-4", 0.95, true).await.unwrap();
    assert_eq!(db.total_hits().await.unwrap(), 1);
}

#[tokio::test]
async fn test_lookup_ignores_system_hash_when_disabled() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);

    db.store("prompt", "hash_a", "gpt-4", &emb, b"cached data", "local")
        .await
        .unwrap();

    // Different system_hash, but match_system_prompt = false → should still hit
    let hit = db
        .lookup(&emb, "hash_b", "gpt-4", 0.95, false)
        .await
        .expect("lookup failed");
    assert!(hit.is_some(), "should hit when match_system_prompt is false");
    assert_eq!(hit.unwrap().prompt_text, "prompt");
}

#[tokio::test]
async fn test_lookup_respects_system_hash_when_enabled() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);

    db.store("prompt", "hash_a", "gpt-4", &emb, b"cached data", "local")
        .await
        .unwrap();

    // Different system_hash, match_system_prompt = true → should miss
    let hit = db
        .lookup(&emb, "hash_b", "gpt-4", 0.95, true)
        .await
        .expect("lookup failed");
    assert!(hit.is_none(), "should miss when match_system_prompt is true");

    // Same system_hash, match_system_prompt = true → should hit
    let hit = db
        .lookup(&emb, "hash_a", "gpt-4", 0.95, true)
        .await
        .expect("lookup failed");
    assert!(hit.is_some(), "should hit with matching system_hash");
}

#[tokio::test]
async fn test_store_computes_content_hash() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);
    let data = b"hello content hash";

    let id = db
        .store("prompt", "sys", "gpt-4", &emb, data, "local")
        .await
        .expect("store failed");
    assert!(id > 0);

    // Compute expected SHA256
    let mut hasher = Sha256::new();
    hasher.update(data);
    let expected = hex::encode(hasher.finalize());

    // Query the stored content_hash directly
    let conn = db.conn.lock().await;
    let mut rows = conn
        .query("SELECT content_hash FROM cache WHERE id = ?1", libsql::params![id])
        .await
        .expect("query failed");
    let row = rows.next().await.expect("query error").expect("no row");
    let stored_hash: String = row.get(0).expect("get failed");

    assert_eq!(stored_hash, expected, "content_hash mismatch");
    assert_eq!(stored_hash.len(), 64, "SHA256 hex should be 64 chars");
}

#[tokio::test]
async fn test_lookup_by_content_hash_end_to_end_integrity() {
    // Stores a response with known content, retrieves it via lookup_by_content_hash,
    // then verifies that SHA-256 of the returned bytes matches the stored hash.
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);
    let data = b"known response payload for integrity check";

    db.store("integrity prompt", "sys", "gpt-4", &emb, data, "local")
        .await
        .expect("store failed");

    // Compute expected hash
    let mut hasher = Sha256::new();
    hasher.update(data);
    let expected_hash = hex::encode(hasher.finalize());

    // Retrieve via lookup_by_content_hash
    let retrieved = db
        .lookup_by_content_hash(&expected_hash)
        .await
        .expect("lookup_by_content_hash failed");
    let retrieved = retrieved.expect("expected Some bytes from lookup_by_content_hash");

    // Verify SHA-256 of retrieved bytes matches the stored hash
    let mut verify_hasher = Sha256::new();
    verify_hasher.update(&retrieved);
    let actual_hash = hex::encode(verify_hasher.finalize());

    assert_eq!(
        actual_hash, expected_hash,
        "SHA-256 of retrieved bytes must equal the stored content_hash"
    );
    assert_eq!(retrieved, data, "retrieved bytes must equal original data");
}

#[tokio::test]
async fn test_lookup_by_content_hash_not_found() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let result = db
        .lookup_by_content_hash("0000000000000000000000000000000000000000000000000000000000000000")
        .await
        .expect("lookup_by_content_hash failed");
    assert!(result.is_none());
}
