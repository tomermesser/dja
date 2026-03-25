use sha2::{Digest, Sha256};

/// A parsed request that has been determined eligible for caching.
pub struct ParsedRequest {
    pub user_message: String,
    pub system_hash: String,
    pub model: String,
    pub is_streaming: bool,
    pub full_body: bytes::Bytes,
}

/// Check if a request body is eligible for semantic caching.
///
/// Returns `Some(ParsedRequest)` if the request can be cached, `None` otherwise.
///
/// The cache key is the **last user message** only — prior conversation context
/// (earlier user/assistant turns) is intentionally ignored. The similarity
/// threshold (default 0.95) protects against false cache matches even when the
/// same question appears in different conversational contexts.
///
/// The `multi_turn` parameter controls whether multi-turn conversations are
/// eligible for caching. When `true`, any conversation whose last message is
/// from the user is eligible. When `false`, only single-turn requests (exactly
/// one user message and no assistant messages) are eligible.
pub fn check_eligibility(body: &[u8], multi_turn: bool) -> Option<ParsedRequest> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = json.as_object()?;

    // Must have a messages array
    let messages = obj.get("messages")?.as_array()?;

    // Diagnostic: log message count and roles for debugging
    let roles: Vec<&str> = messages
        .iter()
        .filter_map(|m| m.get("role").and_then(|r| r.as_str()))
        .collect();
    tracing::debug!(
        message_count = messages.len(),
        roles = ?roles,
        multi_turn_enabled = multi_turn,
        "eligibility check"
    );

    // Must have at least one message
    if messages.is_empty() {
        tracing::debug!("SKIP: no messages");
        return None;
    }

    // When multi-turn caching is disabled, enforce single-turn only:
    // exactly one message, and it must be from the user.
    if !multi_turn {
        if messages.len() != 1 {
            tracing::debug!("SKIP: multi-turn disabled and {} messages", messages.len());
            return None;
        }
        let only_msg = &messages[0];
        if only_msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            return None;
        }
    }

    // Last message must be from the user
    let last_msg = messages.last()?;
    let last_role = last_msg.get("role").and_then(|r| r.as_str());
    if last_role != Some("user") {
        tracing::debug!(last_role = ?last_role, "SKIP: last message is not user");
        return None;
    }

    let content = last_msg.get("content")?;

    // For single-turn requests, reject if tool blocks are present (safety).
    // For multi-turn, allow tool blocks — Claude Code's displayed-response
    // request includes tool_result blocks alongside the user's text question.
    // extract_text_content() only extracts "type": "text" blocks, so tool
    // blocks are naturally ignored during cache key extraction.
    if !multi_turn && has_tool_blocks(content) {
        return None;
    }

    // Extract text from content, handling both string and array forms
    let user_text = extract_text_content(content)?;

    if user_text.is_empty() {
        tracing::debug!("SKIP: empty user text");
        return None;
    }

    // Extract model
    let model = obj.get("model")?.as_str()?.to_string();

    // Extract and hash system prompt
    let system_text = obj
        .get("system")
        .and_then(extract_system_text)
        .unwrap_or_default();
    let system_hash = sha256_hex(&system_text);

    // Check if streaming
    let is_streaming = obj
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    Some(ParsedRequest {
        user_message: user_text,
        system_hash,
        model,
        is_streaming,
        full_body: bytes::Bytes::copy_from_slice(body),
    })
}

/// Extract text content from a message's content field.
/// Handles both `"content": "string"` and `"content": [{"type": "text", "text": "..."}]`.
///
/// For array content, returns only the **last non-system-reminder text block**.
/// Claude Code embeds dynamic `<system-reminder>` blocks as text content blocks
/// in the user message. These change every session and would poison the cache key.
/// The actual user question is always the last text block.
fn extract_text_content(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect();
            // Take the last text block that isn't a system-reminder.
            // Fall back to the very last text block if all are system-reminders.
            texts
                .iter()
                .rev()
                .find(|t| !t.trim_start().starts_with("<system-reminder>"))
                .or_else(|| texts.last())
                .map(|s| s.to_string())
        }
        _ => None,
    }
}

/// Extract text from the system field.
/// Handles both `"system": "string"` and `"system": [{"type": "text", "text": "..."}]`.
fn extract_system_text(system: &serde_json::Value) -> Option<String> {
    match system {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        _ => None,
    }
}

