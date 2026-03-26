# dja monitor — TUI Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a real-time terminal dashboard (`dja monitor`) that shows live request flow, session stats, and estimated savings.

**Architecture:** The daemon gets an in-memory `SessionStats` struct (atomic counters) and a broadcast channel for request events. Two new internal HTTP endpoints serve stats JSON and SSE event streams. The `dja monitor` CLI command connects to these endpoints and renders a `ratatui` TUI.

**Tech Stack:** `ratatui` + `crossterm` for TUI, `tokio::sync::broadcast` for event bus, Axum SSE for streaming events to the monitor.

---

### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add ratatui and crossterm to Cargo.toml**

Add these lines to the `[dependencies]` section:

```toml
# TUI for monitor
ratatui = "0.29"
crossterm = "0.28"
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add ratatui and crossterm for monitor TUI"
```

---

### Task 2: Create SessionStats and RequestEvent

**Files:**
- Create: `src/proxy/metrics.rs`
- Modify: `src/proxy/mod.rs` (add `pub mod metrics;`)

- [ ] **Step 1: Create the metrics module**

Create `src/proxy/metrics.rs`:

```rust
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::broadcast;

/// A single request event emitted by the proxy handler.
#[derive(Debug, Clone, Serialize)]
pub struct RequestEvent {
    /// "hit", "miss", "skip", or "error"
    pub event_type: String,
    /// Latency in milliseconds (None for skips)
    pub latency_ms: Option<u64>,
    /// First 80 chars of the user message (None for skips)
    pub prompt_snippet: Option<String>,
    /// Model name
    pub model: Option<String>,
    /// Similarity score (only for hits)
    pub similarity: Option<f32>,
    /// Cache entry ID (only for hits)
    pub cache_id: Option<i64>,
    /// Request body size in bytes
    pub body_size: usize,
    /// Response size in bytes (only for hits and misses)
    pub response_size: Option<usize>,
    /// ISO 8601 timestamp
    pub timestamp: String,
}

/// Cumulative session statistics, reset on daemon restart.
/// All fields use atomic operations for lock-free concurrent access.
pub struct SessionStats {
    pub started_at: Instant,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub skips: AtomicU64,
    pub errors: AtomicU64,
    /// Sum of (avg_miss_latency - hit_latency) for each HIT, in milliseconds.
    pub time_saved_ms: AtomicU64,
    /// Sum of request body sizes for HITs (used to estimate tokens saved).
    pub upstream_bytes_saved: AtomicU64,
    /// Sum of response sizes for HITs (used to estimate output tokens saved).
    pub response_bytes_saved: AtomicU64,
    /// Sum of all MISS latencies, used to compute average.
    pub total_miss_latency_ms: AtomicU64,
}

impl SessionStats {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            skips: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            time_saved_ms: AtomicU64::new(0),
            upstream_bytes_saved: AtomicU64::new(0),
            response_bytes_saved: AtomicU64::new(0),
            total_miss_latency_ms: AtomicU64::new(0),
        }
    }

    pub fn record_hit(&self, latency_ms: u64, body_size: usize, response_size: usize) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.upstream_bytes_saved.fetch_add(body_size as u64, Ordering::Relaxed);
        self.response_bytes_saved.fetch_add(response_size as u64, Ordering::Relaxed);

        // Estimate time saved: avg_miss_latency - hit_latency
        let misses = self.misses.load(Ordering::Relaxed);
        let avg_miss = if misses > 0 {
            self.total_miss_latency_ms.load(Ordering::Relaxed) / misses
        } else {
            1000 // default 1 second if no misses observed
        };
        let saved = avg_miss.saturating_sub(latency_ms);
        self.time_saved_ms.fetch_add(saved, Ordering::Relaxed);
    }

    pub fn record_miss(&self, latency_ms: u64) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        self.total_miss_latency_ms.fetch_add(latency_ms, Ordering::Relaxed);
    }

    pub fn record_skip(&self) {
        self.skips.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Estimate tokens saved. ~4 chars per token for English text.
    pub fn estimated_tokens_saved(&self) -> u64 {
        let input_bytes = self.upstream_bytes_saved.load(Ordering::Relaxed);
        let output_bytes = self.response_bytes_saved.load(Ordering::Relaxed);
        (input_bytes + output_bytes) / 4
    }

    /// Estimate cost saved in USD. Uses average pricing across model families.
    /// Rough: $10/1M input tokens, $30/1M output tokens (weighted average).
    pub fn estimated_cost_saved_usd(&self) -> f64 {
        let input_tokens = self.upstream_bytes_saved.load(Ordering::Relaxed) as f64 / 4.0;
        let output_tokens = self.response_bytes_saved.load(Ordering::Relaxed) as f64 / 4.0;
        (input_tokens * 10.0 + output_tokens * 30.0) / 1_000_000.0
    }
}

/// Create a broadcast channel for request events.
/// Capacity of 256 events — if the monitor falls behind, old events are dropped.
pub fn event_channel() -> (broadcast::Sender<RequestEvent>, broadcast::Receiver<RequestEvent>) {
    broadcast::channel(256)
}

/// Get current timestamp as ISO 8601 string.
pub fn now_iso8601() -> String {
    // Use a simple approach without chrono dependency
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Format as seconds-since-epoch (monitor can format for display)
    format!("{secs}")
}
```

