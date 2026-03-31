use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};

/// Outcome of trying to register an in-flight request.
pub enum InflightStatus {
    /// First request with this key — proceed to upstream.
    Leader,
    /// Another request is already in-flight — wait on the Notify.
    Waiter(Arc<Notify>),
}

/// Tracks in-flight upstream requests to enable request coalescing.
pub struct InflightMap {
    map: Mutex<HashMap<String, (Arc<Notify>, Instant)>>,
}

impl InflightMap {
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Compute the coalescing key from model + user message.
    pub fn coalesce_key(model: &str, user_message: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(model.as_bytes());
        hasher.update(b":");
        hasher.update(user_message.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Try to register as the leader for this key.
    /// Returns `Leader` if first request, `Waiter(notify)` if already in-flight.
    pub async fn try_register(&self, key: &str) -> InflightStatus {
        let mut map = self.map.lock().await;
        if let Some((notify, _)) = map.get(key) {
            InflightStatus::Waiter(Arc::clone(notify))
        } else {
            let notify = Arc::new(Notify::new());
            map.insert(key.to_string(), (notify, Instant::now()));
            InflightStatus::Leader
        }
    }

    /// Mark a key as complete, waking all waiters and removing the entry.
    pub async fn complete(&self, key: &str) {
        let mut map = self.map.lock().await;
        if let Some((notify, _)) = map.remove(key) {
            notify.notify_waiters();
        }
    }

    /// Remove entries older than `max_age` and wake their waiters.
    pub async fn cleanup_stale(&self, max_age: Duration) {
        let mut map = self.map.lock().await;
        let now = Instant::now();
        map.retain(|_, (notify, created)| {
            if now.duration_since(*created) > max_age {
                notify.notify_waiters();
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coalesce_key_deterministic() {
        let k1 = InflightMap::coalesce_key("claude-sonnet-4-20250514", "hello world");
        let k2 = InflightMap::coalesce_key("claude-sonnet-4-20250514", "hello world");
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_coalesce_key_differs_by_model() {
        let k1 = InflightMap::coalesce_key("claude-sonnet-4-20250514", "hello");
        let k2 = InflightMap::coalesce_key("claude-opus-4-20250514", "hello");
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_coalesce_key_differs_by_message() {
        let k1 = InflightMap::coalesce_key("claude-sonnet-4-20250514", "hello");
        let k2 = InflightMap::coalesce_key("claude-sonnet-4-20250514", "goodbye");
        assert_ne!(k1, k2);
    }

    #[tokio::test]
    async fn test_first_register_is_leader() {
        let map = InflightMap::new();
        match map.try_register("key1").await {
            InflightStatus::Leader => {}
            InflightStatus::Waiter(_) => panic!("expected Leader"),
        }
    }

    #[tokio::test]
    async fn test_second_register_is_waiter() {
        let map = InflightMap::new();
        let _ = map.try_register("key1").await;
        match map.try_register("key1").await {
            InflightStatus::Waiter(_) => {}
            InflightStatus::Leader => panic!("expected Waiter"),
        }
    }

    #[tokio::test]
    async fn test_complete_wakes_waiters() {
        let map = Arc::new(InflightMap::new());
        let _ = map.try_register("key1").await;

        let notify = match map.try_register("key1").await {
            InflightStatus::Waiter(n) => n,
            _ => panic!("expected Waiter"),
        };

        // Enable the future before spawning so notified() is already polled
        let fut = notify.notified();
        tokio::pin!(fut);

        let map_clone = Arc::clone(&map);
        let handle = tokio::spawn(async move {
            // Small yield to let the main task register the notified future
            tokio::task::yield_now().await;
            map_clone.complete("key1").await;
        });

        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("waiter should be notified");
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_complete_allows_new_leader() {
        let map = InflightMap::new();
        let _ = map.try_register("key1").await;
        map.complete("key1").await;
        match map.try_register("key1").await {
            InflightStatus::Leader => {}
            InflightStatus::Waiter(_) => panic!("expected Leader after complete"),
        }
    }

    #[tokio::test]
    async fn test_different_keys_independent() {
        let map = InflightMap::new();
        match map.try_register("key1").await {
            InflightStatus::Leader => {}
            _ => panic!("expected Leader"),
        }
        match map.try_register("key2").await {
            InflightStatus::Leader => {}
            _ => panic!("expected Leader for different key"),
        }
    }

    #[tokio::test]
    async fn test_cleanup_stale_removes_old_entries() {
        let map = InflightMap::new();
        let _ = map.try_register("key1").await;
        // Cleanup with zero max_age removes everything
        map.cleanup_stale(Duration::from_secs(0)).await;
        match map.try_register("key1").await {
            InflightStatus::Leader => {}
            InflightStatus::Waiter(_) => panic!("stale entry should have been cleaned"),
        }
    }

    #[tokio::test]
    async fn test_cleanup_stale_keeps_fresh_entries() {
        let map = InflightMap::new();
        let _ = map.try_register("key1").await;
        map.cleanup_stale(Duration::from_secs(60)).await;
        match map.try_register("key1").await {
            InflightStatus::Waiter(_) => {}
            InflightStatus::Leader => panic!("fresh entry should not be cleaned"),
        }
    }
}
