# Auto-Inject Cache-Control Headers

**Date:** 2026-03-28
**Status:** Design approved, pending implementation

## Summary

dja automatically injects Anthropic `cache_control` breakpoints into forwarded API requests, enabling prompt caching on the Anthropic side. This complements dja's semantic response caching — semantic cache handles repeated similar queries, while prompt caching reduces cost on fresh queries by caching the stable prefix (system prompt, tools).

The key positioning: dja works WITH Anthropic's prompt caching, not against it. Context Gateway's compression breaks prompt cache prefixes; dja's injection optimizes them.

## Anthropic Prompt Caching Primer

Anthropic allows up to **4 `cache_control` breakpoints** per request. Each breakpoint marks a position in the request where the API caches everything preceding it. On subsequent requests with the same prefix, cached tokens cost 10% of normal input pricing.

Breakpoints are placed as `"cache_control": {"type": "ephemeral"}` on content blocks within `system`, `tools`, and `messages`.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| What to mark | System prompt + tools | Covers the vast majority of stable, cacheable tokens with minimal complexity |
| When to inject | All forwarded requests | Cache hits never reach the API, so injection only matters on forwards. Apply universally for simplicity |
| Existing breakpoints | Merge intelligently | Only inject on blocks that don't already have `cache_control`. Respect the 4-breakpoint limit: count existing, use remaining slots, inject nothing if all 4 are used |
| Default state | On by default | Matches dja's zero-config philosophy. Disable with `auto_cache_control = false` |
| Pipeline position | New middleware module | Dedicated `cache_control.rs`, called from handler. Keeps injection logic isolated from forwarding |

## Architecture

### New Module: `src/proxy/cache_control.rs`

Single public function:

```rust
pub fn inject_cache_control(body: &[u8]) -> Option<Bytes>
```

- Takes raw request body bytes
- Returns `Some(modified_bytes)` if breakpoints were injected
- Returns `None` if no injection was needed (no system/tools, all slots used, blocks already marked)
- The module does one thing: inject breakpoints. It doesn't know about config, caching, or why it was called

### Injection Logic

**Step 1 — Parse and count existing breakpoints:**
- Parse body as `serde_json::Value`
- Scan `system`, `tools`, and `messages` for existing `"cache_control"` keys
- If `existing_count >= 4`, return `None` (no slots available)
- `remaining_slots = 4 - existing_count`

**Step 2 — Inject with priority order:**
1. **System prompt** (priority 1, highest token savings):
   - If `system` is a string: convert to array form `[{"type": "text", "text": "...", "cache_control": {"type": "ephemeral"}}]`
   - If `system` is an array: add `cache_control` to the **last** text block (Anthropic caches everything up to the breakpoint)
   - Skip if system already has `cache_control` or no remaining slots
   - Decrement `remaining_slots`
2. **Tools array** (priority 2):
   - Add `cache_control` to the **last** tool definition in the array
   - Skip if tools already marked or no remaining slots

dja uses at most 2 of the 4 slots, leaving room for the client's own breakpoints.

**Step 3 — Serialize and return:**
- If any injection happened, re-serialize to bytes and return `Some(Bytes)`
- If nothing was injected, return `None` (original bytes forwarded unchanged)

### Handler Integration

**Private helper in `handler.rs`:**

```rust
fn maybe_inject_cache_control(body: &[u8], config: &Config) -> Bytes {
    if config.auto_cache_control {
        cache_control::inject_cache_control(body)
            .unwrap_or_else(|| Bytes::copy_from_slice(body))
    } else {
        Bytes::copy_from_slice(body)
    }
}
```

**Called in two places:**

1. **Cache skip path** — ineligible requests still go to the API, still benefit from prompt caching
2. **Cache miss path** — right after cache lookup returns `None`, before forwarding

