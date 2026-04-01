use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::routing::post;
use axum::Router;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;

/// Count of requests that hit the mock server.
struct MockState {
    request_count: AtomicU32,
}

/// Mock Anthropic API: returns a simple non-streaming message response.
async fn mock_messages_handler(
    axum::extract::State(state): axum::extract::State<Arc<MockState>>,
    req: Request<Body>,
) -> Response<Body> {
    state.request_count.fetch_add(1, Ordering::SeqCst);

    // Read the request body to check if streaming is requested
    let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let is_streaming = body_json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    if is_streaming {
        // SSE streaming response
        let sse_body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_mock\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-20250514\",\"content\":[],\"stop_reason\":null}}\n",
            "\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from mock!\"}}\n",
            "\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n",
            "\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
            "\n",
        );
        Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from(sse_body))
            .unwrap()
    } else {
        // Non-streaming JSON response
        let response_json = serde_json::json!({
            "id": "msg_mock_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "text", "text": "Hello from mock!"}
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });
        Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&response_json).unwrap()))
            .unwrap()
    }
}

/// Start a mock Anthropic API server on a random port. Returns (addr, state).
async fn start_mock_server() -> (String, Arc<MockState>) {
    let state = Arc::new(MockState {
        request_count: AtomicU32::new(0),
    });

    let app = Router::new()
        .route("/v1/messages", post(mock_messages_handler))
        .with_state(Arc::clone(&state));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://127.0.0.1:{}", addr.port());

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (url, state)
}

/// Mock state that also captures the last received request body for inspection.
struct CapturingMockState {
    request_count: AtomicU32,
    last_body: tokio::sync::Mutex<Option<serde_json::Value>>,
}

async fn capturing_mock_handler(
    axum::extract::State(state): axum::extract::State<Arc<CapturingMockState>>,
    req: Request<Body>,
) -> Response<Body> {
    state.request_count.fetch_add(1, Ordering::SeqCst);

    let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    *state.last_body.lock().await = Some(body_json);

    let response_json = serde_json::json!({
        "id": "msg_mock_inject",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [{"type": "text", "text": "Injected!"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 3}
    });
    Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&response_json).unwrap()))
        .unwrap()
}

async fn start_capturing_mock_server() -> (String, Arc<CapturingMockState>) {
    let state = Arc::new(CapturingMockState {
        request_count: AtomicU32::new(0),
        last_body: tokio::sync::Mutex::new(None),
    });

    let app = axum::Router::new()
        .route("/v1/messages", axum::routing::post(capturing_mock_handler))
        .with_state(Arc::clone(&state));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://127.0.0.1:{}", addr.port());

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (url, state)
}

