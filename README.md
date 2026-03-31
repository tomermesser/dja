<p align="center">
  <img src="icon.webp" width="180" alt="dja logo" />
</p>

# dja

A semantic cache proxy for AI coding tools. dja sits between your coding assistant (e.g., Claude Code) and the Anthropic API, transparently caching responses. When the same (or semantically similar) prompt is sent again, dja returns the cached response instantly, saving time and API costs.

## Quick Install

```bash
cargo install dja
```

## Quick Setup

```bash
dja init          # Downloads embedding model, creates config + database
dja start         # Start the proxy daemon
```

Then point your AI tool at dja:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:9842
```

Or use `dja init --global` to add this to your shell profile automatically.

## How It Works

1. **Intercept** -- dja proxies all requests to the Anthropic API on `127.0.0.1:9842`.
2. **Embed** -- For eligible requests, dja generates a 384-dimensional embedding of the last user message using a local ONNX model (all-MiniLM-L6-v2). Both single-turn and multi-turn conversations are supported (multi-turn is enabled by default).
3. **Lookup** -- The embedding is compared against cached entries using cosine similarity via libSQL's vector index. If a match is found above the similarity threshold (default 0.95), the cached response is returned.
4. **Store** -- On cache miss, the request is forwarded to the upstream API. The response is streamed back to the client and simultaneously buffered for caching.

Cached responses are marked with `[cached]` at the start of the response text so you can see when a cache hit occurs.

**Request coalescing**: When identical requests arrive concurrently (e.g., from retries or parallel agents), only one is forwarded upstream. Other waiters are served from cache once the first completes.

**Prompt cache optimization**: dja automatically injects Anthropic `cache_control` breakpoints on the system prompt and tool definitions, enabling server-side prompt caching for additional cost savings.

## Configuration

Config lives at `~/.config/dja/config.toml`:

```toml
port = 9842                              # Proxy listen port
upstream = "https://api.anthropic.com"   # Upstream API
threshold = 0.95                         # Cosine similarity threshold
ttl = "30d"                              # Cache entry TTL
max_entries = 10000                      # Maximum cache entries
max_response_size = 1048576              # Max response size to cache (bytes, default 1MB)
log_level = "info"                       # Log level
match_system_prompt = false              # Require system prompt match (false for Claude Code)
multi_turn_caching = true                # Cache multi-turn conversations
auto_cache_control = true                # Auto-inject Anthropic prompt caching breakpoints
request_coalescing = true                # Deduplicate identical in-flight requests
```

## CLI Reference

| Command | Description |
|---------|-------------|
| `dja init [--global]` | Initialize config, download model, create database |
| `dja start` | Start the proxy daemon |
| `dja stop` | Stop the proxy daemon |
| `dja status` | Show daemon status |
| `dja stats [--json] [--graph]` | Show cache statistics or hit graph |
| `dja clear [--older-than 30d]` | Clear cache entries |
| `dja config [key] [value]` | View or modify configuration |
| `dja test "prompt"` | Test embedding and cache lookup |
| `dja export` | Export cache as JSON to stdout |
| `dja import <file>` | Import cache from JSON file |
| `dja log` | Show recent log output |
| `dja verify` | Verify installation health |
| `dja monitor` | Open live TUI dashboard |
