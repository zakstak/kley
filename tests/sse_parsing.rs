//! Integration tests for SSE stream parsing functions.
//!
//! These exercise `process_openai_sse_block` and `process_zai_sse_line`
//! with synthetic payloads to verify correct delta accumulation,
//! completion detection, and error handling.

use kley::agent::{process_openai_sse_block, process_zai_sse_line};

// ═══════════════════════════════════════════════════════════════════════════
// OpenAI SSE block parsing
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn openai_delta_accumulates() {
    let mut response = String::new();

    let block = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"Hello"}"#;

    let done = process_openai_sse_block(block, &mut response).unwrap();
    assert!(!done);
    assert_eq!(response, "Hello");

    let block2 = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":", world!"}"#;

    let done = process_openai_sse_block(block2, &mut response).unwrap();
    assert!(!done);
    assert_eq!(response, "Hello, world!");
}

#[test]
fn openai_completed_signal() {
    let mut response = String::new();

    let block = r#"event: response.completed
data: {"type":"response.completed"}"#;

    let done = process_openai_sse_block(block, &mut response).unwrap();
    assert!(done);
    assert!(response.is_empty(), "completed should not add content");
}

#[test]
fn openai_error_returns_err() {
    let mut response = String::new();

    let block = r#"event: error
data: {"error":{"message":"rate limit exceeded"}}"#;

    let result = process_openai_sse_block(block, &mut response);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("rate limit exceeded"),
        "error message should propagate: {err}"
    );
}

#[test]
fn openai_empty_data_is_noop() {
    let mut response = String::new();

    let block = "event: response.created\n";
    let done = process_openai_sse_block(block, &mut response).unwrap();
    assert!(!done);
    assert!(response.is_empty());
}

#[test]
fn openai_unknown_event_type_ignored() {
    let mut response = String::new();

    let block = r#"event: response.some_future_event
data: {"type":"response.some_future_event","foo":"bar"}"#;

    let done = process_openai_sse_block(block, &mut response).unwrap();
    assert!(!done);
    assert!(response.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// ZAI SSE line parsing
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn zai_delta_accumulates() {
    let mut response = String::new();

    let line = r#"data: {"choices":[{"delta":{"content":"你好"}}]}"#;
    let done = process_zai_sse_line(line, &mut response).unwrap();
    assert!(!done);
    assert_eq!(response, "你好");

    let line2 = r#"data: {"choices":[{"delta":{"content":"世界"}}]}"#;
    let done = process_zai_sse_line(line2, &mut response).unwrap();
    assert!(!done);
    assert_eq!(response, "你好世界");
}

#[test]
fn zai_done_signal() {
    let mut response = String::new();

    let line = "data: [DONE]";
    let done = process_zai_sse_line(line, &mut response).unwrap();
    assert!(done);
}

#[test]
fn zai_empty_line_is_noop() {
    let mut response = String::new();
    let done = process_zai_sse_line("", &mut response).unwrap();
    assert!(!done);
    assert!(response.is_empty());
}

#[test]
fn zai_comment_line_is_noop() {
    let mut response = String::new();
    let done = process_zai_sse_line(": keep-alive", &mut response).unwrap();
    assert!(!done);
    assert!(response.is_empty());
}

#[test]
fn zai_malformed_json_is_silent() {
    let mut response = String::new();
    let line = "data: {not valid json";
    let done = process_zai_sse_line(line, &mut response).unwrap();
    assert!(!done);
    assert!(response.is_empty());
}

#[test]
fn zai_missing_content_field() {
    let mut response = String::new();
    let line = r#"data: {"choices":[{"delta":{}}]}"#;
    let done = process_zai_sse_line(line, &mut response).unwrap();
    assert!(!done);
    assert!(response.is_empty());
}

#[test]
fn zai_empty_choices_array() {
    let mut response = String::new();
    let line = r#"data: {"choices":[]}"#;
    let done = process_zai_sse_line(line, &mut response).unwrap();
    assert!(!done);
    assert!(response.is_empty());
}