#[tokio::test]
async fn test_proxy_non_streaming_cache_hit() {
    // Start mock Anthropic server
    let (mock_url, mock_state) = start_mock_server().await;

    // Create a temp directory for the test's database
    let tmp_dir = tempfile::tempdir().unwrap();
    let db_path = tmp_dir.path().join("cache.db");

    // Open cache database
    let cache = dja::cache::CacheDb::open(&db_path).await.unwrap();

    // Load embedding model
    let model_dir = dja::embedding::download::default_model_dir().unwrap();
    if !model_dir.join("model.onnx").exists() {
        eprintln!("Skipping integration test: embedding model not downloaded. Run `dja init` first.");
        return;
    }
    let embedding_model = dja::embedding::EmbeddingModel::load(&model_dir).unwrap();

    // Create config pointing at mock
    let config = dja::config::Config {
        port: 0, // will be overridden by actual listener
        upstream: mock_url.clone(),
        threshold: 0.95,
        ttl: "30d".to_string(),
        max_entries: 10000,
        max_response_size: 102400,
        log_level: "debug".to_string(),
        match_system_prompt: false,
        multi_turn_caching: true,
        auto_cache_control: true,
        request_coalescing: true,
        p2p: Default::default(),
    };

    // Set up proxy
    let (event_tx, _rx) = dja::proxy::metrics::event_channel();
    let state = std::sync::Arc::new(dja::proxy::server::AppState {
        config,
        http_client: reqwest::Client::new(),
        embedding: tokio::sync::Mutex::new(embedding_model),
        cache,
        stats: dja::proxy::metrics::SessionStats::new(),
        event_tx,
        inflight: dja::proxy::inflight::InflightMap::new(),
        hostname: "test".to_string(),
        p2p_client: None,
        p2p_config: None,
        p2p_index: None,
    });

    let app = axum::Router::new()
        .fallback(dja::proxy::handler::proxy_handler)
        .with_state(state);

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_url = format!("http://127.0.0.1:{}", proxy_addr.port());

    tokio::spawn(async move {
        axum::serve(proxy_listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();

    // --- First request: should go to mock (cache MISS) ---
    let request_body = serde_json::json!({
        "model": "claude-sonnet-4-20250514",
        "messages": [
            {"role": "user", "content": "What is the meaning of life?"}
        ]
    });

    let resp = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .header("x-api-key", "test-key")
        .json(&request_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let text = body["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, "Hello from mock!");
    assert_eq!(mock_state.request_count.load(Ordering::SeqCst), 1);

    // Small delay to let the cache store complete
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // --- Second request (same prompt): should come from cache ---
    let resp2 = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .header("x-api-key", "test-key")
        .json(&request_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp2.status(), StatusCode::OK);
    let body2: serde_json::Value = resp2.json().await.unwrap();
    let text2 = body2["content"][0]["text"].as_str().unwrap();

    // Cached response should have the [cached] marker prepended to the first text block.
    assert_eq!(text2, "[cached] Hello from mock!", "Cached response should have [cached] marker");

    // Mock should NOT have been called a second time
    assert_eq!(
        mock_state.request_count.load(Ordering::SeqCst),
        1,
        "Mock was called again; response should have come from cache"
    );
}

#[tokio::test]
async fn test_proxy_streaming_cache_hit() {
    // Start mock Anthropic server
    let (mock_url, mock_state) = start_mock_server().await;

    // Create a temp directory for the test's database
    let tmp_dir = tempfile::tempdir().unwrap();
    let db_path = tmp_dir.path().join("cache.db");

    let cache = dja::cache::CacheDb::open(&db_path).await.unwrap();

    let model_dir = dja::embedding::download::default_model_dir().unwrap();
    if !model_dir.join("model.onnx").exists() {
        eprintln!("Skipping integration test: embedding model not downloaded.");
        return;
    }
    let embedding_model = dja::embedding::EmbeddingModel::load(&model_dir).unwrap();

    let config = dja::config::Config {
        port: 0,
        upstream: mock_url.clone(),
        threshold: 0.95,
        ttl: "30d".to_string(),
        max_entries: 10000,
        max_response_size: 102400,
        log_level: "debug".to_string(),
        match_system_prompt: false,
        multi_turn_caching: true,
        auto_cache_control: true,
        request_coalescing: true,
        p2p: Default::default(),
    };

    let (event_tx, _rx) = dja::proxy::metrics::event_channel();
    let state = std::sync::Arc::new(dja::proxy::server::AppState {
        config,
        http_client: reqwest::Client::new(),
        embedding: tokio::sync::Mutex::new(embedding_model),
        cache,
        stats: dja::proxy::metrics::SessionStats::new(),
        event_tx,
        inflight: dja::proxy::inflight::InflightMap::new(),
        hostname: "test".to_string(),
        p2p_client: None,
        p2p_config: None,
        p2p_index: None,
    });

    let app = axum::Router::new()
        .fallback(dja::proxy::handler::proxy_handler)
        .with_state(state);

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_url = format!("http://127.0.0.1:{}", proxy_addr.port());

    tokio::spawn(async move {
        axum::serve(proxy_listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();

    // --- First request (streaming): should go to mock ---
    let request_body = serde_json::json!({
        "model": "claude-sonnet-4-20250514",
        "stream": true,
        "messages": [
            {"role": "user", "content": "Explain quantum computing briefly."}
        ]
    });

    let resp = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .header("x-api-key", "test-key")
        .json(&request_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body_text = resp.text().await.unwrap();
    assert!(body_text.contains("Hello from mock!"));
    assert_eq!(mock_state.request_count.load(Ordering::SeqCst), 1);

    // Wait for cache store (streaming cache writes happen asynchronously)
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // --- Second request (same prompt, streaming): should come from cache ---
    let resp2 = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .header("x-api-key", "test-key")
        .json(&request_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp2.status(), StatusCode::OK);
    let body_text2 = resp2.text().await.unwrap();

    let mock_count = mock_state.request_count.load(Ordering::SeqCst);

    // Cached response should contain the original text (marker injection disabled)
    assert!(
        body_text2.contains("Hello from mock!"),
        "Expected original text in cached SSE stream (mock called {mock_count} times), got: {body_text2}"
    );

    // Mock should NOT have been called again
    assert_eq!(
        mock_count,
        1,
        "Mock was called again; streaming response should have come from cache"
    );
}

#[tokio::test]
async fn test_cache_control_injected_on_miss() {
    let (mock_url, mock_state) = start_capturing_mock_server().await;

    let tmp_dir = tempfile::tempdir().unwrap();
    let db_path = tmp_dir.path().join("cache.db");
    let cache = dja::cache::CacheDb::open(&db_path).await.unwrap();

    let model_dir = dja::embedding::download::default_model_dir().unwrap();
    if !model_dir.join("model.onnx").exists() {
        eprintln!("Skipping: embedding model not downloaded. Run `dja init` first.");
        return;
    }
    let embedding_model = dja::embedding::EmbeddingModel::load(&model_dir).unwrap();

    let config = dja::config::Config {
        port: 0,
        upstream: mock_url.clone(),
        threshold: 0.95,
        ttl: "30d".to_string(),
        max_entries: 10000,
        max_response_size: 102400,
        log_level: "debug".to_string(),
        match_system_prompt: false,
        multi_turn_caching: true,
        auto_cache_control: true,
        request_coalescing: true,
        p2p: Default::default(),
    };

    let (event_tx, _rx) = dja::proxy::metrics::event_channel();
    let state = std::sync::Arc::new(dja::proxy::server::AppState {
        config,
        http_client: reqwest::Client::new(),
        embedding: tokio::sync::Mutex::new(embedding_model),
        cache,
        stats: dja::proxy::metrics::SessionStats::new(),
        event_tx,
        inflight: dja::proxy::inflight::InflightMap::new(),
        hostname: "test".to_string(),
        p2p_client: None,
        p2p_config: None,
        p2p_index: None,
    });

    let app = axum::Router::new()
        .fallback(dja::proxy::handler::proxy_handler)
        .with_state(state);

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_url = format!("http://127.0.0.1:{}", proxy_addr.port());

    tokio::spawn(async move {
        axum::serve(proxy_listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();

    let request_body = serde_json::json!({
        "model": "claude-sonnet-4-20250514",
        "system": "You are a concise assistant.",
        "tools": [
            {
                "name": "get_time",
                "description": "Get the current time",
                "input_schema": {"type": "object", "properties": {}}
            }
        ],
        "messages": [{"role": "user", "content": "What time is it?"}]
    });

    let resp = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .header("x-api-key", "test-key")
        .json(&request_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(mock_state.request_count.load(Ordering::SeqCst), 1);

    let received = mock_state.last_body.lock().await;
    let received_json = received.as_ref().expect("mock should have received a request");

    // System should now be an array with cache_control on the last block
    let system = received_json.get("system").expect("system field must be present");
    assert!(system.is_array(), "system should be converted to array by injection");
    let system_blocks = system.as_array().unwrap();
    assert!(
        system_blocks.last().unwrap().get("cache_control").is_some(),
        "last system block must have cache_control injected"
    );

    // Last tool should have cache_control
    let tools = received_json
        .get("tools")
        .expect("tools field must be present")
        .as_array()
        .unwrap();
    assert!(
        tools.last().unwrap().get("cache_control").is_some(),
        "last tool must have cache_control injected"
    );
}

#[tokio::test]
async fn test_cache_control_not_injected_when_disabled() {
    let (mock_url, mock_state) = start_capturing_mock_server().await;

    let tmp_dir = tempfile::tempdir().unwrap();
    let db_path = tmp_dir.path().join("cache.db");
    let cache = dja::cache::CacheDb::open(&db_path).await.unwrap();

    let model_dir = dja::embedding::download::default_model_dir().unwrap();
    if !model_dir.join("model.onnx").exists() {
        eprintln!("Skipping: embedding model not downloaded. Run `dja init` first.");
        return;
    }
    let embedding_model = dja::embedding::EmbeddingModel::load(&model_dir).unwrap();

    let config = dja::config::Config {
        port: 0,
        upstream: mock_url.clone(),
        threshold: 0.95,
        ttl: "30d".to_string(),
        max_entries: 10000,
        max_response_size: 102400,
        log_level: "debug".to_string(),
        match_system_prompt: false,
        multi_turn_caching: true,
        auto_cache_control: false,
        request_coalescing: true,
        p2p: Default::default(),
    };

    let (event_tx, _rx) = dja::proxy::metrics::event_channel();
    let state = std::sync::Arc::new(dja::proxy::server::AppState {
        config,
        http_client: reqwest::Client::new(),
        embedding: tokio::sync::Mutex::new(embedding_model),
        cache,
        stats: dja::proxy::metrics::SessionStats::new(),
        event_tx,
        inflight: dja::proxy::inflight::InflightMap::new(),
        hostname: "test".to_string(),
        p2p_client: None,
        p2p_config: None,
        p2p_index: None,
    });

    let app = axum::Router::new()
        .fallback(dja::proxy::handler::proxy_handler)
        .with_state(state);

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_url = format!("http://127.0.0.1:{}", proxy_addr.port());

    tokio::spawn(async move {
        axum::serve(proxy_listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();

    let request_body = serde_json::json!({
        "model": "claude-sonnet-4-20250514",
        "system": "You are a concise assistant.",
        "messages": [{"role": "user", "content": "Disabled injection test"}]
    });

    let resp = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .header("x-api-key", "test-key")
        .json(&request_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let received = mock_state.last_body.lock().await;
    let received_json = received.as_ref().unwrap();

    // System should remain a string — no injection happened
    let system = received_json.get("system").unwrap();
    assert!(
        system.is_string(),
        "system should remain a string when auto_cache_control is disabled, got: {:?}", system
    );
}

// ---------------------------------------------------------------------------
// GET /internal/p2p/friends
// ---------------------------------------------------------------------------

/// Helper: spin up a minimal proxy with only the internal routes wired in (no
/// embedding model needed) and return its base URL.
async fn start_internal_only_server(cache: dja::cache::CacheDb) -> String {
    use axum::routing::get;

    let config = dja::config::Config {
        port: 0,
        upstream: "http://127.0.0.1:1".to_string(), // never reached
        threshold: 0.95,
        ttl: "30d".to_string(),
        max_entries: 10000,
        max_response_size: 102400,
        log_level: "error".to_string(),
        match_system_prompt: false,
        multi_turn_caching: true,
        auto_cache_control: true,
        request_coalescing: true,
        p2p: Default::default(),
    };

    let (event_tx, _rx) = dja::proxy::metrics::event_channel();

    // We need a dummy embedding model. Use an empty Mutex to satisfy the type
    // signature — these handlers never touch the embedding model.
    // We can't easily construct EmbeddingModel without the model files, so we
    // build the AppState with a real (but unused) model slot by relying on the
    // fact that the internal handlers never access `state.embedding`.
    //
    // Workaround: build the state with an in-memory DB and skip the embedding
    // field by building a full AppState with a try-load that falls back to a
    // stub. Since we can't instantiate EmbeddingModel without files, we skip
    // the embedding and use a separate minimal axum router that directly
    // calls p2p_friends_handler with only the AppState fields it needs.
    //
    // The cleanest solution: build AppState and let the handler use only cache.
    // We must supply an EmbeddingModel — attempt to load from default path and
    // skip the test gracefully if the model is absent.
    let model_dir = dja::embedding::download::default_model_dir().unwrap();
    if !model_dir.join("model.onnx").exists() {
        // Can't run without embedding model; signal caller to skip.
        return String::new();
    }
    let embedding_model = dja::embedding::EmbeddingModel::load(&model_dir).unwrap();

    let state = std::sync::Arc::new(dja::proxy::server::AppState {
        config,
        http_client: reqwest::Client::new(),
        embedding: tokio::sync::Mutex::new(embedding_model),
        cache,
        stats: dja::proxy::metrics::SessionStats::new(),
        event_tx,
        inflight: dja::proxy::inflight::InflightMap::new(),
        hostname: "test".to_string(),
        p2p_client: None,
        p2p_config: None,
        p2p_index: None,
    });

    let app = axum::Router::new()
        .route("/internal/p2p/friends", get(dja::proxy::internal::p2p_friends_handler))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://127.0.0.1:{}", addr.port());

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    url
}

#[tokio::test]
async fn test_p2p_friends_endpoint_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = dja::cache::CacheDb::open(&tmp.path().join("cache.db")).await.unwrap();
    let base_url = start_internal_only_server(cache).await;
    if base_url.is_empty() {
        eprintln!("Skipping test_p2p_friends_endpoint_empty: embedding model not available.");
        return;
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base_url}/internal/p2p/friends"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let friends = body["friends"].as_array().unwrap();
    assert!(friends.is_empty(), "Expected no friends, got: {friends:?}");
}

#[tokio::test]
async fn test_p2p_friends_endpoint_with_data() {
    use dja::p2p::friends::FriendStatus;

    let tmp = tempfile::tempdir().unwrap();
    let cache = dja::cache::CacheDb::open(&tmp.path().join("cache.db")).await.unwrap();

    // Pre-populate friends.
    cache
        .add_friend("peer-alice", "Alice's Mac", "alice.tail:9843", FriendStatus::Active)
        .await
        .unwrap();
    cache
        .add_friend("peer-bob", "Bob's Workstation", "bob.tail:9843", FriendStatus::Active)
        .await
        .unwrap();
    cache
        .add_friend("peer-dave", "dave", "dave.tail:9843", FriendStatus::PendingSent)
        .await
        .unwrap();

    let base_url = start_internal_only_server(cache).await;
    if base_url.is_empty() {
        eprintln!("Skipping test_p2p_friends_endpoint_with_data: embedding model not available.");
        return;
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base_url}/internal/p2p/friends"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let friends = body["friends"].as_array().unwrap();

    assert_eq!(friends.len(), 3, "Expected 3 friends, got: {}", friends.len());

    // Find Alice and verify fields.
    let alice = friends
        .iter()
        .find(|f| f["peer_id"].as_str() == Some("peer-alice"))
        .expect("Alice not found in response");
    assert_eq!(alice["display_name"].as_str().unwrap(), "Alice's Mac");
    assert_eq!(alice["public_addr"].as_str().unwrap(), "alice.tail:9843");
    assert_eq!(alice["status"].as_str().unwrap(), "active");

    // Verify Dave's pending_sent status is serialised correctly.
    let dave = friends
        .iter()
        .find(|f| f["peer_id"].as_str() == Some("peer-dave"))
        .expect("Dave not found in response");
    assert_eq!(dave["status"].as_str().unwrap(), "pending_sent");
}
