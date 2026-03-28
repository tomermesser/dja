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

    let existing_count = {
        let mut count = 0;
        for field in &["system", "tools", "messages"] {
            if let Some(val) = obj.get(*field) {
                count += count_cache_control_in_value(val);
            }
        }
        count
    };

    if existing_count >= 4 {
        return None;
    }
    let mut remaining = 4 - existing_count;
    let mut injected_system = false;
    let mut injected_tools = false;

    if remaining > 0 {
        if let Some(system) = obj.get_mut("system") {
            if try_inject_system(system) {
                injected_system = true;
                remaining -= 1;
            }
        }
    }

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

fn count_cache_control_in_value(val: &serde_json::Value) -> usize {
    match val {
        serde_json::Value::Object(map) => {
            let self_count = if map.contains_key("cache_control") { 1 } else { 0 };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn has_cache_control(val: &serde_json::Value) -> bool {
        val.get("cache_control").is_some()
    }

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
        assert!(has_cache_control(&blocks[0]));
    }

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
        assert!(!has_cache_control(&blocks[0]));
        assert!(has_cache_control(&blocks[1]));
    }

    #[test]
    fn test_injects_tools_last_tool() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "tools": [
                {"name": "tool_a", "description": "first", "input_schema": {"type": "object"}},
                {"name": "tool_b", "description": "second", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();
        let result = inject_cache_control(&body).expect("should inject");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();
        let tools = json["tools"].as_array().unwrap();
        assert!(!has_cache_control(&tools[0]));
        assert!(has_cache_control(&tools[1]));
    }

    #[test]
    fn test_injects_both_system_and_tools() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are helpful.",
            "tools": [{"name": "search", "description": "search", "input_schema": {"type": "object"}}],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();
        let result = inject_cache_control(&body).expect("should inject both");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();
        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[0]));
        let tools = json["tools"].as_array().unwrap();
        assert!(has_cache_control(&tools[0]));
    }

    #[test]
    fn test_preserves_existing_client_breakpoints() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [{"type": "text", "text": "Client marked this", "cache_control": {"type": "ephemeral"}}],
            "tools": [{"name": "tool_a", "description": "tool", "input_schema": {"type": "object"}}],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();
        let result = inject_cache_control(&body).expect("should inject on tools");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();
        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[0]));
        let tools = json["tools"].as_array().unwrap();
        assert!(has_cache_control(&tools[0]));
    }

    #[test]
    fn test_no_injection_when_all_slots_used() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "b1", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "b2", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "b3", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "b4", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();
        assert!(inject_cache_control(&body).is_none());
    }

    #[test]
    fn test_uses_exactly_one_remaining_slot() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "b1", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "b2", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "b3", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "b4"}
            ],
            "tools": [{"name": "tool_a", "description": "tool", "input_schema": {"type": "object"}}],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();
        let result = inject_cache_control(&body).expect("should inject once");
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();
        let system = json["system"].as_array().unwrap();
        assert!(has_cache_control(&system[3]));
        let tools = json["tools"].as_array().unwrap();
        assert!(!has_cache_control(&tools[0]));
    }

    #[test]
    fn test_returns_none_when_nothing_to_inject() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();
        assert!(inject_cache_control(&body).is_none());
    }

    #[test]
    fn test_returns_none_for_invalid_json() {
        assert!(inject_cache_control(b"not json").is_none());
        assert!(inject_cache_control(b"").is_none());
        assert!(inject_cache_control(b"[]").is_none());
    }

    #[test]
    fn test_returns_none_when_already_fully_marked() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [{"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}],
            "tools": [{"name": "t", "description": "tool", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "user", "content": "Hello"}]
        })).unwrap();
        assert!(inject_cache_control(&body).is_none());
    }

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
        assert_eq!(system[0]["text"].as_str().unwrap(), original_text);
        assert_eq!(system[0]["type"].as_str().unwrap(), "text");
    }
}
