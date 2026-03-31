# dja - Semantic Cache Proxy for AI Coding Tools

## What is dja?

dja is a local HTTP proxy that sits between AI coding tools (e.g., Claude Code) and the Anthropic API. It transparently caches responses using semantic similarity — when a similar prompt is sent again, dja returns the cached response instantly, saving time and API costs.

## Architecture

```
Client (Claude Code) -> dja proxy (127.0.0.1:9842) -> Anthropic API
                              |
                    +---------+---------+
                    |         |         |
               Embedding   Cache    Metrics
              (ONNX RT)  (libSQL)  (in-memory)
```

### Core Flow (POST /v1/messages)

1. **Eligibility check** — parse request JSON, extract last user message (skipping `<system-reminder>` blocks), check for tool blocks
2. **Embed** — generate 384-dim vector via all-MiniLM-L6-v2 (ONNX Runtime, local inference)
3. **Lookup** — cosine similarity search in libSQL vector index (top-k=10, filter by model and optionally system_hash)
4. **Hit** — return cached response bytes (SSE or JSON), marked with `[cached]`
5. **Miss** — forward to upstream, tee-stream response to client + buffer for cache storage

Non-messages requests are forwarded transparently.

### Module Layout

```
src/
  main.rs              # CLI entry point (clap)
  lib.rs               # Module re-exports
  config.rs            # Config struct, paths (~/.config/dja/, ~/.local/share/dja/)
  proxy/
    server.rs          # Axum server setup, AppState (shared state)
    handler.rs         # Main proxy handler with cache interception + request coalescing
    eligibility.rs     # Request parsing, eligibility checks, system-reminder filtering
    inflight.rs        # Request coalescing (singleflight) — InflightMap, coalesce key
    forward.rs         # Upstream forwarding (3 variants: passthrough, raw, with_body)
    stream.rs          # SSE parsing, [cached] marker injection, tee-stream, replay
    metrics.rs         # SessionStats (atomic counters), RequestEvent, broadcast channel
    internal.rs        # GET /internal/stats (JSON), GET /internal/events (SSE)
  cache/
    db.rs              # CacheDb (libSQL), schema, EMBEDDING_DIM=384
    lookup.rs          # Vector similarity lookup with hit tracking
    store.rs           # Cache entry storage, export/import, hits_by_day
    eviction.rs        # TTL and LRU eviction
    tests.rs           # Cache integration tests
  embedding/
    model.rs           # EmbeddingModel — ONNX inference, mean pooling, L2 normalize
    tokenizer.rs       # HuggingFace tokenizer wrapper
    download.rs        # Model download from HuggingFace Hub
  cli/
    start.rs           # Daemon start (PID file, SIGTERM handler, log setup)
    monitor.rs         # Live TUI dashboard (ratatui) — stats + SSE event feed
    init.rs            # First-time setup (config, model download, DB creation)
    stats.rs, clear.rs, config_cmd.rs, test_cmd.rs, log.rs, verify.rs, export.rs, import.rs
tests/
  proxy_test.rs        # Integration tests for the proxy
```

### Key Dependencies

- **axum** — HTTP server/router
- **reqwest** — Upstream HTTP client
- **ort** — ONNX Runtime for embedding inference
- **libsql** — SQLite-compatible DB with vector search (cosine similarity)
- **ratatui + crossterm** — TUI monitor dashboard
- **clap** — CLI argument parsing
- **tokio** — Async runtime

### Data Storage

- Config: `~/.config/dja/config.toml`
- Data: `~/.local/share/dja/` (cache.db, dja.pid, dja.log, models/)
- Embedding model: `~/.local/share/dja/models/all-MiniLM-L6-v2/`

## Build and Test

```bash
cargo build                    # Build
cargo test                     # Run all tests (some embedding tests need model downloaded)
cargo run -- start             # Run the proxy
cargo run -- monitor           # Open TUI dashboard
```

Rust edition: 2024. Requires nightly or recent stable for `let` chains in pattern matching.

## Key Design Decisions

- **Last-user-message only**: Cache key is the last user message text, not the full conversation. The high similarity threshold (0.95) prevents false matches.
- **System-reminder filtering**: Claude Code injects dynamic `<system-reminder>` blocks as text content — these are stripped from cache key extraction to avoid cache poisoning.
- **Multi-turn caching** (default on): Multi-turn conversations are eligible if the last message is from the user. Tool-only messages (no text blocks) are skipped.
- **match_system_prompt** (default off): When disabled, system prompt hash is ignored in lookups. Best for Claude Code which has dynamic system prompts.
- **Streaming tee**: SSE responses are tee-streamed to the client and buffered simultaneously for cache storage.
- **`[cached]` marker**: Injected into the first text_delta (SSE) or first text content block (JSON) of cached responses.
- **Request coalescing** (default on): When identical requests arrive concurrently (e.g., from Claude Code retries or parallel subagents), only the first is forwarded to upstream. Waiters are served from cache once the leader completes. Controlled by `request_coalescing` config field.
- **auto_cache_control** (default on): Injects Anthropic `cache_control` breakpoints on system prompt and last tool definition, enabling server-side prompt caching. Respects the 4-breakpoint limit.

## CLI Commands

`dja init`, `start`, `stop`, `status`, `stats`, `clear`, `config`, `test`, `export`, `import`, `log`, `verify`, `monitor`
