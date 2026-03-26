use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

/// A single request event for the SSE stream.
#[derive(Debug, Clone, Serialize)]
pub struct RequestEvent {
    pub event_type: String,
    pub latency_ms: Option<u64>,
    pub prompt_snippet: Option<String>,
    pub model: Option<String>,
    pub similarity: Option<f32>,
    pub cache_id: Option<i64>,
    pub body_size: usize,
    pub response_size: Option<usize>,
    pub timestamp: String,
}

/// Atomic session-level counters for the proxy.
pub struct SessionStats {
    pub started_at: Instant,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub skips: AtomicU64,
    pub errors: AtomicU64,
    pub time_saved_ms: AtomicU64,
    pub upstream_bytes_saved: AtomicU64,
    pub response_bytes_saved: AtomicU64,
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

    /// Record a cache hit. Computes estimated time saved using average miss
    /// latency (defaults to 1000ms if no misses recorded yet).
    pub fn record_hit(&self, _latency_ms: u64, body_size: usize, response_size: usize) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.upstream_bytes_saved
            .fetch_add(body_size as u64, Ordering::Relaxed);
        self.response_bytes_saved
            .fetch_add(response_size as u64, Ordering::Relaxed);

        let misses = self.misses.load(Ordering::Relaxed);
        let avg_miss_latency = if misses > 0 {
            self.total_miss_latency_ms.load(Ordering::Relaxed) / misses
        } else {
            1000
        };
        self.time_saved_ms
            .fetch_add(avg_miss_latency, Ordering::Relaxed);
    }

    /// Record a cache miss.
    pub fn record_miss(&self, latency_ms: u64) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        self.total_miss_latency_ms
            .fetch_add(latency_ms, Ordering::Relaxed);
    }

    /// Record a skipped request (not eligible for caching).
    pub fn record_skip(&self) {
        self.skips.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an error.
    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Seconds since the session started.
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Rough estimate: (input bytes saved + output bytes saved) / 4.
    pub fn estimated_tokens_saved(&self) -> u64 {
        let input = self.upstream_bytes_saved.load(Ordering::Relaxed);
        let output = self.response_bytes_saved.load(Ordering::Relaxed);
        (input + output) / 4
    }

    /// Rough cost estimate: $10/1M input tokens, $30/1M output tokens.
    pub fn estimated_cost_saved_usd(&self) -> f64 {
        let input_tokens = self.upstream_bytes_saved.load(Ordering::Relaxed) as f64 / 4.0;
        let output_tokens = self.response_bytes_saved.load(Ordering::Relaxed) as f64 / 4.0;
        (input_tokens * 10.0 + output_tokens * 30.0) / 1_000_000.0
    }
}

/// Create a broadcast channel for request events.
pub fn event_channel() -> (broadcast::Sender<RequestEvent>, broadcast::Receiver<RequestEvent>) {
    let (tx, rx) = broadcast::channel(256);
    (tx, rx)
}

/// Current time as a simple ISO-8601-ish Unix timestamp string.
pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    secs.to_string()
}
