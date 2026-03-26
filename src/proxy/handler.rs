use crate::proxy::eligibility;
use crate::proxy::forward;
use crate::proxy::metrics::{self, RequestEvent};
use crate::proxy::server::AppState;
use crate::proxy::stream;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, Response};
use std::sync::Arc;
use std::time::Instant;

/// Catch-all handler that forwards every request to the upstream API,
/// with semantic caching for eligible POST /v1/messages requests.
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
) -> Response<Body> {
    let method = req.method().clone();
    let path = req.uri().path_and_query()
        .map(|pq| pq.to_string())
        .unwrap_or_else(|| req.uri().path().to_string());

    let start = Instant::now();

    // Only intercept POST /v1/messages
    let is_messages_post = method == Method::POST && req.uri().path() == "/v1/messages";

    let response = if is_messages_post {
        handle_messages_request(Arc::clone(&state), req, start).await
    } else {
        forward::forward_request(&state, req).await
    };

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

/// Handle a POST /v1/messages request with semantic caching.
async fn handle_messages_request(
    state: Arc<AppState>,
    req: Request<Body>,
    start: Instant,
) -> anyhow::Result<Response<Body>> {
    // Read the request body first so we can inspect it
    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, 10 * 1024 * 1024).await?;
    let body_size = body_bytes.len();

    // Check eligibility
    let parsed = match eligibility::check_eligibility(&body_bytes, state.config.multi_turn_caching) {
        Some(parsed) => parsed,
        None => {
            tracing::info!("cache SKIP: request not eligible");
            state.stats.record_skip();
            let _ = state.event_tx.send(RequestEvent {
                event_type: "skip".to_string(),
                latency_ms: None,
                prompt_snippet: None,
                model: None,
                similarity: None,
                cache_id: None,
                body_size,
                response_size: None,
                timestamp: metrics::now_timestamp(),
            });
            // Reconstruct the request and forward normally
            let req = Request::from_parts(parts, Body::from(body_bytes));
            return forward::forward_request(&state, req).await;
        }
    };

    // Embed the user message
    let embedding = match state.embedding.lock().await.embed(&parsed.user_message) {
        Ok(emb) => emb,
        Err(e) => {
            tracing::warn!(error = %e, "cache ERROR: embedding failed, falling back to forward");
            state.stats.record_error();
            let _ = state.event_tx.send(RequestEvent {
                event_type: "error".to_string(),
                latency_ms: Some(start.elapsed().as_millis() as u64),
                prompt_snippet: Some(parsed.user_message.chars().take(80).collect()),
                model: Some(parsed.model.clone()),
                similarity: None,
                cache_id: None,
                body_size,
                response_size: None,
                timestamp: metrics::now_timestamp(),
            });
            let req = Request::from_parts(parts, Body::from(body_bytes));
            return forward::forward_request(&state, req).await;
        }
    };

    // Search cache
    let cache_result = match state
        .cache
        .lookup(&embedding, &parsed.system_hash, &parsed.model, state.config.threshold as f32, state.config.match_system_prompt)
        .await
    {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(error = %e, "cache ERROR: lookup failed, falling back to forward");
            state.stats.record_error();
            let _ = state.event_tx.send(RequestEvent {
                event_type: "error".to_string(),
                latency_ms: Some(start.elapsed().as_millis() as u64),
                prompt_snippet: Some(parsed.user_message.chars().take(80).collect()),
                model: Some(parsed.model.clone()),
                similarity: None,
                cache_id: None,
                body_size,
                response_size: None,
                timestamp: metrics::now_timestamp(),
            });
            let req = Request::from_parts(parts, Body::from(body_bytes));
            return forward::forward_request(&state, req).await;
        }
    };

    match cache_result {
        Some(hit) => {
            // Cache HIT
            let snippet: String = parsed.user_message.chars().take(80).collect();
            let latency_ms = start.elapsed().as_millis() as u64;
            let response_size = hit.response_data.len();
            tracing::info!(
                similarity = hit.similarity,
                prompt_snippet = %snippet,
                cache_id = hit.id,
                "cache HIT"
            );

            state.stats.record_hit(latency_ms, body_size, response_size);
            let _ = state.event_tx.send(RequestEvent {
                event_type: "hit".to_string(),
                latency_ms: Some(latency_ms),
                prompt_snippet: Some(snippet),
                model: Some(parsed.model.clone()),
                similarity: Some(hit.similarity),
                cache_id: Some(hit.id),
                body_size,
                response_size: Some(response_size),
                timestamp: metrics::now_timestamp(),
            });

            // Return cached response bytes as-is.
            let content_type = if parsed.is_streaming {
                "text/event-stream"
            } else {
                "application/json"
            };
            let mut builder = Response::builder()
                .status(200)
                .header("content-type", content_type);

            // Check if stored data looks gzip-compressed (magic bytes 1f 8b)
            if hit.response_data.len() >= 2
                && hit.response_data[0] == 0x1f
                && hit.response_data[1] == 0x8b
            {
                builder = builder.header("content-encoding", "gzip");
            }

            Ok(builder
                .body(Body::from(hit.response_data))
                .expect("building cached response"))
        }
        None => {
            // Cache MISS
            let snippet: String = parsed.user_message.chars().take(80).collect();
            let latency_ms = start.elapsed().as_millis() as u64;
            tracing::info!(prompt_snippet = %snippet, "cache MISS");

            // Note: we record miss latency after the full upstream round-trip,
            // but we send the event now with the current latency. The actual
            // upstream latency will be captured in the proxied request log.
            state.stats.record_miss(latency_ms);
            let _ = state.event_tx.send(RequestEvent {
                event_type: "miss".to_string(),
                latency_ms: Some(latency_ms),
                prompt_snippet: Some(snippet),
                model: Some(parsed.model.clone()),
                similarity: None,
                cache_id: None,
                body_size,
                response_size: None,
                timestamp: metrics::now_timestamp(),
            });

            if parsed.is_streaming {
                // Streaming cache miss: tee the stream to client + buffer for cache
                let req = Request::from_parts(parts, Body::from(body_bytes));
                let (upstream_resp, response_builder) =
                    forward::forward_raw(&state, req, parsed.full_body).await?;

                let status = upstream_resp.status();
                let (body, buffer_rx) = stream::tee_stream(upstream_resp);
                let response = response_builder.body(body)?;

                // Spawn background task to cache the response once streaming completes
                if status.is_success() {
                    let state = Arc::clone(&state);
                    let user_message = parsed.user_message;
                    let system_hash = parsed.system_hash;
                    let model = parsed.model;

                    tokio::spawn(async move {
                        let max_response_size = state.config.max_response_size;
                        match buffer_rx.await {
                            Ok(buffer) => {
                                let response_size = buffer.len();
                                if response_size <= max_response_size {
                                    if let Err(e) = state
                                        .cache
                                        .store(
                                            &user_message,
                                            &system_hash,
                                            &model,
                                            &embedding,
                                            &buffer,
                                        )
                                        .await
                                    {
                                        tracing::warn!(error = %e, "cache ERROR: failed to store streamed response");
                                    } else {
                                        tracing::debug!(response_size, "cached streamed response stored");
                                    }
                                } else {
                                    tracing::debug!(
                                        response_size,
                                        max = max_response_size,
                                        "not caching oversized streamed response"
                                    );
                                }
                            }
                            Err(_) => {
                                tracing::warn!("cache ERROR: stream buffer channel closed before completion");
                            }
                        }
                    });
                }

                Ok(response)
            } else {
                // Non-streaming cache miss: buffer entire response
                let req = Request::from_parts(parts, Body::from(body_bytes));
                let (response, response_bytes) =
                    forward::forward_with_body(&state, req, parsed.full_body).await?;

                let status = response.status();
                let response_size = response_bytes.len();

                if status.is_success() && response_size <= state.config.max_response_size {
                    // Store in cache (fire-and-forget error handling)
                    if let Err(e) = state
                        .cache
                        .store(
                            &parsed.user_message,
                            &parsed.system_hash,
                            &parsed.model,
                            &embedding,
                            &response_bytes,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "cache ERROR: failed to store response");
                    } else {
                        tracing::debug!(response_size, "cached response stored");
                    }
                } else if !status.is_success() {
                    tracing::debug!(status = %status, "not caching non-success response");
                } else {
                    tracing::debug!(
                        response_size,
                        max = state.config.max_response_size,
                        "not caching oversized response"
                    );
                }

                Ok(response)
            }
        }
    }
}
