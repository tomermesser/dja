use crate::config::Config;
use crate::p2p::lookup::p2p_lookup;
use crate::proxy::cache_control;
use crate::proxy::eligibility;
use crate::proxy::forward;
use crate::proxy::inflight::{InflightMap, InflightStatus};
use crate::proxy::metrics::{self, RequestEvent};
use crate::proxy::server::AppState;
use crate::proxy::stream;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, Response};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Gate for cache_control injection. Returns modified bytes if injection is
/// enabled and applicable, otherwise returns a copy of the original bytes.
fn maybe_inject_cache_control(body: &[u8], config: &Config) -> bytes::Bytes {
    if config.auto_cache_control {
        cache_control::inject_cache_control(body)
            .unwrap_or_else(|| bytes::Bytes::copy_from_slice(body))
    } else {
        bytes::Bytes::copy_from_slice(body)
    }
}

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
                source: None,
            });
            // Injection is safe here: no cache key was extracted for skipped requests.
            let forward_body = maybe_inject_cache_control(&body_bytes, &state.config);
            let req = Request::from_parts(parts, Body::from(forward_body));
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
                source: None,
            });
            // Injection is safe here: cache key was already extracted in check_eligibility above.
            let forward_body = maybe_inject_cache_control(&body_bytes, &state.config);
            let req = Request::from_parts(parts, Body::from(forward_body));
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
                source: None,
            });
            // Injection is safe here: cache key was already extracted in check_eligibility above.
            let forward_body = maybe_inject_cache_control(&body_bytes, &state.config);
            let req = Request::from_parts(parts, Body::from(forward_body));
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
                source: Some(hit.source.clone()),
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
            // Cache MISS — try P2P before request coalescing / upstream.
            let snippet: String = parsed.user_message.chars().take(80).collect();
            let latency_ms = start.elapsed().as_millis() as u64;
            tracing::info!(prompt_snippet = %snippet, "cache MISS");

            // --- P2P lookup ---
            if let (Some(p2p_index), Some(p2p_client), Some(p2p_config)) = (
                state.p2p_index.as_ref(),
                state.p2p_client.as_ref(),
                state.p2p_config.as_ref(),
            ) {
                if let Some(p2p_hit) = p2p_lookup(
                    p2p_index,
                    p2p_client,
                    &state.cache,
                    p2p_config,
                    &embedding,
                    &parsed.model,
                    &parsed.system_hash,
                    state.config.match_system_prompt,
                    state.config.threshold as f32,
                )
                .await
                {
                    let p2p_latency_ms = start.elapsed().as_millis() as u64;
                    let response_size = p2p_hit.data.len();
                    let source = format!("p2p:{}", p2p_hit.peer_id);

                    tracing::info!(
                        peer_id = %p2p_hit.peer_id,
                        content_hash = %p2p_hit.content_hash,
                        response_size,
                        "P2P HIT: serving response from peer"
                    );

                    state.stats.record_p2p_hit(response_size);
                    let _ = state.event_tx.send(RequestEvent {
                        event_type: "p2p_hit".to_string(),
                        latency_ms: Some(p2p_latency_ms),
                        prompt_snippet: Some(snippet),
                        model: Some(parsed.model.clone()),
                        similarity: None,
                        cache_id: None,
                        body_size,
                        response_size: Some(response_size),
                        timestamp: metrics::now_timestamp(),
                        source: Some(source.clone()),
                    });

                    // Store in local cache so future lookups hit locally.
                    let store_result = state
                        .cache
                        .store(
                            &parsed.user_message,
                            &parsed.system_hash,
                            &parsed.model,
                            &embedding,
                            &p2p_hit.data,
                            &source,
                        )
                        .await;
                    if let Err(e) = store_result {
                        tracing::warn!(error = %e, "P2P: failed to store fetched response locally");
                    }

                    let content_type = if parsed.is_streaming {
                        "text/event-stream"
                    } else {
                        "application/json"
                    };
                    let mut builder = Response::builder()
                        .status(200)
                        .header("content-type", content_type);

                    if p2p_hit.data.len() >= 2
                        && p2p_hit.data[0] == 0x1f
                        && p2p_hit.data[1] == 0x8b
                    {
                        builder = builder.header("content-encoding", "gzip");
                    }

                    return Ok(builder
                        .body(Body::from(p2p_hit.data))
                        .expect("building p2p response"));
                }
            }
            // --- end P2P lookup ---

            // Request coalescing: if an identical request is already in-flight,
            // wait for it to complete and retry cache lookup instead of sending
            // a duplicate upstream request.
            let coalesce_key = if state.config.request_coalescing {
                let key = InflightMap::coalesce_key(&parsed.model, &parsed.user_message);
                match state.inflight.try_register(&key).await {
                    InflightStatus::Waiter(notify) => {
                        tracing::debug!(prompt_snippet = %snippet, "coalescing: waiting for in-flight request");
                        let wait_start = Instant::now();

                        let timed_out = tokio::time::timeout(
                            Duration::from_secs(60),
                            notify.notified(),
                        )
                        .await
                        .is_err();

                        if !timed_out {
                            // Leader completed — retry cache lookup
                            if let Ok(Some(hit)) = state
                                .cache
                                .lookup(
                                    &embedding,
                                    &parsed.system_hash,
                                    &parsed.model,
                                    state.config.threshold as f32,
                                    state.config.match_system_prompt,
                                )
                                .await
                            {
                                let waited_ms = wait_start.elapsed().as_millis() as u64;
                                let response_size = hit.response_data.len();
                                tracing::info!(
                                    similarity = hit.similarity,
                                    waited_ms,
                                    prompt_snippet = %snippet,
                                    "cache COALESCED"
                                );
                                state.stats.record_coalesced(body_size, response_size);
                                let _ = state.event_tx.send(RequestEvent {
                                    event_type: "coalesced".to_string(),
                                    latency_ms: Some(waited_ms),
                                    prompt_snippet: Some(snippet),
                                    model: Some(parsed.model.clone()),
                                    similarity: Some(hit.similarity),
                                    cache_id: Some(hit.id),
                                    body_size,
                                    response_size: Some(response_size),
                                    timestamp: metrics::now_timestamp(),
                                    source: Some(hit.source.clone()),
                                });

                                let content_type = if parsed.is_streaming {
                                    "text/event-stream"
                                } else {
                                    "application/json"
                                };
                                let mut builder = Response::builder()
                                    .status(200)
                                    .header("content-type", content_type);
                                if hit.response_data.len() >= 2
                                    && hit.response_data[0] == 0x1f
                                    && hit.response_data[1] == 0x8b
                                {
                                    builder = builder.header("content-encoding", "gzip");
                                }
                                return Ok(builder
                                    .body(Body::from(hit.response_data))
                                    .expect("building coalesced response"));
                            }
                            tracing::debug!("coalescing: cache still miss after wait, proceeding to upstream");
                        } else {
                            tracing::debug!("coalescing: timed out waiting, proceeding to upstream");
                        }
                        // Fall through to upstream — register as new leader
                        let _ = state.inflight.try_register(&key).await;
                        Some(key)
                    }
                    InflightStatus::Leader => Some(key),
                }
            } else {
                None
            };

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
                source: None,
            });

            // SAFETY: injection is safe here because the cache key (user_message embedding)
            // was already extracted above in check_eligibility. Mutating the forwarded bytes
            // does not affect the semantic cache lookup or storage paths.
            let forward_body = maybe_inject_cache_control(&body_bytes, &state.config);

            if parsed.is_streaming {
                // Streaming cache miss: tee the stream to client + buffer for cache
                let req = Request::from_parts(parts, Body::from(forward_body.clone()));
                let (upstream_resp, response_builder) =
                    forward::forward_raw(&state, req, forward_body).await?;

                let status = upstream_resp.status();
                let (body, buffer_rx) = stream::tee_stream(upstream_resp);
                let response = response_builder.body(body)?;

                // Spawn background task to cache the response once streaming completes
                let state_bg = Arc::clone(&state);
                let user_message = parsed.user_message;
                let system_hash = parsed.system_hash;
                let model = parsed.model;
                let hostname = state.hostname.clone();
                let is_success = status.is_success();

                tokio::spawn(async move {
                    if is_success {
                        let max_response_size = state_bg.config.max_response_size;
                        match buffer_rx.await {
                            Ok(buffer) => {
                                let response_size = buffer.len();
                                if response_size <= max_response_size {
                                    match state_bg
                                        .cache
                                        .store(
                                            &user_message,
                                            &system_hash,
                                            &model,
                                            &embedding,
                                            &buffer,
                                            &hostname,
                                        )
                                        .await
                                    {
                                        Err(e) => {
                                            tracing::warn!(error = %e, "cache ERROR: failed to store streamed response");
                                        }
                                        Ok(_) => {
                                            tracing::debug!(response_size, "cached streamed response stored");
                                            // Fire-and-forget: publish to central index.
                                            if let (Some(index), Some(p2p_cfg)) = (
                                                state_bg.p2p_index.as_ref(),
                                                state_bg.p2p_config.as_ref(),
                                            ) {
                                                use sha2::{Digest, Sha256};
                                                let content_hash = hex::encode(Sha256::digest(&buffer));
                                                let _ = index
                                                    .publish(
                                                        &p2p_cfg.peer_id,
                                                        &content_hash,
                                                        &model,
                                                        &system_hash,
                                                        &embedding,
                                                        response_size,
                                                    )
                                                    .await;
                                            }
                                        }
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
                    }
                    // Always complete the in-flight entry to unblock waiters
                    if let Some(key) = coalesce_key {
                        state_bg.inflight.complete(&key).await;
                    }
                });

                Ok(response)
            } else {
                // Non-streaming cache miss: buffer entire response
                let req = Request::from_parts(parts, Body::from(forward_body.clone()));
                let (response, response_bytes) =
                    forward::forward_with_body(&state, req, forward_body).await?;

                let status = response.status();
                let response_size = response_bytes.len();

                if status.is_success() && response_size <= state.config.max_response_size {
                    // Store in cache (fire-and-forget error handling)
                    match state
                        .cache
                        .store(
                            &parsed.user_message,
                            &parsed.system_hash,
                            &parsed.model,
                            &embedding,
                            &response_bytes,
                            &state.hostname,
                        )
                        .await
                    {
                        Err(e) => {
                            tracing::warn!(error = %e, "cache ERROR: failed to store response");
                        }
                        Ok(_) => {
                            tracing::debug!(response_size, "cached response stored");
                            // Fire-and-forget: publish to central index.
                            if let (Some(index), Some(p2p_cfg)) = (
                                state.p2p_index.as_ref(),
                                state.p2p_config.as_ref(),
                            ) {
                                use sha2::{Digest, Sha256};
                                let content_hash = hex::encode(Sha256::digest(&response_bytes));
                                let index = Arc::clone(index);
                                let p2p_cfg = Arc::clone(p2p_cfg);
                                let model = parsed.model.clone();
                                let system_hash = parsed.system_hash.clone();
                                tokio::spawn(async move {
                                    let _ = index
                                        .publish(
                                            &p2p_cfg.peer_id,
                                            &content_hash,
                                            &model,
                                            &system_hash,
                                            &embedding,
                                            response_size,
                                        )
                                        .await;
                                });
                            }
                        }
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

                // Complete the in-flight entry to unblock waiters
                if let Some(key) = coalesce_key {
                    state.inflight.complete(&key).await;
                }

                Ok(response)
            }
        }
    }
}
