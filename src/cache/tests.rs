use super::*;

/// Helper: create a dummy 384-dim embedding with a specific pattern
fn make_embedding(seed: f32) -> Vec<f32> {
    let mut emb = vec![0.0f32; 384];
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
        .store("test prompt", "sys123", "gpt-4", &emb, data)
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
        .lookup(&emb, "sys123", "gpt-4", 0.95)
        .await
        .expect("lookup failed");
    assert!(hit.is_none());
}

#[tokio::test]
async fn test_store_and_lookup_hit() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);
    let data = b"cached response";

    db.store("hello world", "syshash", "gpt-4", &emb, data)
        .await
        .expect("store failed");

    // Same embedding should be a perfect match
    let hit = db
        .lookup(&emb, "syshash", "gpt-4", 0.95)
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

    db.store("prompt", "syshash", "gpt-4", &emb, b"data")
        .await
        .unwrap();

    // Different model should not match
    let hit = db
        .lookup(&emb, "syshash", "claude-3", 0.95)
        .await
        .expect("lookup failed");
    assert!(hit.is_none());
}

#[tokio::test]
async fn test_lookup_wrong_system_hash_no_hit() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);

    db.store("prompt", "hash_a", "gpt-4", &emb, b"data")
        .await
        .unwrap();

    // Different system_hash should not match
    let hit = db
        .lookup(&emb, "hash_b", "gpt-4", 0.95)
        .await
        .expect("lookup failed");
    assert!(hit.is_none());
}

#[tokio::test]
async fn test_evict_by_ttl() {
    let db = CacheDb::open_in_memory().await.expect("open failed");
    let emb = make_embedding(1.0);

    db.store("old prompt", "sys", "gpt-4", &emb, b"old data")
        .await
        .unwrap();

    // Manually set created_at to the past
    db.conn()
        .execute("UPDATE cache SET created_at = 1000", ())
        .await
        .unwrap();

    // Store a fresh entry
    let emb2 = make_embedding(2.0);
    db.store("new prompt", "sys", "gpt-4", &emb2, b"new data")
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
    db.store("prompt", "sys", "gpt-4", &emb, b"data")
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
        db.store("prompt", "sys", "gpt-4", &emb, b"data")
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
    db.store("p1", "sys", "gpt-4", &emb1, b"aaaa").await.unwrap();
    db.store("p2", "sys", "gpt-4", &emb2, b"bbbbbb").await.unwrap();

    assert_eq!(db.entry_count().await.unwrap(), 2);
    assert_eq!(db.total_size().await.unwrap(), 10); // 4 + 6
    assert_eq!(db.total_hits().await.unwrap(), 0);

    // Trigger a hit
    db.lookup(&emb1, "sys", "gpt-4", 0.95).await.unwrap();
    assert_eq!(db.total_hits().await.unwrap(), 1);
}
