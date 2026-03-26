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
        "time_saved_ms": stats.time_saved_ms.load(Ordering::Relaxed),
        "estimated_tokens_saved": stats.estimated_tokens_saved(),
        "estimated_cost_saved_usd": stats.estimated_cost_saved_usd(),
        "uptime_secs": stats.uptime_secs(),
        "cache_entry_count": entry_count,
    }))
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
