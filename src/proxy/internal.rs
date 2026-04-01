use crate::proxy::server::AppState;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio_stream::Stream;

/// GET /internal/stats — returns JSON with session counters.
pub async fn stats_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let stats = &state.stats;
    let entry_count = state.cache.entry_count().await.unwrap_or(0);

    Json(serde_json::json!({
        "hits": stats.hits.load(Ordering::Relaxed),
        "misses": stats.misses.load(Ordering::Relaxed),
        "skips": stats.skips.load(Ordering::Relaxed),
        "errors": stats.errors.load(Ordering::Relaxed),
        "coalesced": stats.coalesced.load(Ordering::Relaxed),
        "time_saved_ms": stats.time_saved_ms.load(Ordering::Relaxed),
        "estimated_tokens_saved": stats.estimated_tokens_saved(),
        "estimated_cost_saved_usd": stats.estimated_cost_saved_usd(),
        "uptime_secs": stats.uptime_secs(),
        "cache_entry_count": entry_count,
        "p2p_hits": stats.p2p_hits.load(Ordering::Relaxed),
        "p2p_served": stats.p2p_served.load(Ordering::Relaxed),
        "p2p_errors": stats.p2p_errors.load(Ordering::Relaxed),
        "p2p_enabled": state.p2p_client.is_some(),
    }))
}

/// GET /internal/p2p/friends — returns JSON list of friends from the cache DB.
pub async fn p2p_friends_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.cache.list_friends().await {
        Ok(friends) => {
            let json_friends: Vec<serde_json::Value> = friends
                .into_iter()
                .map(|f| {
                    serde_json::json!({
                        "peer_id": f.peer_id,
                        "display_name": f.display_name,
                        "public_addr": f.public_addr,
                        "status": f.status.to_string(),
                    })
                })
                .collect();
            Json(serde_json::json!({ "friends": json_friends })).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to list friends: {e}");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to list friends" })),
            )
                .into_response()
        }
    }
}

/// GET /internal/events — SSE stream of request events.
pub async fn events_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.event_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Ok(json) = serde_json::to_string(&event) {
                        yield Ok(Event::default().event("request").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "SSE consumer lagged, skipping events");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
