use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::cache::CacheDb;
use crate::config::P2pConfig;
use crate::p2p::FriendStatus;

/// Shared state for the peer HTTP server.
#[derive(Clone)]
pub struct PeerServerState {
    pub db: Arc<CacheDb>,
    pub config: Arc<P2pConfig>,
}

// ── Request/response types ─────────────────────────────────────────────────

#[derive(Serialize)]
struct PingResponseBody {
    peer_id: String,
    display_name: String,
    version: String,
}

#[derive(Deserialize)]
struct FetchQuery {
    content_hash: String,
}

#[derive(Deserialize, Serialize)]
pub struct InviteBody {
    pub peer_id: String,
    pub display_name: String,
    pub public_addr: String,
}

// ── Handlers ───────────────────────────────────────────────────────────────

/// GET /p2p/ping — returns peer identity, no auth required.
async fn ping_handler(State(state): State<PeerServerState>) -> impl IntoResponse {
    Json(PingResponseBody {
        peer_id: state.config.peer_id.clone(),
        display_name: state.config.display_name.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// GET /p2p/fetch?content_hash=<hash>
/// Requires the caller to send `X-Peer-Id` header that is in the friends list.
async fn fetch_handler(
    State(state): State<PeerServerState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<FetchQuery>,
) -> Response {
    // Auth: X-Peer-Id must be in friends table
    let peer_id = match headers.get("X-Peer-Id").and_then(|v| v.to_str().ok()) {
        Some(id) => id.to_string(),
        None => {
            return (StatusCode::FORBIDDEN, "Missing X-Peer-Id header").into_response();
        }
    };

    match state.db.is_active_friend(&peer_id).await {
        Ok(true) => {}
        Ok(false) => {
            return (StatusCode::FORBIDDEN, "Peer not in friends list").into_response();
        }
        Err(e) => {
            tracing::error!("DB error during is_active_friend check: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "DB error").into_response();
        }
    }

    // Retrieve the cached response data
    match state.db.lookup_by_content_hash(&query.content_hash).await {
        Ok(Some(data)) => (StatusCode::OK, data).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Cache entry not found").into_response(),
        Err(e) => {
            tracing::error!("DB error during lookup_by_content_hash: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "DB error").into_response()
        }
    }
}

/// POST /p2p/invite — receive an invite from a remote peer; add as pending_received.
async fn invite_handler(
    State(state): State<PeerServerState>,
    Json(body): Json<InviteBody>,
) -> Response {
    match state
        .db
        .add_friend(
            &body.peer_id,
            &body.display_name,
            &body.public_addr,
            FriendStatus::PendingReceived,
        )
        .await
    {
        Ok(()) => (StatusCode::OK, "invite received").into_response(),
        Err(e) => {
            tracing::error!("Failed to add pending friend: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "DB error").into_response()
        }
    }
}

/// POST /p2p/invite/accept — the remote peer accepted our sent invite.
/// Upgrades `pending_sent` → `active`, or adds as `active` if not present.
async fn invite_accept_handler(
    State(state): State<PeerServerState>,
    Json(body): Json<InviteBody>,
) -> Response {
    // Try to upgrade an existing pending_sent entry first
    let upgraded = match state.db.update_friend_status(&body.peer_id, FriendStatus::Active).await {
        Ok(updated) => updated,
        Err(e) => {
            tracing::error!("Failed to update friend status: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "DB error").into_response();
        }
    };

    if !upgraded {
        // Not found — add as active directly
        if let Err(e) = state
            .db
            .add_friend(
                &body.peer_id,
                &body.display_name,
                &body.public_addr,
                FriendStatus::Active,
            )
            .await
        {
            tracing::error!("Failed to add friend as active: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "DB error").into_response();
        }
    }

    (StatusCode::OK, "invite accepted").into_response()
}

// ── Router builder (shared between production and tests) ──────────────────

/// Build the Axum router for the peer HTTP server.
pub fn build_peer_router(state: PeerServerState) -> Router {
    Router::new()
        .route("/p2p/ping", get(ping_handler))
        .route("/p2p/fetch", get(fetch_handler))
        .route("/p2p/invite", post(invite_handler))
        .route("/p2p/invite/accept", post(invite_accept_handler))
        .with_state(state)
}

// ── Server startup ─────────────────────────────────────────────────────────

/// Start the peer HTTP server and run until the process exits.
///
/// Binds to `127.0.0.1` by default. External reachability is expected to be
/// provided by a tunnel (Tailscale, Cloudflare Tunnel, etc.) as documented in
/// the P2P design spec — peers are not required to be directly internet-routable.
pub async fn start_peer_server(state: PeerServerState) -> Result<()> {
    let port = state.config.listen_port;
    let addr = format!("127.0.0.1:{port}");

    let app = build_peer_router(state);

    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("P2P peer server listening on {addr}");

    axum::serve(listener, app).await?;
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheDb;

    /// Helper: spin up a peer server on a random port and return the base URL.
    async fn start_test_server(db: Arc<CacheDb>, config: Arc<P2pConfig>) -> String {
        let state = PeerServerState {
            db: Arc::clone(&db),
            config: Arc::clone(&config),
        };

        let app = build_peer_router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        format!("http://127.0.0.1:{port}")
    }

    fn test_config(peer_id: &str, display_name: &str) -> Arc<P2pConfig> {
        Arc::new(P2pConfig {
            enabled: true,
            peer_id: peer_id.to_string(),
            display_name: display_name.to_string(),
            listen_port: 0,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn test_ping_returns_identity() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());
        let config = test_config("test-peer-id", "TestNode");
        let base = start_test_server(db, config).await;

        let resp = reqwest::get(format!("{base}/p2p/ping")).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["peer_id"], "test-peer-id");
        assert_eq!(body["display_name"], "TestNode");
        assert!(body["version"].is_string());
    }

    #[tokio::test]
    async fn test_fetch_forbidden_without_peer_id_header() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());
        let config = test_config("server-peer", "Server");
        let base = start_test_server(db, config).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/p2p/fetch?content_hash=abc123"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_fetch_forbidden_for_non_friend() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());
        let config = test_config("server-peer", "Server");
        let base = start_test_server(db, config).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/p2p/fetch?content_hash=abc123"))
            .header("X-Peer-Id", "unknown-peer")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn test_fetch_forbidden_for_pending_received_friend() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());

        // Add friend with pending_received status — must NOT be allowed to fetch
        db.add_friend("pending-peer", "PendingNode", "pending:9843", FriendStatus::PendingReceived)
            .await
            .unwrap();

        let config = test_config("server-peer", "Server");
        let base = start_test_server(db, config).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/p2p/fetch?content_hash=abc123"))
            .header("X-Peer-Id", "pending-peer")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403, "pending_received peer must be forbidden");
    }

    #[tokio::test]
    async fn test_fetch_forbidden_for_pending_sent_friend() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());

        db.add_friend("sent-peer", "SentNode", "sent:9843", FriendStatus::PendingSent)
            .await
            .unwrap();

        let config = test_config("server-peer", "Server");
        let base = start_test_server(db, config).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/p2p/fetch?content_hash=abc123"))
            .header("X-Peer-Id", "sent-peer")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403, "pending_sent peer must be forbidden");
    }

    #[tokio::test]
    async fn test_fetch_returns_data_for_friend() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());

        // Insert a cache entry with a known content_hash
        let content_hash = "deadbeefdeadbeef";
        let response_data = b"hello cached world".to_vec();
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO cache (prompt_text, system_hash, model, response_data, response_size, created_at, content_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                libsql::params![
                    "test prompt",
                    "sys_hash",
                    "claude-3",
                    response_data.clone(),
                    response_data.len() as i64,
                    0i64,
                    content_hash
                ],
            )
            .await
            .unwrap();
        }

        // Add the requester as an active friend
        db.add_friend("friend-peer", "FriendNode", "friend:9843", FriendStatus::Active)
            .await
            .unwrap();

        let config = test_config("server-peer", "Server");
        let base = start_test_server(db, config).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/p2p/fetch?content_hash={content_hash}"))
            .header("X-Peer-Id", "friend-peer")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.bytes().await.unwrap();
        assert_eq!(body.as_ref(), b"hello cached world");
    }

    #[tokio::test]
    async fn test_fetch_not_found_for_missing_hash() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());

        db.add_friend("friend-peer", "FriendNode", "friend:9843", FriendStatus::Active)
            .await
            .unwrap();

        let config = test_config("server-peer", "Server");
        let base = start_test_server(db, config).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/p2p/fetch?content_hash=nonexistent"))
            .header("X-Peer-Id", "friend-peer")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn test_invite_adds_pending_received() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());
        let config = test_config("server-peer", "Server");
        let base = start_test_server(Arc::clone(&db), config).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/p2p/invite"))
            .json(&serde_json::json!({
                "peer_id": "remote-peer",
                "display_name": "RemoteNode",
                "public_addr": "remote:9843"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let friend = db.get_friend("remote-peer").await.unwrap();
        assert!(friend.is_some());
        let f = friend.unwrap();
        assert_eq!(f.status, FriendStatus::PendingReceived);
        assert_eq!(f.display_name, "RemoteNode");
    }

    #[tokio::test]
    async fn test_invite_accept_upgrades_pending_sent_to_active() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());

        // Pre-insert as pending_sent
        db.add_friend("remote-peer", "RemoteNode", "remote:9843", FriendStatus::PendingSent)
            .await
            .unwrap();

        let config = test_config("server-peer", "Server");
        let base = start_test_server(Arc::clone(&db), config).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/p2p/invite/accept"))
            .json(&serde_json::json!({
                "peer_id": "remote-peer",
                "display_name": "RemoteNode",
                "public_addr": "remote:9843"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let friend = db.get_friend("remote-peer").await.unwrap().unwrap();
        assert_eq!(friend.status, FriendStatus::Active);
    }

    #[tokio::test]
    async fn test_invite_accept_adds_active_when_not_present() {
        let db = Arc::new(CacheDb::open_in_memory().await.unwrap());
        let config = test_config("server-peer", "Server");
        let base = start_test_server(Arc::clone(&db), config).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/p2p/invite/accept"))
            .json(&serde_json::json!({
                "peer_id": "brand-new-peer",
                "display_name": "BrandNew",
                "public_addr": "new:9843"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let friend = db.get_friend("brand-new-peer").await.unwrap().unwrap();
        assert_eq!(friend.status, FriendStatus::Active);
        assert_eq!(friend.display_name, "BrandNew");
    }
}
