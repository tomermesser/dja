# Auto-Inject Cache-Control Headers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Automatically inject Anthropic `cache_control` breakpoints on system prompt and tools in every request forwarded to the API, enabling Anthropic-side prompt caching with zero client configuration.

**Architecture:** A new `cache_control.rs` module exposes a single pure function `inject_cache_control(body: &[u8]) -> Option<Bytes>` that parses, mutates, and re-serializes the request JSON. The handler wraps the config gate in a `maybe_inject_cache_control` helper and calls it in two places: the ineligible-skip path and the cache-miss path. The semantic cache key is extracted before injection runs, so the two systems are fully independent.

**Tech Stack:** `serde_json` (already in Cargo.toml), `bytes::Bytes` (already in Cargo.toml), `tracing` (already in Cargo.toml).

---

## File Map

| File | Action | What changes |
|------|--------|-------------|
| `src/config.rs` | Modify | Add `auto_cache_control: bool` field, default `true` |
| `src/proxy/cache_control.rs` | Create | Injection logic + all unit tests |
| `src/proxy/mod.rs` | Modify | Add `pub mod cache_control;` |
| `src/proxy/handler.rs` | Modify | Add `maybe_inject_cache_control` helper, call in skip + miss paths |
| `tests/proxy_test.rs` | Modify | Update `Config` struct literals + add injection integration test |

---

## Task 1: Add `auto_cache_control` to Config

**Files:**
- Modify: `src/config.rs`
- Modify: `tests/proxy_test.rs` (struct literal updates)

- [ ] **Step 1: Write the failing config tests**

Add these two tests at the bottom of the `#[cfg(test)]` block in `src/config.rs`, before the final `}`:

```rust
    #[test]
    fn test_default_auto_cache_control_is_true() {
        let config = Config::default();
        assert!(config.auto_cache_control);
    }

    #[test]
    fn test_parse_auto_cache_control_false() {
        let toml_str = r#"
            auto_cache_control = false
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.auto_cache_control);
    }
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test test_default_auto_cache_control_is_true test_parse_auto_cache_control_false 2>&1 | tail -20
```

Expected: compile error — `auto_cache_control` field does not exist.

- [ ] **Step 3: Add the field to Config**

In `src/config.rs`, add after `multi_turn_caching`:

```rust
    /// Whether to auto-inject Anthropic cache_control breakpoints on forwarded requests.
    /// Default true.
    pub auto_cache_control: bool,
```

And in the `Default` impl, add after `multi_turn_caching: true,`:

```rust
            auto_cache_control: true,
```

- [ ] **Step 4: Fix the Config struct literals in integration tests**

In `tests/proxy_test.rs`, both `Config { ... }` struct literals (around lines 124–134 and 230–240) need the new field. Add `auto_cache_control: true,` after `multi_turn_caching: true,` in each:

```rust
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
        auto_cache_control: true,   // <-- add this line
    };
```

Do this for both occurrences in the file.

- [ ] **Step 5: Run to verify tests pass**

```bash
cargo test test_default_auto_cache_control_is_true test_parse_auto_cache_control_false 2>&1 | tail -20
```

Expected: both PASS.

- [ ] **Step 6: Verify nothing else broke**

```bash
cargo test 2>&1 | tail -20
```

Expected: all existing tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/config.rs tests/proxy_test.rs
git commit -m "feat: add auto_cache_control config field (default true)"
```

---

## Task 2: Create `cache_control.rs` with injection logic

**Files:**
- Create: `src/proxy/cache_control.rs`
- Modify: `src/proxy/mod.rs`

### Step 1: Register the module

- [ ] **Add `pub mod cache_control;` to `src/proxy/mod.rs`**

The file currently reads:
```rust
pub mod eligibility;
pub mod forward;
pub mod handler;
pub mod internal;
pub mod metrics;
pub mod server;
pub mod stream;
```

Add `pub mod cache_control;` after `pub mod eligibility;`:

```rust
pub mod cache_control;
pub mod eligibility;
pub mod forward;
pub mod handler;
pub mod internal;
pub mod metrics;
pub mod server;
pub mod stream;
```

### Step 2: Write all unit tests first (TDD)

- [ ] **Create `src/proxy/cache_control.rs` with the tests and a stub**

```rust
use bytes::Bytes;

