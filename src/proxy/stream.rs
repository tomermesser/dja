use axum::body::Body;
use bytes::Bytes;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::oneshot;

/// A parsed SSE event.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event_type: Option<String>,
    pub data: String,
}

/// Parse raw SSE bytes into structured events.
///
/// SSE format: lines starting with `event:`, `data:`, separated by blank lines.
/// An event may have multiple `data:` lines (concatenated with newlines).
pub fn parse_sse_events(raw: &[u8]) -> Vec<SseEvent> {
    let text = String::from_utf8_lossy(raw);
    let mut events = Vec::new();
    let mut current_event_type: Option<String> = None;
    let mut current_data_lines: Vec<String> = Vec::new();

    for line in text.lines() {
        if line.is_empty() {
            // Blank line = end of event
            if !current_data_lines.is_empty() {
                events.push(SseEvent {
                    event_type: current_event_type.take(),
                    data: current_data_lines.join("\n"),
                });
                current_data_lines.clear();
            } else {
                current_event_type = None;
            }
        } else if let Some(value) = line.strip_prefix("event:") {
            current_event_type = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            current_data_lines.push(value.trim_start_matches(' ').to_string());
        }
        // Ignore other lines (comments starting with ':', etc.)
    }

    // Handle trailing event without final blank line
    if !current_data_lines.is_empty() {
        events.push(SseEvent {
            event_type: current_event_type.take(),
            data: current_data_lines.join("\n"),
        });
    }

    events
}

/// Serialize an SSE event back to wire format.
fn serialize_sse_event(event: &SseEvent) -> String {
    let mut out = String::new();
    if let Some(ref et) = event.event_type {
        out.push_str("event: ");
        out.push_str(et);
        out.push('\n');
    }
    for line in event.data.lines() {
        out.push_str("data: ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out
}

/// Inject `[cached] ` marker into the first text_delta content_block_delta event.
///
/// Returns the modified raw SSE bytes.
pub fn inject_cached_marker_sse(raw: &[u8]) -> Vec<u8> {
    let mut events = parse_sse_events(raw);
    let mut injected = false;

    for event in &mut events {
        if injected {
            break;
        }
        if event.event_type.as_deref() == Some("content_block_delta") {
            if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&event.data) {
                let is_text_delta = json
                    .get("delta")
                    .and_then(|d| d.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("text_delta");

                if is_text_delta {
                    if let Some(text) = json
                        .get_mut("delta")
                        .and_then(|d| d.get_mut("text"))
                        .and_then(|t| t.as_str().map(|s| s.to_string()))
                    {
                        json["delta"]["text"] =
                            serde_json::Value::String(format!("[cached] {text}"));
                        event.data = serde_json::to_string(&json).unwrap();
                        injected = true;
                    }
                }
            }
        }
    }

    let mut out = String::new();
    for event in &events {
        out.push_str(&serialize_sse_event(event));
    }
    out.into_bytes()
}

/// Inject `[cached] ` marker into a non-streaming JSON response.
///
/// Finds the first content block of type "text" and prepends `[cached] ` to its text.
pub fn inject_cached_marker_json(raw: &[u8]) -> Vec<u8> {
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(raw) else {
        return raw.to_vec();
    };

    if let Some(content) = json.get_mut("content").and_then(|c| c.as_array_mut()) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()).map(String::from) {
                    block["text"] = serde_json::Value::String(format!("[cached] {text}"));
                    break;
                }
            }
        }
    }

    serde_json::to_vec(&json).unwrap_or_else(|_| raw.to_vec())
}

/// Stream that replays cached SSE events with small delays.
struct ReplayStream {
    events: Vec<SseEvent>,
    index: usize,
    delay: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl futures::Stream for ReplayStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // If we have a pending delay, wait for it
        if let Some(ref mut delay) = self.delay {
            match delay.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(()) => {
                    self.delay = None;
                }
            }
        }

        if self.index >= self.events.len() {
            return Poll::Ready(None);
        }

        let serialized = serialize_sse_event(&self.events[self.index]);
        self.index += 1;

        // Set up delay for next event
        if self.index < self.events.len() {
            self.delay = Some(Box::pin(tokio::time::sleep(std::time::Duration::from_millis(5))));
        }

        Poll::Ready(Some(Ok(Bytes::from(serialized))))
    }
}

/// Replay cached SSE response as a streaming body with small delays between events.
pub fn replay_cached_response(cached_data: Vec<u8>, inject_marker: bool) -> Body {
    let data = if inject_marker {
        inject_cached_marker_sse(&cached_data)
    } else {
        cached_data
    };

    let events = parse_sse_events(&data);
    let stream = ReplayStream {
        events,
        index: 0,
        delay: None,
    };

    Body::from_stream(stream)
}

/// Stream that tees upstream bytes to both client and a buffer.
struct TeeStream {
    inner: Pin<Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: Vec<u8>,
    tx: Option<oneshot::Sender<Vec<u8>>>,
}

impl futures::Stream for TeeStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => {
                self.buffer.extend_from_slice(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => {
                // Send what we have so far
                if let Some(tx) = self.tx.take() {
                    let buf = std::mem::take(&mut self.buffer);
                    let _ = tx.send(buf);
                }
                Poll::Ready(Some(Err(std::io::Error::other(e))))
            }
            Poll::Ready(None) => {
                // Stream complete — send full buffer
                if let Some(tx) = self.tx.take() {
                    let buf = std::mem::take(&mut self.buffer);
                    let _ = tx.send(buf);
                }
                Poll::Ready(None)
            }
        }
    }
}