/// Check if content contains tool_result or tool_use blocks.
fn has_tool_blocks(content: &serde_json::Value) -> bool {
    if let Some(blocks) = content.as_array() {
        blocks.iter().any(|b| {
            let block_type = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
            block_type == "tool_result" || block_type == "tool_use"
        })
    } else {
        false
    }
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_eligible_request() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "What is Rust?"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("should be eligible");
        assert_eq!(parsed.user_message, "What is Rust?");
        assert_eq!(parsed.model, "claude-sonnet-4-20250514");
        assert!(!parsed.is_streaming);
    }

    #[test]
    fn test_eligible_with_system_prompt() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are a helpful assistant.",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("should be eligible");
        assert_eq!(parsed.user_message, "Hello");
        // System hash should be consistent
        let hash2 = sha256_hex("You are a helpful assistant.");
        assert_eq!(parsed.system_hash, hash2);
    }

    #[test]
    fn test_eligible_streaming() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "stream": true,
            "messages": [
                {"role": "user", "content": "Hi"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("should be eligible");
        assert!(parsed.is_streaming);
    }

    #[test]
    fn test_eligible_array_content() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Hello world"}
                ]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("should be eligible");
        assert_eq!(parsed.user_message, "Hello world");
    }

    #[test]
    fn test_eligible_multi_turn() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "How are you?"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("multi-turn should be eligible");
        assert_eq!(parsed.user_message, "How are you?");
    }

    #[test]
    fn test_ineligible_tool_use() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "123", "content": "result"}
                ]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(check_eligibility(&bytes, true).is_none(), "tool_result should be ineligible");
    }

    #[test]
    fn test_ineligible_no_messages() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514"
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(check_eligibility(&bytes, true).is_none());
    }

    #[test]
    fn test_ineligible_no_model() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(check_eligibility(&bytes, true).is_none());
    }

    #[test]
    fn test_ineligible_invalid_json() {
        assert!(check_eligibility(b"not json", true).is_none());
    }

    #[test]
    fn test_eligible_multiple_user_messages() {
        // Two user messages in a row — last user message is used
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "user", "content": "World"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("should be eligible");
        assert_eq!(parsed.user_message, "World");
    }

    #[test]
    fn test_multi_turn_extracts_last_user_message() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "First question"},
                {"role": "assistant", "content": "First answer"},
                {"role": "user", "content": "Second question"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("should be eligible");
        assert_eq!(parsed.user_message, "Second question");
    }

    #[test]
    fn test_multi_turn_rejects_tool_use_in_last_message() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi!"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "123", "content": "result"}
                ]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(check_eligibility(&bytes, true).is_none(), "tool_result in last message should be ineligible");
    }

    #[test]
    fn test_multi_turn_rejects_if_last_message_not_user() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(check_eligibility(&bytes, true).is_none(), "last message is assistant, should be ineligible");
    }

    #[test]
    fn test_system_hash_absent_vs_empty() {
        // No system field
        let body1 = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let parsed1 = check_eligibility(&serde_json::to_vec(&body1).unwrap(), true).unwrap();

        // The hash of an empty string should be used when system is absent
        let empty_hash = sha256_hex("");
        assert_eq!(parsed1.system_hash, empty_hash);
    }

    #[test]
    fn test_tool_use_in_user_content_blocks_single_turn() {
        // Single-turn: tool blocks in user message → rejected
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Here's some context"},
                    {"type": "tool_use", "id": "123", "name": "get_weather", "input": {}}
                ]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(check_eligibility(&bytes, false).is_none(), "single-turn tool_use should be ineligible");
    }

    #[test]
    fn test_tool_blocks_allowed_in_multi_turn() {
        // Multi-turn: tool blocks in last user message → allowed, text extracted
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "First question"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "123", "name": "get_weather", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "123", "content": "sunny"},
                    {"type": "text", "text": "What about tomorrow?"}
                ]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("multi-turn with tool blocks should be eligible");
        assert_eq!(parsed.user_message, "What about tomorrow?");
    }

    #[test]
    fn test_extracts_last_non_system_reminder_text_block() {
        // Claude Code embeds <system-reminder> as text blocks in user content.
        // We should extract only the actual user question (last non-system-reminder block).
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "<system-reminder>\nDynamic session context that changes\n</system-reminder>"},
                    {"type": "text", "text": "How are you today?"}
                ]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("should be eligible");
        assert_eq!(parsed.user_message, "How are you today?");
    }

    #[test]
    fn test_system_array_format() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "You are helpful."},
                {"type": "text", "text": "Be concise."}
            ],
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, true).expect("should be eligible");
        let expected_hash = sha256_hex("You are helpful.\nBe concise.");
        assert_eq!(parsed.system_hash, expected_hash);
    }

    #[test]
    fn test_multi_turn_disabled_rejects_multi_turn() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "How are you?"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(
            check_eligibility(&bytes, false).is_none(),
            "multi-turn should be ineligible when multi_turn=false"
        );
    }

    #[test]
    fn test_multi_turn_disabled_allows_single_turn() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "What is Rust?"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let parsed = check_eligibility(&bytes, false).expect("single-turn should be eligible when multi_turn=false");
        assert_eq!(parsed.user_message, "What is Rust?");
    }

    #[test]
    fn test_multi_turn_disabled_rejects_multiple_user_messages() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "user", "content": "World"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(
            check_eligibility(&bytes, false).is_none(),
            "multiple messages should be ineligible when multi_turn=false"
        );
    }
}