/// Inject Anthropic `cache_control` breakpoints into a request body.
///
/// Marks the system prompt and/or the last tool definition with
/// `"cache_control": {"type": "ephemeral"}`, enabling Anthropic-side prompt
/// caching. Respects the 4-breakpoint limit: counts existing markers and only
/// uses remaining slots.
///
/// Returns `Some(modified_bytes)` if any injection happened, `None` if the
/// body was left unchanged (no system/tools, all 4 slots used, already fully
/// marked, or invalid JSON).
pub fn inject_cache_control(body: &[u8]) -> Option<Bytes> {
    todo!("implement")
}

// ── Private helpers ──────────────────────────────────────────────────────────

fn count_cache_control_in_value(val: &serde_json::Value) -> usize {
    todo!("implement")
}

fn try_inject_system(system: &mut serde_json::Value) -> bool {
    todo!("implement")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn has_cache_control(val: &serde_json::Value) -> bool {
        val.get("cache_control").is_some()
    }

    // 1. System string → converted to array with cache_control on the block
    #[test]
    fn test_injects_system_string() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let system = json.get("system").unwrap();
        assert!(system.is_array(), "system should be converted to array");
        let blocks = system.as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "You are a helpful assistant.");
        assert!(has_cache_control(&blocks[0]), "last system block should have cache_control");
    }

    // 2. System array → cache_control added to last text block
    #[test]
    fn test_injects_system_array_last_block() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "First block"},
                {"type": "text", "text": "Last block"}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let blocks = json["system"].as_array().unwrap();
        assert!(!has_cache_control(&blocks[0]), "first block should NOT have cache_control");
        assert!(has_cache_control(&blocks[1]), "last block should have cache_control");
    }

    // 3. Tools → cache_control added to last tool
    #[test]
    fn test_injects_tools_last_tool() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "tools": [
                {"name": "tool_a", "description": "first tool", "input_schema": {"type": "object"}},
                {"name": "tool_b", "description": "second tool", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let tools = json["tools"].as_array().unwrap();
        assert!(!has_cache_control(&tools[0]), "first tool should NOT have cache_control");
        assert!(has_cache_control(&tools[1]), "last tool should have cache_control");
    }

    // 4. Both system + tools → both get injected (uses 2 of 4 slots)
    #[test]
    fn test_injects_both_system_and_tools() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are helpful.",
            "tools": [
                {"name": "search", "description": "search tool", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject both");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        // System should be converted and marked
        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[0]));

        // Tool should be marked
        let tools = json["tools"].as_array().unwrap();
        assert!(has_cache_control(&tools[0]));
    }

    // 5. Existing client breakpoints are preserved, not overwritten
    #[test]
    fn test_preserves_existing_client_breakpoints() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "Client marked this", "cache_control": {"type": "ephemeral"}}
            ],
            "tools": [
                {"name": "tool_a", "description": "tool", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should still inject on tools");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        // System: existing breakpoint preserved
        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[0]), "client's system breakpoint should be preserved");

        // Tools: dja uses the remaining slot
        let tools = json["tools"].as_array().unwrap();
        assert!(has_cache_control(&tools[0]), "dja should inject on tool");
    }

    // 6. All 4 slots used by client → return None
    #[test]
    fn test_no_injection_when_all_slots_used() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "block 1", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 2", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 3", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 4", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        assert!(inject_cache_control(&body).is_none(), "should return None when all 4 slots are used");
    }

    // 7. 3 slots used → only 1 injection (system preferred over tools)
    #[test]
    fn test_uses_exactly_one_remaining_slot() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "block 1", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 2", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 3", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 4"}
            ],
            "tools": [
                {"name": "tool_a", "description": "tool", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject exactly once");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        // System block 4 gets the remaining slot
        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[3]), "block 4 should get the last slot");

        // Tools should NOT be marked (no slots left)
        let tools = json["tools"].as_array().unwrap();
        assert!(!has_cache_control(&tools[0]), "tools should not be marked — no slots left");
    }

    // 8. No system, no tools → None
    #[test]
    fn test_returns_none_when_nothing_to_inject() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        assert!(inject_cache_control(&body).is_none());
    }

    // 9. Invalid JSON → None
    #[test]
    fn test_returns_none_for_invalid_json() {
        assert!(inject_cache_control(b"not json").is_none());
        assert!(inject_cache_control(b"").is_none());
        assert!(inject_cache_control(b"[]").is_none()); // array, not object
    }

    // 10. System and tools both already fully marked → None
    #[test]
    fn test_returns_none_when_already_fully_marked() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}
            ],
            "tools": [
                {"name": "t", "description": "tool", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        assert!(inject_cache_control(&body).is_none(), "nothing to inject when both are already marked");
    }

    // 11. String system conversion preserves the text content exactly
    #[test]
    fn test_string_system_conversion_preserves_text() {
        let original_text = "You are a pirate. Speak only in pirate English.";
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": original_text,
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let system = json["system"].as_array().unwrap();
        assert_eq!(system[0]["text"].as_str().unwrap(), original_text,
            "text content must be preserved exactly during string→array conversion");
        assert_eq!(system[0]["type"].as_str().unwrap(), "text");
    }
}
```

- [ ] **Step 3: Run tests to verify they all fail with the todo! stub**

```bash
cargo test --lib cache_control 2>&1 | tail -30
```

Expected: all 11 tests panic with "not yet implemented".

### Step 3: Implement the injection logic

- [ ] **Step 4: Replace the stub with the real implementation**

Replace the entire content of `src/proxy/cache_control.rs` with:

```rust
use bytes::Bytes;