/// Create a tee stream that forwards upstream SSE to the client while buffering for cache.
///
/// Returns (body for client, receiver for buffered bytes).
pub fn tee_stream(
    upstream: reqwest::Response,
) -> (Body, oneshot::Receiver<Vec<u8>>) {
    let (tx, rx) = oneshot::channel::<Vec<u8>>();

    let byte_stream = upstream.bytes_stream();

    let stream = TeeStream {
        inner: Box::pin(byte_stream),
        buffer: Vec::new(),
        tx: Some(tx),
    };

    let body = Body::from_stream(stream);
    (body, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_events() {
        let raw = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: ping\ndata: {}\n\n";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type.as_deref(), Some("message_start"));
        assert_eq!(events[0].data, "{\"type\":\"message_start\"}");
        assert_eq!(events[1].event_type.as_deref(), Some("ping"));
        assert_eq!(events[1].data, "{}");
    }

    #[test]
    fn test_parse_event_without_type() {
        let raw = b"data: {\"hello\":\"world\"}\n\n";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, None);
        assert_eq!(events[0].data, "{\"hello\":\"world\"}");
    }

    #[test]
    fn test_parse_multiline_data() {
        let raw = b"event: test\ndata: line1\ndata: line2\n\n";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn test_parse_trailing_event_no_blank_line() {
        let raw = b"event: test\ndata: hello";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type.as_deref(), Some("test"));
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn test_parse_empty_input() {
        let events = parse_sse_events(b"");
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_multiple_blank_lines() {
        let raw = b"event: a\ndata: 1\n\n\nevent: b\ndata: 2\n\n";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type.as_deref(), Some("a"));
        assert_eq!(events[1].event_type.as_deref(), Some("b"));
    }

    #[test]
    fn test_inject_cached_marker_sse() {
        let raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\"}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n",
            "\n",
        );

        let result = inject_cached_marker_sse(raw.as_bytes());
        let result_str = String::from_utf8(result).unwrap();

        // Verify by parsing
        let events = parse_sse_events(result_str.as_bytes());
        assert_eq!(events.len(), 3);

        let delta1: serde_json::Value = serde_json::from_str(&events[1].data).unwrap();
        assert_eq!(delta1["delta"]["text"].as_str().unwrap(), "[cached] Hello");

        let delta2: serde_json::Value = serde_json::from_str(&events[2].data).unwrap();
        assert_eq!(delta2["delta"]["text"].as_str().unwrap(), " world");
    }

    #[test]
    fn test_inject_cached_marker_sse_no_text_delta() {
        let raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\"}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            "\n",
        );

        let result = inject_cached_marker_sse(raw.as_bytes());
        let result_str = String::from_utf8(result).unwrap();
        assert!(!result_str.contains("[cached]"));
    }

    #[test]
    fn test_inject_cached_marker_sse_only_first_delta() {
        // Ensure only the FIRST text_delta gets the marker
        let raw = concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"A\"}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"B\"}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"C\"}}\n",
            "\n",
        );

        let result = inject_cached_marker_sse(raw.as_bytes());
        let events = parse_sse_events(&result);

        let d0: serde_json::Value = serde_json::from_str(&events[0].data).unwrap();
        assert_eq!(d0["delta"]["text"].as_str().unwrap(), "[cached] A");

        let d1: serde_json::Value = serde_json::from_str(&events[1].data).unwrap();
        assert_eq!(d1["delta"]["text"].as_str().unwrap(), "B");

        let d2: serde_json::Value = serde_json::from_str(&events[2].data).unwrap();
        assert_eq!(d2["delta"]["text"].as_str().unwrap(), "C");
    }

    #[test]
    fn test_inject_cached_marker_json() {
        let json = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "content": [
                {"type": "text", "text": "Hello world"}
            ]
        });
        let raw = serde_json::to_vec(&json).unwrap();
        let result = inject_cached_marker_json(&raw);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(
            parsed["content"][0]["text"].as_str().unwrap(),
            "[cached] Hello world"
        );
    }

    #[test]
    fn test_inject_cached_marker_json_no_text_block() {
        let json = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "content": [
                {"type": "tool_use", "id": "tu_1", "name": "foo", "input": {}}
            ]
        });
        let raw = serde_json::to_vec(&json).unwrap();
        let result = inject_cached_marker_json(&raw);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed["content"][0]["type"].as_str().unwrap(), "tool_use");
    }

    #[test]
    fn test_inject_cached_marker_json_invalid() {
        let raw = b"not json";
        let result = inject_cached_marker_json(raw);
        assert_eq!(result, raw);
    }

    #[test]
    fn test_inject_cached_marker_json_multiple_blocks() {
        // Only the first text block should get the marker
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "First"},
                {"type": "text", "text": "Second"}
            ]
        });
        let raw = serde_json::to_vec(&json).unwrap();
        let result = inject_cached_marker_json(&raw);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed["content"][0]["text"].as_str().unwrap(), "[cached] First");
        assert_eq!(parsed["content"][1]["text"].as_str().unwrap(), "Second");
    }

    #[test]
    fn test_roundtrip_serialize_parse() {
        let original_raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\"}\n",
            "\n",
            "event: ping\n",
            "data: {}\n",
            "\n",
        );
        let events = parse_sse_events(original_raw.as_bytes());
        let mut serialized = String::new();
        for event in &events {
            serialized.push_str(&serialize_sse_event(event));
        }
        let re_parsed = parse_sse_events(serialized.as_bytes());
        assert_eq!(events, re_parsed);
    }
}