Both sites include a comment: injection is safe here because the cache key was already extracted upstream in `check_eligibility`. The ordering guarantee — eligibility -> embed -> lookup -> inject -> forward — ensures cache key extraction and body mutation never interfere.

**`parsed.full_body` contract:** `ParsedRequest.full_body` stores the original bytes for cache key purposes. After injection, the *modified* bytes go to the API, but semantic cache storage and lookup use the original user message text. These paths are independent by design.

### Request Flow

```
POST /v1/messages arrives
        |
  parse body bytes
        |
  check_eligibility() -----> INELIGIBLE -----> maybe_inject_cache_control()
        |                                              |
     ELIGIBLE                                   forward_request()
        |
  embed user message
        |
  cache lookup
        |
    +---+---+
    |       |
   HIT    MISS
    |       |
  return   maybe_inject_cache_control()
  cached       |
  response  forward (streaming or buffered)
            + store in semantic cache
```

### Config Addition

In `config.rs`, add to `Config` struct:

```rust
/// Whether to auto-inject Anthropic cache_control breakpoints on forwarded requests.
/// Default true.
pub auto_cache_control: bool,
```

Default: `true`. Toggled in `config.toml` with `auto_cache_control = false`.

## Observability

**Debug logging only:**

```rust
tracing::debug!(
    injected_system = injected_system,
    injected_tools = injected_tools,
    existing_breakpoints = existing_count,
    remaining_slots = remaining_slots,
    "cache_control injection"
);
```

**Explicitly not added:**
- No new fields on `RequestEvent`
- No new counters on `SessionStats`
- No `[cache-control-injected]` marker in responses

**Rationale:** You cannot attribute prompt cache savings to dja's injection. If Anthropic returns `cache_read_input_tokens: 50000`, you don't know if that's because dja injected breakpoints or because the client already had them. The debug log is honest — it records what dja *did*, not what dja *achieved*. Anthropic's response headers record what was achieved. Keep those separate. If prompt cache analytics are added later, they should read from response headers, not from injection tracking.

## Edge Cases

| Case | Behavior |
|------|----------|
| No `system` and no `tools` | Return `None`, forward original bytes |
| Client used all 4 breakpoint slots | Return `None`, forward original bytes |
| Client used 3 slots | Inject on 1 block (system preferred over tools) |
| `system` is a plain string | Convert to array form with `cache_control` (in forwarded copy only — original bytes unchanged) |
| `system` is already an array with `cache_control` on last block | Skip system, try tools |
| Request is not valid JSON | Return `None`, forward original bytes |
| Non-messages request (GET, etc.) | Never reaches injection — handled by passthrough path |

## Testing Strategy

Unit tests in `cache_control.rs`:

1. **Basic injection** — system string gets converted to array with `cache_control`
2. **System array injection** — `cache_control` added to last text block
3. **Tools injection** — `cache_control` added to last tool
4. **Both injected** — system + tools when both present and unmarked
5. **Existing breakpoints respected** — client's markers preserved, count enforced
6. **4-slot limit** — returns `None` when all slots used
7. **3 slots used** — injects exactly 1 (system over tools)
8. **No system, no tools** — returns `None`
9. **Invalid JSON** — returns `None`
10. **Already fully marked** — system and tools both have `cache_control`, returns `None`
11. **String system conversion** — verify the string-to-array transformation preserves text content

Integration test: verify that a request forwarded through dja with `auto_cache_control = true` contains the expected `cache_control` fields in the body received by a mock upstream.

## Files Changed

| File | Change |
|------|--------|
| `src/proxy/cache_control.rs` | **New** — injection logic + unit tests |
| `src/proxy/mod.rs` | Add `pub mod cache_control;` |
| `src/proxy/handler.rs` | Add `maybe_inject_cache_control` helper, call in skip + miss paths |
| `src/config.rs` | Add `auto_cache_control: bool` field (default `true`) |
| `tests/proxy_test.rs` | Integration test for cache-control injection |