/// Inject Anthropic `cache_control` breakpoints into a request body.
///
/// Marks the system prompt (priority 1) and the last tool definition
/// (priority 2) with `"cache_control": {"type": "ephemeral"}`, enabling
/// Anthropic-side prompt caching. Respects the 4-breakpoint limit: counts
/// existing markers first and only uses remaining slots.
///
/// Returns `Some(modified_bytes)` if any injection happened, `None` if the
/// body was left unchanged (no system/tools, all 4 slots used, already fully
/// marked, or invalid JSON).
pub fn inject_cache_control(body: &[u8]) -> Option<Bytes> {
    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = json.as_object_mut()?;

    let existing_count = count_existing_breakpoints(obj);
    if existing_count >= 4 {
        return None;
    }
    let mut remaining = 4 - existing_count;
    let mut injected_system = false;
    let mut injected_tools = false;

    // Priority 1: system prompt (largest stable block, biggest savings)
    if remaining > 0 {
        if let Some(system) = obj.get_mut("system") {
            if try_inject_system(system) {
                injected_system = true;
                remaining -= 1;
            }
        }
    }

    // Priority 2: last tool definition
    if remaining > 0 {
        if let Some(tools) = obj.get_mut("tools") {
            if let Some(arr) = tools.as_array_mut() {
                if let Some(last_tool) = arr.last_mut() {
                    if last_tool.get("cache_control").is_none() {
                        if let Some(tool_obj) = last_tool.as_object_mut() {
                            tool_obj.insert(
                                "cache_control".to_string(),
                                serde_json::json!({"type": "ephemeral"}),
                            );
                            injected_tools = true;
                        }
                    }
                }
            }
        }
    }

    if !injected_system && !injected_tools {
        return None;
    }

    tracing::debug!(
        injected_system = injected_system,
        injected_tools = injected_tools,
        existing_breakpoints = existing_count,
        remaining_slots = remaining,
        "cache_control injection"
    );

    serde_json::to_vec(&json).ok().map(Bytes::from)
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// Count how many `cache_control` breakpoints already exist across
/// system, tools, and messages fields.
fn count_existing_breakpoints(obj: &serde_json::Map<String, serde_json::Value>) -> usize {
    let mut count = 0;
    for field in &["system", "tools", "messages"] {
        if let Some(val) = obj.get(*field) {
            count += count_cache_control_in_value(val);
        }
    }
    count
}

/// Recursively count objects that contain a `cache_control` key.
fn count_cache_control_in_value(val: &serde_json::Value) -> usize {
    match val {
        serde_json::Value::Object(map) => {
            let self_count = if map.contains_key("cache_control") { 1 } else { 0 };
            // Don't recurse into the cache_control value itself
            let child_count: usize = map
                .iter()
                .filter(|(k, _)| k.as_str() != "cache_control")
                .map(|(_, v)| count_cache_control_in_value(v))
                .sum();
            self_count + child_count
        }
        serde_json::Value::Array(arr) => arr.iter().map(count_cache_control_in_value).sum(),
        _ => 0,
    }
}

/// Try to inject `cache_control` on the system field.
///
/// - String: converted to `[{"type": "text", "text": "...", "cache_control": {...}}]`
/// - Array: `cache_control` added to the last text block that doesn't already have one
///
/// Returns `true` if injection happened.
fn try_inject_system(system: &mut serde_json::Value) -> bool {
    match system {
        serde_json::Value::String(s) => {
            let text = s.clone();
            *system = serde_json::json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"}
            }]);
            true
        }
        serde_json::Value::Array(blocks) => {
            if let Some(last_text) = blocks.iter_mut().rev().find(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("text")
                    && b.get("cache_control").is_none()
            }) {
                if let Some(obj) = last_text.as_object_mut() {
                    obj.insert(
                        "cache_control".to_string(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn has_cache_control(val: &serde_json::Value) -> bool {
        val.get("cache_control").is_some()
    }

    // 1. System string → converted to array with cache_control on the block
    #[test]
    fn test_injects_system_string() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let system = json.get("system").unwrap();
        assert!(system.is_array(), "system should be converted to array");
        let blocks = system.as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "You are a helpful assistant.");
        assert!(has_cache_control(&blocks[0]), "last system block should have cache_control");
    }

    // 2. System array → cache_control added to last text block
    #[test]
    fn test_injects_system_array_last_block() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "First block"},
                {"type": "text", "text": "Last block"}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let blocks = json["system"].as_array().unwrap();
        assert!(!has_cache_control(&blocks[0]), "first block should NOT have cache_control");
        assert!(has_cache_control(&blocks[1]), "last block should have cache_control");
    }

    // 3. Tools → cache_control added to last tool
    #[test]
    fn test_injects_tools_last_tool() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "tools": [
                {"name": "tool_a", "description": "first tool", "input_schema": {"type": "object"}},
                {"name": "tool_b", "description": "second tool", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let tools = json["tools"].as_array().unwrap();
        assert!(!has_cache_control(&tools[0]), "first tool should NOT have cache_control");
        assert!(has_cache_control(&tools[1]), "last tool should have cache_control");
    }

    // 4. Both system + tools → both get injected (uses 2 of 4 slots)
    #[test]
    fn test_injects_both_system_and_tools() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are helpful.",
            "tools": [
                {"name": "search", "description": "search tool", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject both");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[0]));

        let tools = json["tools"].as_array().unwrap();
        assert!(has_cache_control(&tools[0]));
    }

    // 5. Existing client breakpoints are preserved, not overwritten
    #[test]
    fn test_preserves_existing_client_breakpoints() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "Client marked this", "cache_control": {"type": "ephemeral"}}
            ],
            "tools": [
                {"name": "tool_a", "description": "tool", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should still inject on tools");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[0]), "client's system breakpoint should be preserved");

        let tools = json["tools"].as_array().unwrap();
        assert!(has_cache_control(&tools[0]), "dja should inject on tool");
    }

    // 6. All 4 slots used by client → return None
    #[test]
    fn test_no_injection_when_all_slots_used() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "block 1", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 2", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 3", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 4", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        assert!(inject_cache_control(&body).is_none(), "should return None when all 4 slots are used");
    }

    // 7. 3 slots used → only 1 injection (system preferred over tools)
    #[test]
    fn test_uses_exactly_one_remaining_slot() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "block 1", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 2", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 3", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "block 4"}
            ],
            "tools": [
                {"name": "tool_a", "description": "tool", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject exactly once");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[3]), "block 4 should get the last slot");

        let tools = json["tools"].as_array().unwrap();
        assert!(!has_cache_control(&tools[0]), "tools should not be marked — no slots left");
    }

    // 8. No system, no tools → None
    #[test]
    fn test_returns_none_when_nothing_to_inject() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        assert!(inject_cache_control(&body).is_none());
    }

    // 9. Invalid JSON → None
    #[test]
    fn test_returns_none_for_invalid_json() {
        assert!(inject_cache_control(b"not json").is_none());
        assert!(inject_cache_control(b"").is_none());
        assert!(inject_cache_control(b"[]").is_none());
    }

    // 10. System and tools both already fully marked → None
    #[test]
    fn test_returns_none_when_already_fully_marked() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}
            ],
            "tools": [
                {"name": "t", "description": "tool", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        assert!(inject_cache_control(&body).is_none(), "nothing to inject when both are already marked");
    }

    // 11. String system conversion preserves the text content exactly
    #[test]
    fn test_string_system_conversion_preserves_text() {
        let original_text = "You are a pirate. Speak only in pirate English.";
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": original_text,
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();

        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let system = json["system"].as_array().unwrap();
        assert_eq!(system[0]["text"].as_str().unwrap(), original_text,
            "text content must be preserved exactly during string→array conversion");
        assert_eq!(system[0]["type"].as_str().unwrap(), "text");
    }
}
```

- [ ] **Step 5: Run all unit tests and verify they pass**

```bash
cargo test --lib cache_control 2>&1 | tail -30
```

Expected: all 11 tests PASS.

- [ ] **Step 6: Verify nothing else broke**

```bash
cargo test 2>&1 | tail -10
```

Expected: all tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/proxy/cache_control.rs src/proxy/mod.rs
git commit -m "feat: add cache_control injection module with 11 unit tests"
```

