<p align="center">
  <img src="icon.webp" width="180" alt="dja logo" />
</p>

# dja

A semantic cache proxy for AI coding tools. dja sits between your coding assistant (e.g., Claude Code) and the Anthropic API, transparently caching responses. When the same (or semantically similar) prompt is sent again, dja returns the cached response instantly, saving time and API costs.

## Installation

### Quick Install (Linux/macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/tomermesser/dja/main/install.sh | sh
```

> Installs to `~/.local/bin` and adds it to your PATH automatically.

### Cargo

```bash
cargo install --git https://github.com/tomermesser/dja
```

### Pre-built Binaries

Download from [releases](https://github.com/tomermesser/dja/releases):
- macOS: `dja-aarch64-apple-darwin` (Apple Silicon)
- Linux: `dja-x86_64-unknown-linux-gnu`

### Verify Installation

```bash
dja --version   # Should show current version
dja verify      # Check installation health
```

## Quick Start

```bash
dja init          # Downloads embedding model, creates config + database
dja start         # Start the proxy daemon
```

That's it. `dja init` sets up shell integration so `ANTHROPIC_BASE_URL` is automatically configured whenever dja is running. Open a new terminal (or `source ~/.zshrc`) and your AI tool will route through dja.

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

## P2P Cache Sharing

dja can share its cache with other machines on the same network. When a prompt misses your local cache, dja checks connected peers before hitting the upstream API.

### Setup

P2P is enabled by default when you run `dja init`. During setup you'll be prompted for a display name, and dja will auto-detect your local IP address.

Both machines need dja installed and running:

```bash
# Machine A
curl -fsSL https://raw.githubusercontent.com/tomermesser/dja/main/install.sh | sh
dja init
dja start
```

```bash
# Machine B
curl -fsSL https://raw.githubusercontent.com/tomermesser/dja/main/install.sh | sh
dja init
dja start
```

### Connecting Peers

On Machine A, generate an invite code:

```bash
dja p2p invite
```

This prints a base64 invite code. Copy it and run on Machine B:

```bash
dja p2p add <invite-code>
```

The handshake is mutual — both machines are now connected. If the peer is unreachable at the time, the friend is saved as pending and the handshake completes when you re-run the command.

### Managing Peers

```bash
dja p2p friends       # List all connected peers
dja p2p status        # Show P2P status (peer ID, address, friend count)
dja p2p remove <id>   # Remove a peer
```

### P2P Configuration

The `[p2p]` section in `~/.config/dja/config.toml`:

```toml
[p2p]
enabled = true                  # Enable/disable P2P
peer_id = "dja_6ff7bbdc"        # Auto-generated unique ID
display_name = "MacBook Pro"    # Human-readable name
public_addr = "10.0.1.5:9843"  # Address other peers use to reach you
listen_port = 9843              # P2P API port
```

> If you're using Tailscale or another VPN, update `public_addr` to your Tailscale IP.

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
| `dja p2p invite` | Generate an invite code for this node |
| `dja p2p add <code>` | Add a peer by invite code |
| `dja p2p remove <id>` | Remove a peer |
| `dja p2p friends` | List all connected peers |
| `dja p2p status` | Show P2P status |
| `dja uninstall [--force]` | Completely remove dja (binary, data, config, shell hooks) |

## Uninstall

To completely remove dja from your system:

```bash
dja uninstall
```

This removes the binary, cache database, config, embedding model, and shell integration. Use `--force` to skip the confirmation prompt.
