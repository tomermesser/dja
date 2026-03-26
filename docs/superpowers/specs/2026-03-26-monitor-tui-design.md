# dja monitor — Terminal TUI Dashboard

## Purpose

Real-time terminal dashboard for observing dja's caching behavior. Shows live request flow, aggregate session stats, and estimated savings. Helps debug caching behavior and measure dja's value.

## Layout

```
┌──────────────────────────────────────────────────────────┐
│ dja monitor              uptime: 2h 14m   cache: 847    │
├──────────────┬──────────────┬────────────────────────────┤
│  Requests    │  Hit Rate    │  Savings                   │
│  HIT:   142  │  ████████░░  │  Time: 3m 22s saved        │
│  MISS:   31  │  82.1%       │  Tokens: ~1.2M est.        │
│  SKIP:   18  │              │  Cost: ~$3.60 est.         │
├──────────────┴──────────────┴────────────────────────────┤
│ Live Requests                                             │
│ 21:52:04 HIT  3ms  "which fruit is best?"          opus  │
│ 21:52:04 HIT  2ms  "which fruit is best?"          opus  │
│ 21:52:01 MISS 934ms "what's new in rust?"          opus  │
│ 21:51:58 SKIP  --  (not eligible)                  opus  │
│ 21:51:45 HIT  4ms  "explain borrow checker"       haiku  │
│ ...                                                       │
└──────────────────────────────────────────────────────────┘
```

**Top bar**: daemon uptime, total cache entries.

**Stats panel** (3 columns):
- Requests: HIT / MISS / SKIP counts
- Hit Rate: visual bar + percentage
- Savings: time saved, estimated tokens saved, estimated cost saved

**Live feed**: scrolling list of recent requests, most recent on top, color-coded by result type.

## Color Scheme

| Element | Color | Meaning |
|---------|-------|---------|
| HIT label + row | Green | Fast cached response |
| MISS label + row | Yellow | Forwarded to upstream, stored |
| SKIP label + row | Dim gray | Not eligible for caching |
| ERROR label + row | Red | Embedding/lookup failure |
| Stats numbers, headers | Cyan | Key metrics |
| Default text | White | Everything else |

The hit rate bar uses green for filled segments and dark gray for empty segments.

## Architecture

### Daemon Side

Two new internal endpoints on the existing Axum HTTP server:

**`GET /internal/stats`** — JSON response:
```json
{
  "hits": 142,
  "misses": 31,
  "skips": 18,
  "errors": 0,
  "time_saved_ms": 202400,
  "estimated_tokens_saved": 1200000,
  "estimated_cost_saved_usd": 3.60,
  "uptime_secs": 8040,
  "cache_entry_count": 847
}
```

**`GET /internal/events`** — SSE stream, one event per request:
```
event: request
data: {"type":"hit","latency_ms":3,"prompt_snippet":"which fruit is best?","model":"claude-opus-4-6","similarity":1.0,"cache_id":75,"timestamp":"2026-03-26T21:52:04Z"}

event: request
data: {"type":"miss","latency_ms":934,"prompt_snippet":"what's new in rust?","model":"claude-opus-4-6","timestamp":"2026-03-26T21:52:01Z"}

event: request
data: {"type":"skip","latency_ms":null,"prompt_snippet":null,"model":"claude-opus-4-6","timestamp":"2026-03-26T21:51:58Z"}
```

### Session Stats (`SessionStats`)

In-memory struct with atomic counters, shared via `Arc` in `AppState`. Resets when daemon restarts. No DB writes.

```rust
pub struct SessionStats {
    pub started_at: Instant,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub skips: AtomicU64,
    pub errors: AtomicU64,
    pub time_saved_ms: AtomicU64,      // sum of (estimated_upstream_latency - cache_latency) per HIT
    pub upstream_bytes_saved: AtomicU64, // sum of request body sizes for HITs
}
```

For events, the daemon maintains a broadcast channel (`tokio::sync::broadcast`). Each request result (HIT/MISS/SKIP/ERROR) is sent into the channel. The SSE endpoint subscribes and forwards events to connected monitors.

### Monitor Side (`dja monitor`)

CLI command that:
1. Connects to `http://127.0.0.1:{port}/internal/events` for live request feed
2. Polls `http://127.0.0.1:{port}/internal/stats` every 2 seconds for stats panel
3. Renders TUI with `ratatui` + `crossterm` backend
4. Exits on `q` or Ctrl+C

The monitor is a read-only client. Multiple monitors can connect simultaneously.

### Token/Cost Estimation

Rough heuristic for estimating saved tokens per HIT:
- Input tokens: `request_body_size / 4` (approximate 4 chars per token for English)
- Output tokens: `response_size / 4`
- Cost per HIT saved: `input_tokens * input_price + output_tokens * output_price`

Model pricing lookup table (hardcoded, updated manually):

| Model family | Input (per 1M) | Output (per 1M) |
|-------------|----------------|-----------------|
| opus        | $15.00         | $75.00          |
| sonnet      | $3.00          | $15.00          |
| haiku       | $0.80          | $4.00           |

Model family extracted from model name string (e.g., `claude-opus-4-6` → `opus`).

### Time Saved Estimation

For each HIT, time saved = `average_miss_latency - hit_latency`. The average miss latency is computed as a rolling average from observed MISS events in the current session. If no misses observed yet, use a default of 1000ms.

## Files

| File | Purpose |
|------|---------|
| `src/proxy/metrics.rs` | `SessionStats` struct, `RequestEvent` struct, broadcast channel setup |
| `src/proxy/server.rs` | Wire `/internal/stats` and `/internal/events` routes, add `SessionStats` + broadcast sender to `AppState` |
| `src/proxy/handler.rs` | Record events into `SessionStats` and broadcast channel on each request |
| `src/cli/monitor.rs` | TUI rendering with ratatui, SSE client, stats polling |
| `src/cli/mod.rs` | Register `monitor` subcommand |

## Dependencies

New crate dependencies:

| Crate | Purpose |
|-------|---------|
| `ratatui` | Terminal UI framework |
| `crossterm` | Terminal backend for ratatui |

No new daemon dependencies — Axum already supports SSE via `axum::response::sse`, and `tokio::sync::broadcast` is part of tokio.

## Interaction

- `dja monitor` — starts the TUI dashboard
- `q` or `Ctrl+C` — exits
- No other keybindings in v1 (future: pause/filter/scroll)

## Edge Cases

- **Daemon not running**: `dja monitor` prints "dja is not running. Start it with `dja start`." and exits.
- **Connection lost**: TUI shows "Connection lost — reconnecting..." in the live feed area, retries every 2 seconds.
- **No events yet**: Live feed shows "Waiting for requests..." in dim gray.
- **Terminal too small**: Gracefully degrade — hide stats panel if width < 60, show minimal layout.

## Out of Scope (v1)

- Scrolling/filtering the live feed
- Historical data (use `dja stats` for that)
- Exporting monitor data
- Custom color themes
- Remote monitoring (only localhost)