---

## Task 3: Wire injection into the handler

**Files:**
- Modify: `src/proxy/handler.rs`

- [ ] **Step 1: Add the import and helper at the top of the file**

In `src/proxy/handler.rs`, add `use crate::config::Config;` to the existing imports block:

```rust
use crate::config::Config;
use crate::proxy::cache_control;
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
```

Then add the private helper function just below the imports, before `pub async fn proxy_handler`:

```rust
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
```

- [ ] **Step 2: Wire into the skip path**

In `handle_messages_request`, find the `None` arm of `check_eligibility` (around line 79 in the original). Replace:

```rust
        None => {
            tracing::info!("cache SKIP: request not eligible");
            state.stats.record_skip();
            let _ = state.event_tx.send(RequestEvent { ... });
            // Reconstruct the request and forward normally
            let req = Request::from_parts(parts, Body::from(body_bytes));
            return forward::forward_request(&state, req).await;
        }
```

with:

```rust
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
            // Injection is safe here: no cache key extraction happened for skipped requests.
            let forward_body = maybe_inject_cache_control(&body_bytes, &state.config);
            let req = Request::from_parts(parts, Body::from(forward_body));
            return forward::forward_request(&state, req).await;
        }
```

- [ ] **Step 3: Wire into the miss path**

In the `None => { // Cache MISS` arm (around line 195), find where the streaming/non-streaming split begins:

