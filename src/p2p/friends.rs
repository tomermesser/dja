use anyhow::{Context, Result};
use libsql::Connection;
use tokio::sync::MutexGuard;

use crate::cache::CacheDb;

/// A record from the `friends` table.
#[derive(Debug, Clone, PartialEq)]
pub struct FriendRecord {
    pub peer_id: String,
    pub display_name: String,
    pub public_addr: String,
    pub added_at: i64,
    /// One of: "active" | "pending_sent" | "pending_received"
    pub status: String,
}

impl CacheDb {
    /// Add a new friend (or replace an existing record with the same peer_id).
    pub async fn add_friend(
        &self,
        peer_id: &str,
        display_name: &str,
        public_addr: &str,
        status: &str,
    ) -> Result<()> {
        let added_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("System time before UNIX epoch")?
            .as_secs() as i64;

        let conn: MutexGuard<'_, Connection> = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO friends (peer_id, display_name, public_addr, added_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![peer_id, display_name, public_addr, added_at, status],
        )
        .await
        .context("Failed to insert friend")?;
        Ok(())
    }

    /// Remove a friend by peer_id.
    pub async fn remove_friend(&self, peer_id: &str) -> Result<()> {
        let conn: MutexGuard<'_, Connection> = self.conn.lock().await;
        conn.execute(
            "DELETE FROM friends WHERE peer_id = ?1",
            libsql::params![peer_id],
        )
        .await
        .context("Failed to remove friend")?;
        Ok(())
    }

    /// List all friends.
    pub async fn list_friends(&self) -> Result<Vec<FriendRecord>> {
        let conn: MutexGuard<'_, Connection> = self.conn.lock().await;
        let mut rows = conn
            .query("SELECT peer_id, display_name, public_addr, added_at, status FROM friends ORDER BY added_at", ())
            .await
            .context("Failed to list friends")?;

        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            result.push(FriendRecord {
                peer_id: row.get(0)?,
                display_name: row.get(1)?,
                public_addr: row.get(2)?,
                added_at: row.get(3)?,
                status: row.get(4)?,
            });
        }
        Ok(result)
    }

    /// Get a single friend by peer_id.
    pub async fn get_friend(&self, peer_id: &str) -> Result<Option<FriendRecord>> {
        let conn: MutexGuard<'_, Connection> = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT peer_id, display_name, public_addr, added_at, status FROM friends WHERE peer_id = ?1",
                libsql::params![peer_id],
            )
            .await
            .context("Failed to get friend")?;

        if let Some(row) = rows.next().await? {
            Ok(Some(FriendRecord {
                peer_id: row.get(0)?,
                display_name: row.get(1)?,
                public_addr: row.get(2)?,
                added_at: row.get(3)?,
                status: row.get(4)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Update the status of a friend.
    pub async fn update_friend_status(&self, peer_id: &str, status: &str) -> Result<()> {
        let conn: MutexGuard<'_, Connection> = self.conn.lock().await;
        conn.execute(
            "UPDATE friends SET status = ?1 WHERE peer_id = ?2",
            libsql::params![status, peer_id],
        )
        .await
        .context("Failed to update friend status")?;
        Ok(())
    }

    /// Check whether a peer_id exists in the friends table.
    pub async fn is_friend(&self, peer_id: &str) -> Result<bool> {
        let conn: MutexGuard<'_, Connection> = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT 1 FROM friends WHERE peer_id = ?1 LIMIT 1",
                libsql::params![peer_id],
            )
            .await
            .context("Failed to check friend")?;
        Ok(rows.next().await?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_and_list_friends() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        db.add_friend("peer-1", "Alice", "alice.tailnet:9843", "active")
            .await
            .expect("add_friend failed");

        let friends = db.list_friends().await.expect("list_friends failed");
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].peer_id, "peer-1");
        assert_eq!(friends[0].display_name, "Alice");
        assert_eq!(friends[0].public_addr, "alice.tailnet:9843");
        assert_eq!(friends[0].status, "active");
    }

    #[tokio::test]
    async fn test_get_friend() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        db.add_friend("peer-2", "Bob", "bob.tailnet:9843", "pending_sent")
            .await
            .expect("add_friend failed");

        let friend = db.get_friend("peer-2").await.expect("get_friend failed");
        assert!(friend.is_some());
        let f = friend.unwrap();
        assert_eq!(f.peer_id, "peer-2");
        assert_eq!(f.status, "pending_sent");

        let missing = db.get_friend("nonexistent").await.expect("get_friend failed");
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_remove_friend() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        db.add_friend("peer-3", "Carol", "carol.tailnet:9843", "active")
            .await
            .unwrap();
        assert!(db.is_friend("peer-3").await.unwrap());

        db.remove_friend("peer-3").await.expect("remove_friend failed");
        assert!(!db.is_friend("peer-3").await.unwrap());

        let friends = db.list_friends().await.unwrap();
        assert!(friends.is_empty());
    }

    #[tokio::test]
    async fn test_update_friend_status() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        db.add_friend("peer-4", "Dave", "dave.tailnet:9843", "pending_received")
            .await
            .unwrap();

        db.update_friend_status("peer-4", "active")
            .await
            .expect("update_friend_status failed");

        let friend = db.get_friend("peer-4").await.unwrap().unwrap();
        assert_eq!(friend.status, "active");
    }

    #[tokio::test]
    async fn test_is_friend() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        assert!(!db.is_friend("peer-5").await.unwrap());

        db.add_friend("peer-5", "Eve", "eve.tailnet:9843", "active")
            .await
            .unwrap();

        assert!(db.is_friend("peer-5").await.unwrap());
    }

    #[tokio::test]
    async fn test_list_friends_empty() {
        let db = CacheDb::open_in_memory().await.expect("open failed");
        let friends = db.list_friends().await.expect("list_friends failed");
        assert!(friends.is_empty());
    }

    #[tokio::test]
    async fn test_add_friend_replace() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        db.add_friend("peer-6", "Frank", "frank.tailnet:9843", "pending_sent")
            .await
            .unwrap();

        // Re-add with same peer_id but different values — should replace
        db.add_friend("peer-6", "Frank Updated", "frank-new.tailnet:9843", "active")
            .await
            .unwrap();

        let friends = db.list_friends().await.unwrap();
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].display_name, "Frank Updated");
        assert_eq!(friends[0].status, "active");
    }
}