- [ ] **Step 2: Register the module**

Add to `src/proxy/mod.rs`:

```rust
pub mod metrics;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 4: Commit**

```bash
git add src/proxy/metrics.rs src/proxy/mod.rs
git commit -m "feat(monitor): add SessionStats and RequestEvent types"
```

---

### Task 3: Wire metrics into AppState and handler

**Files:**
- Modify: `src/proxy/server.rs`
- Modify: `src/proxy/handler.rs`

- [ ] **Step 1: Add SessionStats and broadcast sender to AppState**

In `src/proxy/server.rs`, add imports and fields:

```rust
use crate::proxy::metrics::{self, RequestEvent, SessionStats};
use tokio::sync::broadcast;
```

Update `AppState`:

```rust
pub struct AppState {
    pub config: Config,
    pub http_client: reqwest::Client,
    pub embedding: Mutex<EmbeddingModel>,
    pub cache: CacheDb,
    pub stats: SessionStats,
    pub event_tx: broadcast::Sender<RequestEvent>,
}
```

In the `run()` function, create the channel and stats before building `AppState`:

```rust
let (event_tx, _event_rx) = metrics::event_channel();

let state = Arc::new(AppState {
    config,
    http_client,
    embedding: Mutex::new(embedding_model),
    cache,
    stats: SessionStats::new(),
    event_tx,
});
```

- [ ] **Step 2: Record events in the handler**

In `src/proxy/handler.rs`, add the import:

```rust
use crate::proxy::metrics::{RequestEvent, now_iso8601};
```

After the cache HIT log (after line 175), add:

```rust
state.stats.record_hit(
    start.elapsed().as_millis() as u64,
    body_bytes.len(),
    hit.response_data.len(),
);
let _ = state.event_tx.send(RequestEvent {
    event_type: "hit".into(),
    latency_ms: Some(start.elapsed().as_millis() as u64),
    prompt_snippet: Some(snippet.clone()),
    model: Some(parsed.model.clone()),
    similarity: Some(hit.similarity),
    cache_id: Some(hit.id),
    body_size: body_bytes.len(),
    response_size: Some(hit.response_data.len()),
    timestamp: now_iso8601(),
});
```

After the cache MISS log (after line 205), add:

```rust
state.stats.record_miss(start.elapsed().as_millis() as u64);
let _ = state.event_tx.send(RequestEvent {
    event_type: "miss".into(),
    latency_ms: Some(start.elapsed().as_millis() as u64),
    prompt_snippet: Some(snippet.clone()),
    model: Some(parsed.model.clone()),
    similarity: None,
    cache_id: None,
    body_size: body_bytes.len(),
    response_size: None,
    timestamp: now_iso8601(),
});
```

After the cache SKIP log (after line 135), add:

```rust
state.stats.record_skip();
let _ = state.event_tx.send(RequestEvent {
    event_type: "skip".into(),
    latency_ms: None,
    prompt_snippet: None,
    model: None,
    similarity: None,
    cache_id: None,
    body_size: body_bytes.len(),
    response_size: None,
    timestamp: now_iso8601(),
});
```

Note: `start` is defined at the top of `proxy_handler` but the `handle_messages_request` function needs its own `start`. Add `let start = Instant::now();` at the top of `handle_messages_request` (it's separate from the outer `start` in `proxy_handler`). Actually, looking at the code, `handle_messages_request` doesn't have access to `start`. You need to pass it in or create a new one. The simplest approach: add a `start` parameter to `handle_messages_request`:

Change the signature to:
```rust
async fn handle_messages_request(
    state: Arc<AppState>,
    req: Request<Body>,
    start: Instant,
) -> anyhow::Result<Response<Body>> {
```

And update the call site in `proxy_handler` (line 28):
```rust
handle_messages_request(Arc::clone(&state), req, start).await
```

Remove the existing `let start = Instant::now();` from `proxy_handler` — no, keep it there since it's used later for latency logging. Just pass it through.

- [ ] **Step 3: Verify it compiles and tests pass**

Run: `cargo test`
Expected: all tests pass (integration tests may need `stats` and `event_tx` added to test `AppState` construction)

- [ ] **Step 4: Fix integration tests**

In `tests/proxy_test.rs`, update both `AppState` struct literals to include the new fields:

```rust
use dja::proxy::metrics;

// In both test functions:
let (event_tx, _) = metrics::event_channel();
// Add to AppState:
stats: metrics::SessionStats::new(),
event_tx,
```

- [ ] **Step 5: Run tests again**

Run: `cargo test`
Expected: all 71 tests pass

- [ ] **Step 6: Commit**

```bash
git add src/proxy/server.rs src/proxy/handler.rs tests/proxy_test.rs
git commit -m "feat(monitor): wire SessionStats and event broadcast into handler"
```

---

### Task 4: Add internal HTTP endpoints

**Files:**
- Modify: `src/proxy/server.rs`
- Modify: `src/proxy/handler.rs` (or create a new `src/proxy/internal.rs`)

- [ ] **Step 1: Create internal endpoint handlers**

Create `src/proxy/internal.rs`:

```rust
use crate::proxy::server::AppState;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Json;
use futures::stream::Stream;
use serde_json::json;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// GET /internal/stats — returns session stats as JSON.
pub async fn stats_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let stats = &state.stats;
    let cache_entry_count = state.cache.entry_count().await.unwrap_or(0);

    Json(json!({
        "hits": stats.hits.load(Ordering::Relaxed),
        "misses": stats.misses.load(Ordering::Relaxed),
        "skips": stats.skips.load(Ordering::Relaxed),
        "errors": stats.errors.load(Ordering::Relaxed),
        "time_saved_ms": stats.time_saved_ms.load(Ordering::Relaxed),
        "estimated_tokens_saved": stats.estimated_tokens_saved(),
        "estimated_cost_saved_usd": stats.estimated_cost_saved_usd(),
        "uptime_secs": stats.uptime_secs(),
        "cache_entry_count": cache_entry_count,
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
                    if let Ok(data) = serde_json::to_string(&event) {
                        yield Ok(Event::default().event("request").data(data));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // Monitor fell behind — skip dropped events
                    tracing::debug!(skipped = n, "monitor event stream lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

- [ ] **Step 2: Add async-stream dependency**

Add to `Cargo.toml` under `[dependencies]`:

```toml
async-stream = "0.3"
```

- [ ] **Step 3: Register the module and routes**

Add to `src/proxy/mod.rs`:

```rust
pub mod internal;
```

In `src/proxy/server.rs`, update the router to include internal routes:

```rust
use crate::proxy::internal;
use axum::routing::get;

let app = Router::new()
    .route("/internal/stats", get(internal::stats_handler))
    .route("/internal/events", get(internal::events_handler))
    .fallback(handler::proxy_handler)
    .with_state(state);
```

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add src/proxy/internal.rs src/proxy/mod.rs src/proxy/server.rs Cargo.toml Cargo.lock
git commit -m "feat(monitor): add /internal/stats and /internal/events SSE endpoints"
```

---

### Task 5: Build the TUI monitor

**Files:**
- Create: `src/cli/monitor.rs`
- Modify: `src/cli/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Create the monitor module**

Create `src/cli/monitor.rs`:

```rust
use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Gauge, Paragraph, Row, Table},
};
use serde::Deserialize;
use std::io::stdout;
use std::time::Duration;

use crate::config::Config;

/// A request event received from the SSE stream.
#[derive(Debug, Clone, Deserialize)]
struct EventData {
    event_type: String,
    latency_ms: Option<u64>,
    prompt_snippet: Option<String>,
    model: Option<String>,
    similarity: Option<f32>,
    #[allow(dead_code)]
    cache_id: Option<i64>,
    #[allow(dead_code)]
    body_size: usize,
    #[allow(dead_code)]
    response_size: Option<usize>,
    #[allow(dead_code)]
    timestamp: String,
}

/// Stats response from /internal/stats.
#[derive(Debug, Clone, Deserialize)]
struct StatsData {
    hits: u64,
    misses: u64,
    skips: u64,
    errors: u64,
    time_saved_ms: u64,
    estimated_tokens_saved: u64,
    estimated_cost_saved_usd: f64,
    uptime_secs: u64,
    cache_entry_count: u64,
}

/// State for the monitor TUI.
struct MonitorState {
    stats: Option<StatsData>,
    events: Vec<EventData>,
    connected: bool,
    max_events: usize,
}

impl MonitorState {
    fn new() -> Self {
        Self {
            stats: None,
            events: Vec::new(),
            connected: false,
            max_events: 100,
        }
    }

    fn push_event(&mut self, event: EventData) {
        self.events.insert(0, event); // newest first
        if self.events.len() > self.max_events {
            self.events.pop();
        }
    }
}

pub async fn run() -> Result<()> {
    let config = Config::load()?;
    let base_url = format!("http://127.0.0.1:{}", config.port);

    // Check if daemon is running
    let client = reqwest::Client::new();
    if client.get(format!("{base_url}/internal/stats")).send().await.is_err() {
        anyhow::bail!("dja is not running. Start it with `dja start`.");
    }

    // Set up terminal
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = run_monitor(&mut terminal, &client, &base_url).await;

    // Restore terminal
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

async fn run_monitor(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    client: &reqwest::Client,
    base_url: &str,
) -> Result<()> {
    let mut state = MonitorState::new();

    // Start SSE event stream in background
    let events_url = format!("{base_url}/internal/events");
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<EventData>(64);

    let sse_client = client.clone();
    tokio::spawn(async move {
        loop {
            match connect_sse(&sse_client, &events_url, &event_tx).await {
                Ok(()) => break, // clean shutdown
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });

    let stats_url = format!("{base_url}/internal/stats");
    let mut stats_interval = tokio::time::interval(Duration::from_secs(2));
    let mut render_interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            _ = render_interval.tick() => {
                // Check for keyboard input (non-blocking)
                if event::poll(Duration::from_millis(0))? {
                    if let Event::Key(key) = event::read()? {
                        if key.kind == KeyEventKind::Press
                            && (key.code == KeyCode::Char('q') || key.code == KeyCode::Char('c') && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL))
                        {
                            return Ok(());
                        }
                    }
                }
                terminal.draw(|frame| render(frame, &state))?;
            }
            _ = stats_interval.tick() => {
                // Poll stats
                match client.get(&stats_url).send().await {
                    Ok(resp) => {
                        if let Ok(stats) = resp.json::<StatsData>().await {
                            state.stats = Some(stats);
                            state.connected = true;
                        }
                    }
                    Err(_) => {
                        state.connected = false;
                    }
                }
            }
            Some(event) = event_rx.recv() => {
                state.push_event(event);
            }
        }
    }
}

/// Connect to SSE stream and forward events.
async fn connect_sse(
    client: &reqwest::Client,
    url: &str,
    tx: &tokio::sync::mpsc::Sender<EventData>,
) -> Result<()> {
    let resp = client.get(url).send().await.context("connecting to SSE")?;
    let mut stream = resp.bytes_stream();

    use futures::StreamExt;
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading SSE chunk")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Parse SSE events from buffer
        while let Some(pos) = buffer.find("\n\n") {
            let event_text = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            // Extract data line
            for line in event_text.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<EventData>(data) {
                        let _ = tx.send(event).await;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Render the TUI.
fn render(frame: &mut Frame, state: &MonitorState) {
    let area = frame.area();

    // Split into: header (1), stats panel (5), live feed (rest)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // header
            Constraint::Length(6),  // stats
            Constraint::Min(5),    // live feed
        ])
        .split(area);

    // Header
    render_header(frame, chunks[0], state);

    // Stats panel
    render_stats(frame, chunks[1], state);

    // Live feed
    render_live_feed(frame, chunks[2], state);
}

fn render_header(frame: &mut Frame, area: Rect, state: &MonitorState) {
    let uptime = state.stats.as_ref().map(|s| format_duration(s.uptime_secs)).unwrap_or_else(|| "---".into());
    let entries = state.stats.as_ref().map(|s| s.cache_entry_count.to_string()).unwrap_or_else(|| "---".into());
    let status = if state.connected { "connected" } else { "disconnected" };

    let header = Line::from(vec![
        Span::styled(" dja monitor ", Style::default().fg(Color::Cyan).bold()),
        Span::raw("  "),
        Span::styled(status, if state.connected { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Red) }),
        Span::raw("  uptime: "),
        Span::styled(uptime, Style::default().fg(Color::Cyan)),
        Span::raw("  cache: "),
        Span::styled(entries, Style::default().fg(Color::Cyan)),
        Span::raw(" entries"),
    ]);

    frame.render_widget(Paragraph::new(header), area);
}

fn render_stats(frame: &mut Frame, area: Rect, state: &MonitorState) {
    let block = Block::default().borders(Borders::ALL).title(" Session Stats ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(inner);

    if let Some(stats) = &state.stats {
        // Column 1: Requests
        let requests = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" HIT:  ", Style::default().fg(Color::Green)),
                Span::styled(stats.hits.to_string(), Style::default().fg(Color::Green).bold()),
            ]),
            Line::from(vec![
                Span::styled(" MISS: ", Style::default().fg(Color::Yellow)),
                Span::styled(stats.misses.to_string(), Style::default().fg(Color::Yellow).bold()),
            ]),
            Line::from(vec![
                Span::styled(" SKIP: ", Style::default().fg(Color::DarkGray)),
                Span::styled(stats.skips.to_string(), Style::default().fg(Color::DarkGray)),
            ]),
        ]);
        frame.render_widget(requests, cols[0]);

        // Column 2: Hit Rate
        let total = stats.hits + stats.misses;
        let hit_rate = if total > 0 { stats.hits as f64 / total as f64 } else { 0.0 };
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray))
            .ratio(hit_rate)
            .label(format!("{:.1}%", hit_rate * 100.0));
        let rate_area = Rect { y: cols[1].y + 1, height: 1, ..cols[1] };
        frame.render_widget(gauge, rate_area);

        let rate_label = Paragraph::new(Line::from(
            Span::styled(" Hit Rate", Style::default().fg(Color::Cyan))
        ));
        frame.render_widget(rate_label, cols[1]);

        // Column 3: Savings
        let savings = Paragraph::new(vec![
            Line::from(vec![
                Span::raw(" Time:   "),
                Span::styled(format_duration(stats.time_saved_ms / 1000), Style::default().fg(Color::Cyan).bold()),
                Span::raw(" saved"),
            ]),
            Line::from(vec![
                Span::raw(" Tokens: "),
                Span::styled(format_tokens(stats.estimated_tokens_saved), Style::default().fg(Color::Cyan).bold()),
                Span::raw(" est."),
            ]),
            Line::from(vec![
                Span::raw(" Cost:   "),
                Span::styled(format!("${:.2}", stats.estimated_cost_saved_usd), Style::default().fg(Color::Cyan).bold()),
                Span::raw(" est."),
            ]),
        ]);
        frame.render_widget(savings, cols[2]);
    } else {
        let loading = Paragraph::new(Span::styled(" Connecting...", Style::default().fg(Color::DarkGray)));
        frame.render_widget(loading, inner);
    }
}

fn render_live_feed(frame: &mut Frame, area: Rect, state: &MonitorState) {
    let block = Block::default().borders(Borders::ALL).title(" Live Requests ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.events.is_empty() {
        let msg = Paragraph::new(Span::styled(
            " Waiting for requests...",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(msg, inner);
        return;
    }

    let rows: Vec<Row> = state.events.iter().take(inner.height as usize).map(|e| {
        let (type_style, type_label) = match e.event_type.as_str() {
            "hit" => (Style::default().fg(Color::Green), " HIT "),
            "miss" => (Style::default().fg(Color::Yellow), " MISS"),
            "skip" => (Style::default().fg(Color::DarkGray), " SKIP"),
            "error" => (Style::default().fg(Color::Red), " ERR "),
            _ => (Style::default(), " ??? "),
        };

        let latency = e.latency_ms
            .map(|ms| format!("{ms:>5}ms"))
            .unwrap_or_else(|| "    --".into());

        let snippet = e.prompt_snippet.as_deref().unwrap_or("(not eligible)");
        let snippet_display: String = snippet.chars().take(50).collect();

        let model = e.model.as_deref()
            .map(|m| {
                // Extract short model name: "claude-opus-4-6" → "opus"
                if m.contains("opus") { "opus" }
                else if m.contains("sonnet") { "sonnet" }
                else if m.contains("haiku") { "haiku" }
                else { m }
            })
            .unwrap_or("--");

        Row::new(vec![
            Cell::from(Span::styled(type_label, type_style)),
            Cell::from(Span::styled(latency, type_style)),
            Cell::from(Span::styled(format!(" {snippet_display}"), type_style)),
            Cell::from(Span::styled(format!(" {model}"), Style::default().fg(Color::DarkGray))),
        ])
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),   // type
            Constraint::Length(8),   // latency
            Constraint::Min(20),     // prompt
            Constraint::Length(8),   // model
        ],
    );

    frame.render_widget(table, inner);
}

fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("~{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("~{:.0}K", tokens as f64 / 1_000.0)
    } else {
        format!("~{tokens}")
    }
}
```

- [ ] **Step 2: Register the CLI command**

Add to `src/cli/mod.rs`:

```rust
pub mod monitor;
```

In `src/main.rs`, add the `Monitor` variant to the `Commands` enum:

```rust
/// Live monitor dashboard
Monitor,
```

And add the match arm:

```rust
Commands::Monitor => dja::cli::monitor::run().await?,
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 4: Commit**

```bash
git add src/cli/monitor.rs src/cli/mod.rs src/main.rs
git commit -m "feat(monitor): add dja monitor TUI command"
```

---

### Task 6: Integration test and cleanup

**Files:**
- Modify: `src/proxy/handler.rs` (remove temporary debug logging)

- [ ] **Step 1: Remove temporary debug logging from handler**

In `src/proxy/handler.rs`, remove the large `// Log request structure for debugging` block (lines 73-129). This diagnostic logging was added during development and is now replaced by the proper metrics system. The `tracing::info!` lines for HIT/MISS/SKIP remain.

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 3: Build release and install**

Run: `cargo install --path .`
Expected: installs successfully

- [ ] **Step 4: Manual smoke test**

1. `dja stop && dja start`
2. In a new terminal: `dja monitor` — should show the TUI with "Waiting for requests..."
3. In another terminal: `ANTHROPIC_BASE_URL=http://127.0.0.1:9842 claude` — ask a question
4. Monitor should show MISS event in green, then on repeat question show HIT event
5. Stats panel should update with hit/miss counts and savings
6. Press `q` to exit monitor

- [ ] **Step 5: Commit cleanup**

```bash
git add src/proxy/handler.rs
git commit -m "refactor: remove temporary debug logging, replaced by monitor metrics"
```
