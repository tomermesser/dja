use crate::proxy::server::AppState;
use anyhow::Result;
use axum::body::Body;
use axum::http::{Request, Response};
use futures::TryStreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// Headers that should NOT be forwarded (hop-by-hop headers).
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// Forward the incoming request to the upstream and return the response.
pub async fn forward_request(
    state: &AppState,
    req: Request<Body>,
) -> Result<Response<Body>> {
    let upstream_url = format!(
        "{}{}",
        state.config.upstream.trim_end_matches('/'),
        req.uri().path_and_query().map(|pq| pq.to_string()).unwrap_or_default()
    );

    // Build upstream headers, passing through everything except hop-by-hop.
    let mut upstream_headers = HeaderMap::new();
    for (name, value) in req.headers() {
        let name_str = name.as_str().to_lowercase();
        if HOP_BY_HOP_HEADERS.contains(&name_str.as_str()) {
            continue;
        }
        upstream_headers.insert(name.clone(), value.clone());
    }

    let method = req.method().clone();

    // Collect the request body.
    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await?;

    // Build the upstream request.
    let upstream_req = state
        .http_client
        .request(method, &upstream_url)
        .headers(upstream_headers)
        .body(body_bytes);

    // Send to upstream.
    let upstream_resp = upstream_req.send().await?;

    // Map the response status and headers back.
    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();

    // Check if this is a streaming response (SSE).
    let is_streaming = resp_headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"));

    let mut response_builder = Response::builder().status(status.as_u16());

    // Forward response headers.
    for (name, value) in &resp_headers {
        let name_str = name.as_str().to_lowercase();
        if HOP_BY_HOP_HEADERS.contains(&name_str.as_str()) {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            response_builder = response_builder.header(n, v);
        }
    }

    if is_streaming {
        // Stream the response body through as SSE chunks.
        let byte_stream = upstream_resp
            .bytes_stream()
            .map_err(std::io::Error::other);
        let body = Body::from_stream(byte_stream);
        Ok(response_builder.body(body)?)
    } else {
        // Non-streaming: read full body and pass through.
        let body_bytes = upstream_resp.bytes().await?;
        Ok(response_builder.body(Body::from(body_bytes))?)
    }
}
