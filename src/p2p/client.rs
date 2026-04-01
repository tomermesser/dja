use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// Response from a peer's `/p2p/ping` endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PingResponse {
    pub peer_id: String,
    pub display_name: String,
    pub version: String,
}

/// Invite/accept request body.
#[derive(Serialize)]
struct InvitePayload<'a> {
    peer_id: &'a str,
    display_name: &'a str,
    public_addr: &'a str,
}

/// Thin HTTP client for communicating with peer nodes.
pub struct PeerClient {
    client: reqwest::Client,
}

impl PeerClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .build()
                .expect("Failed to build reqwest client"),
        }
    }

    /// Fetch a cached response from a peer by content hash.
    ///
    /// Sends `GET http://{peer_addr}/p2p/fetch?content_hash={hash}` with a 5 s
    /// timeout and verifies that the SHA-256 of the returned bytes matches the
    /// requested hash.
    pub async fn fetch_response(
        &self,
        peer_addr: &str,
        content_hash: &str,
        self_peer_id: &str,
    ) -> Result<Vec<u8>> {
        let url = format!("http://{peer_addr}/p2p/fetch?content_hash={content_hash}");
        let resp = self
            .client
            .get(&url)
            .header("X-Peer-Id", self_peer_id)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .with_context(|| format!("fetch_response: request to {url} failed"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!(
                "fetch_response: peer returned status {status} for hash {content_hash}"
            ));
        }

        let data = resp
            .bytes()
            .await
            .with_context(|| "fetch_response: failed to read response body")?
            .to_vec();

        // Verify SHA-256 of returned data matches the requested content_hash.
        let digest = Sha256::digest(&data);
        let actual_hex = hex::encode(digest);
        if actual_hex != content_hash {
            return Err(anyhow!(
                "fetch_response: hash mismatch — expected {content_hash}, got {actual_hex}"
            ));
        }

        Ok(data)
    }

    /// Ping a peer to retrieve its identity.
    ///
    /// Sends `GET http://{peer_addr}/p2p/ping` with a 2 s timeout.
    pub async fn ping(&self, peer_addr: &str) -> Result<PingResponse> {
        let url = format!("http://{peer_addr}/p2p/ping");
        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .with_context(|| format!("ping: request to {url} failed"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("ping: peer returned status {status}"));
        }

        let ping_resp: PingResponse = resp
            .json()
            .await
            .with_context(|| "ping: failed to parse response JSON")?;
        Ok(ping_resp)
    }

    /// Send a friendship invite to a peer.
    ///
    /// Sends `POST http://{peer_addr}/p2p/invite` with this peer's identity.
    pub async fn send_invite(
        &self,
        peer_addr: &str,
        self_peer_id: &str,
        display_name: &str,
        public_addr: &str,
    ) -> Result<()> {
        let url = format!("http://{peer_addr}/p2p/invite");
        let payload = InvitePayload {
            peer_id: self_peer_id,
            display_name,
            public_addr,
        };

        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .with_context(|| format!("send_invite: request to {url} failed"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("send_invite: peer returned status {status}"));
        }

        Ok(())
    }

    /// Tell a peer that we accept their invite.
    ///
    /// Sends `POST http://{peer_addr}/p2p/invite/accept` with this peer's identity.
    pub async fn accept_invite(
        &self,
        peer_addr: &str,
        self_peer_id: &str,
        display_name: &str,
        public_addr: &str,
    ) -> Result<()> {
        let url = format!("http://{peer_addr}/p2p/invite/accept");
        let payload = InvitePayload {
            peer_id: self_peer_id,
            display_name,
            public_addr,
        };

        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .with_context(|| format!("accept_invite: request to {url} failed"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("accept_invite: peer returned status {status}"));
        }

        Ok(())
    }
}

impl Default for PeerClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheDb;
    use crate::config::P2pConfig;
    use crate::p2p::server::{build_peer_router, PeerServerState};
    use std::sync::Arc;
    use tokio::net::TcpListener;

    async fn start_test_peer(peer_id: &str, display_name: &str) -> (Arc<CacheDb>, String) {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());
        let config = Arc::new(P2pConfig {
            enabled: true,
            peer_id: peer_id.to_string(),
            display_name: display_name.to_string(),
            listen_port: 0,
            ..Default::default()
        });

        let state = PeerServerState {
            db: Arc::clone(&db),
            config,
        };

        let app = build_peer_router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let addr = format!("127.0.0.1:{port}");
        (db, addr)
    }

    #[tokio::test]
    async fn test_client_ping() {
        let (_db, addr) = start_test_peer("peer-abc", "AlphaNode").await;
        let client = PeerClient::new();
        let resp = client.ping(&addr).await.unwrap();
        assert_eq!(resp.peer_id, "peer-abc");
        assert_eq!(resp.display_name, "AlphaNode");
    }

    #[tokio::test]
    async fn test_client_send_invite() {
        let (db, addr) = start_test_peer("server-peer", "Server").await;
        let client = PeerClient::new();
        client
            .send_invite(&addr, "caller-peer", "Caller", "caller:9843")
            .await
            .unwrap();
        let friend = db.get_friend("caller-peer").await.unwrap();
        assert!(friend.is_some());
        assert_eq!(friend.unwrap().status, crate::p2p::FriendStatus::PendingReceived);
    }

    #[tokio::test]
    async fn test_client_accept_invite() {
        let (db, addr) = start_test_peer("server-peer", "Server").await;

        // Pre-add as pending_sent so we can upgrade
        db.add_friend("caller-peer", "Caller", "caller:9843", crate::p2p::FriendStatus::PendingSent)
            .await
            .unwrap();

        let client = PeerClient::new();
        client
            .accept_invite(&addr, "caller-peer", "Caller", "caller:9843")
            .await
            .unwrap();

        let friend = db.get_friend("caller-peer").await.unwrap().unwrap();
        assert_eq!(friend.status, crate::p2p::FriendStatus::Active);
    }
}