```rust
            if parsed.is_streaming {
                // Streaming cache miss: tee the stream to client + buffer for cache
                let req = Request::from_parts(parts, Body::from(body_bytes));
                let (upstream_resp, response_builder) =
                    forward::forward_raw(&state, req, parsed.full_body).await?;
```

Replace the block starting from `if parsed.is_streaming {` through the end of the `None` arm with:

```rust
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
                let req = Request::from_parts(parts, Body::from(forward_body.clone()));
                let (response, response_bytes) =
                    forward::forward_with_body(&state, req, forward_body).await?;

                let status = response.status();
                let response_size = response_bytes.len();

                if status.is_success() && response_size <= state.config.max_response_size {
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
```

**Note on `forward_raw` signature:** `forward_raw` takes `req: Request<Body>` and `body_bytes: Bytes` as separate arguments. Pass `forward_body.clone()` as the body of `req` AND as the `body_bytes` argument. The function ignores the body inside `req` and uses the explicit `body_bytes` parameter — this matches the existing pattern in the original code where `parsed.full_body` was passed separately.

- [ ] **Step 4: Build to verify no compile errors**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 5: Run all tests**

```bash
cargo test 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/proxy/handler.rs
git commit -m "feat: wire cache_control injection into handler (skip + miss paths)"
```

---

## Task 4: Integration test for injection

