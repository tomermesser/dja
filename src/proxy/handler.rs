use crate::proxy::forward;
use crate::proxy::server::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, Response};
use std::sync::Arc;
use std::time::Instant;

/// Catch-all handler that forwards every request to the upstream API.
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
) -> Response<Body> {
    let method = req.method().clone();
    let path = req.uri().path_and_query()
        .map(|pq| pq.to_string())
        .unwrap_or_else(|| req.uri().path().to_string());

    let start = Instant::now();

    let response = forward::forward_request(&state, req).await;

    let latency = start.elapsed();

    match &response {
        Ok(resp) => {
            tracing::info!(
                method = %method,
                path = %path,
                status = resp.status().as_u16(),
                latency_ms = latency.as_millis() as u64,
                "proxied request"
            );
        }
        Err(e) => {
            tracing::error!(
                method = %method,
                path = %path,
                error = %e,
                latency_ms = latency.as_millis() as u64,
                "proxy error"
            );
        }
    }

    response.unwrap_or_else(|e| {
        Response::builder()
            .status(502)
            .body(Body::from(format!("Proxy error: {e}")))
            .expect("building error response")
    })
}
