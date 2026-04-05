use crate::auth::ResolvedAuth;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TEST_MODEL: &str = "gpt-5.3-codex-spark";
pub const CONTROL_PREFIX: &str = "mock-openai-control:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlledResponse {
    ToolCall { name: String, arguments: Value },
    Text { content: String },
}

pub fn auth(base_url: impl Into<String>) -> ResolvedAuth {
    ResolvedAuth {
        provider: "openai".to_string(),
        api_key: "test-key".to_string(),
        base_url: base_url.into(),
        account_id: None,
    }
}

pub fn controlled_tool_prompt(name: &str, arguments: Value) -> String {
    controlled_response_prompt(ControlledResponse::ToolCall {
        name: name.to_string(),
        arguments,
    })
}

pub fn controlled_text_prompt(content: &str) -> String {
    controlled_response_prompt(ControlledResponse::Text {
        content: content.to_string(),
    })
}

pub fn controlled_response_prompt(control: ControlledResponse) -> String {
    format!(
        "{CONTROL_PREFIX}{}",
        serde_json::to_string(&control).unwrap_or_default()
    )
}

pub fn parse_controlled_prompt(prompt: &str) -> Option<ControlledResponse> {
    let raw = prompt.trim();
    let payload = raw.strip_prefix(CONTROL_PREFIX)?;
    serde_json::from_str(payload).ok()
}

pub fn tool_call_sse(name: &str, arguments: &Value) -> String {
    let arguments_text = serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string());
    format!(
        concat!(
            "event: response.output_item.added\n",
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"{name}\"}}}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {{\"type\":\"response.function_call_arguments.delta\",\"delta\":{arguments}}}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {{\"type\":\"response.function_call_arguments.done\",\"call_id\":\"call-1\",\"name\":\"{name}\"}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"usage\":{{\"input_tokens\":11,\"output_tokens\":7,\"total_tokens\":18}}}}\n\n"
        ),
        name = name,
        arguments = serde_json::to_string(&arguments_text).unwrap_or_else(|_| "\"{}\"".to_string()),
    )
}

pub fn text_sse(text: &str) -> String {
    format!(
        concat!(
            "event: response.output_text.delta\n",
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":{text}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"usage\":{{\"input_tokens\":13,\"output_tokens\":5,\"total_tokens\":18}}}}\n\n"
        ),
        text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string()),
    )
}