**Files:**
- Modify: `tests/proxy_test.rs`

The integration test needs a mock server that captures the request body it received, so we can assert that `cache_control` fields are present.

- [ ] **Step 1: Add a request-capturing mock state and handler**

In `tests/proxy_test.rs`, after the existing `MockState` struct and before `start_mock_server`, add:

```rust
/// Mock state that also captures the last received request body.
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
```

- [ ] **Step 2: Write the injection integration test**

Add this test at the end of `tests/proxy_test.rs`:

```rust
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
    };

    let (event_tx, _rx) = dja::proxy::metrics::event_channel();
    let state = std::sync::Arc::new(dja::proxy::server::AppState {
        config,
        http_client: reqwest::Client::new(),
        embedding: tokio::sync::Mutex::new(embedding_model),
        cache,
        stats: dja::proxy::metrics::SessionStats::new(),
        event_tx,
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

    // Send a request with a system prompt and a tool — both should get cache_control injected
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

    // Inspect what the mock actually received
    let received = mock_state.last_body.lock().await;
    let received_json = received.as_ref().expect("mock should have received a request");

    // System should now be an array with cache_control on the last block
    let system = received_json.get("system")
        .expect("system field must be present");
    assert!(system.is_array(), "system should be converted to array by injection");
    let system_blocks = system.as_array().unwrap();
    assert!(
        system_blocks.last().unwrap().get("cache_control").is_some(),
        "last system block must have cache_control injected"
    );

    // Last tool should have cache_control
    let tools = received_json.get("tools")
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
        auto_cache_control: false,  // disabled
    };

    let (event_tx, _rx) = dja::proxy::metrics::event_channel();
    let state = std::sync::Arc::new(dja::proxy::server::AppState {
        config,
        http_client: reqwest::Client::new(),
        embedding: tokio::sync::Mutex::new(embedding_model),
        cache,
        stats: dja::proxy::metrics::SessionStats::new(),
        event_tx,
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

    // System should remain a string (not converted to array, no injection)
    let system = received_json.get("system").unwrap();
    assert!(
        system.is_string(),
        "system should remain a string when auto_cache_control is disabled, got: {:?}", system
    );
}
```

- [ ] **Step 3: Run the new integration tests**

```bash
cargo test test_cache_control 2>&1 | tail -30
```

Expected: both `test_cache_control_injected_on_miss` and `test_cache_control_not_injected_when_disabled` PASS. (If the embedding model is not downloaded, both print a skip message and exit gracefully.)

- [ ] **Step 4: Run the full test suite**

```bash
cargo test 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 5: Final commit**

```bash
git add tests/proxy_test.rs
git commit -m "test: add integration tests for cache_control injection"
```

---

## Self-Review

**Spec coverage:**

| Spec requirement | Task |
|-----------------|------|
| `inject_cache_control(body: &[u8]) -> Option<Bytes>` | Task 2 |
| Count existing breakpoints, use remaining slots | Task 2 (tests 5, 6, 7) |
| System string → array conversion | Task 2 (tests 1, 11) |
| System array → inject on last text block | Task 2 (test 2) |
| Tools → inject on last tool | Task 2 (test 3) |
| Both system + tools | Task 2 (test 4) |
| No system/tools → None | Task 2 (test 8) |
| Invalid JSON → None | Task 2 (test 9) |
| Already fully marked → None | Task 2 (test 10) |
| `auto_cache_control` config field, default `true` | Task 1 |
| `maybe_inject_cache_control` helper in handler | Task 3 |
| Called on skip path | Task 3 |
| Called on miss path | Task 3 |
| Safety comment at injection sites | Task 3 |
| `pub mod cache_control` in mod.rs | Task 2 |
| Debug logging inside inject_cache_control | Task 2 (implementation) |
| Integration test: injection present when enabled | Task 4 |
| Integration test: no injection when disabled | Task 4 |

No gaps found.

**Placeholder scan:** No TBDs, TODOs, or vague steps. All code blocks are complete.

**Type consistency:** `inject_cache_control` returns `Option<Bytes>` throughout. `maybe_inject_cache_control` returns `bytes::Bytes`. `forward_raw` and `forward_with_body` both accept `Bytes` as the body argument — consistent across all tasks.
