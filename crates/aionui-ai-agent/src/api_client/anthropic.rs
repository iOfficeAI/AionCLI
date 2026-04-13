use std::sync::Arc;

use serde_json::{Value, json};

use super::client::{ApiClientError, RotatingClient};
use super::key_manager::ApiKeyManager;

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic API client with multi-key rotation.
///
/// Supports both the native Anthropic Messages API and an
/// OpenAI-compatible `createChatCompletion` that performs protocol
/// conversion under the hood.
pub struct AnthropicRotatingClient {
    inner: RotatingClient,
}

impl AnthropicRotatingClient {
    pub fn new(
        key_manager: Arc<ApiKeyManager>,
        base_url: &str,
        max_retries: Option<usize>,
        retry_delay_ms: Option<u64>,
    ) -> Self {
        Self {
            inner: RotatingClient::new(key_manager, base_url, max_retries, retry_delay_ms),
        }
    }

    pub fn key_manager(&self) -> &Arc<ApiKeyManager> {
        self.inner.key_manager()
    }

    pub fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    /// Native Anthropic Messages API call.
    ///
    /// POST /v1/messages with `x-api-key` + `anthropic-version` headers.
    pub async fn create_message(
        &self,
        request: &Value,
    ) -> Result<Value, ApiClientError> {
        self.inner
            .execute_with_retry(|client, base_url, api_key| {
                client
                    .post(format!("{base_url}/v1/messages"))
                    .header("x-api-key", api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .json(request)
            })
            .await
    }

    /// OpenAI-compatible chat completion, converted to/from Anthropic format.
    pub async fn create_chat_completion(
        &self,
        request: &Value,
    ) -> Result<Value, ApiClientError> {
        let anthropic_request = openai_to_anthropic_request(request);

        let response = self
            .inner
            .execute_with_retry(|client, base_url, api_key| {
                client
                    .post(format!("{base_url}/v1/messages"))
                    .header("x-api-key", api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .json(&anthropic_request)
            })
            .await?;

        Ok(anthropic_to_openai_response(&response))
    }
}

// ---------------------------------------------------------------------------
// OpenAI → Anthropic protocol conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI ChatCompletion request to Anthropic Messages format.
fn openai_to_anthropic_request(openai: &Value) -> Value {
    let mut messages = Vec::new();
    let mut system_text: Option<String> = None;

    if let Some(openai_messages) = openai.get("messages").and_then(|v| v.as_array()) {
        for msg in openai_messages {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("user");
            let content = msg.get("content").cloned().unwrap_or(json!(""));

            match role {
                "system" => {
                    // Anthropic uses a top-level `system` field
                    if let Some(text) = content.as_str() {
                        system_text = Some(text.to_string());
                    }
                }
                "assistant" => {
                    messages.push(json!({
                        "role": "assistant",
                        "content": content,
                    }));
                }
                _ => {
                    // "user" or any other role
                    messages.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            }
        }
    }

    let model = openai
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("claude-sonnet-4-20250514");

    let max_tokens = openai
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(4096);

    let mut result = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
    });

    if let Some(system) = system_text {
        result["system"] = json!(system);
    }
    if let Some(temp) = openai.get("temperature") {
        result["temperature"] = temp.clone();
    }
    if let Some(top_p) = openai.get("top_p") {
        result["top_p"] = top_p.clone();
    }

    // Convert tools
    if let Some(tools) = openai.get("tools").and_then(|v| v.as_array()) {
        let anthropic_tools: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                if tool.get("type").and_then(|t| t.as_str()) != Some("function") {
                    return None;
                }
                let func = tool.get("function")?;
                let mut t = json!({
                    "name": func.get("name")?,
                });
                if let Some(desc) = func.get("description") {
                    t["description"] = desc.clone();
                }
                if let Some(params) = func.get("parameters") {
                    t["input_schema"] = params.clone();
                }
                Some(t)
            })
            .collect();

        if !anthropic_tools.is_empty() {
            result["tools"] = json!(anthropic_tools);
        }
    }

    result
}

/// Convert an Anthropic Messages response to OpenAI ChatCompletion format.
fn anthropic_to_openai_response(anthropic: &Value) -> Value {
    let content_blocks = anthropic
        .get("content")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Concatenate all text blocks
    let text: String = content_blocks
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                block.get("text").and_then(|t| t.as_str()).map(String::from)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");

    let stop_reason = anthropic
        .get("stop_reason")
        .and_then(|r| r.as_str())
        .map(anthropic_stop_reason_to_openai)
        .unwrap_or("stop");

    json!({
        "id": anthropic.get("id").cloned().unwrap_or(json!("anthropic-converted")),
        "object": "chat.completion",
        "model": anthropic.get("model").cloned().unwrap_or(json!("")),
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text,
            },
            "finish_reason": stop_reason,
        }],
        "usage": convert_anthropic_usage(anthropic.get("usage")),
    })
}

fn anthropic_stop_reason_to_openai(reason: &str) -> &str {
    match reason {
        "end_turn" => "stop",
        "max_tokens" => "length",
        "stop_sequence" => "stop",
        "tool_use" => "tool_calls",
        _ => "stop",
    }
}

fn convert_anthropic_usage(usage: Option<&Value>) -> Value {
    let Some(u) = usage else {
        return json!({});
    };
    json!({
        "prompt_tokens": u.get("input_tokens").cloned().unwrap_or(json!(0)),
        "completion_tokens": u.get("output_tokens").cloned().unwrap_or(json!(0)),
        "total_tokens":
            u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
            + u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_to_anthropic_basic() {
        let openai = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                { "role": "user", "content": "Hello" }
            ],
            "max_tokens": 200,
            "temperature": 0.5
        });

        let anthropic = openai_to_anthropic_request(&openai);
        assert_eq!(anthropic["model"], "claude-sonnet-4-20250514");
        assert_eq!(anthropic["max_tokens"], 200);
        assert_eq!(anthropic["temperature"], 0.5);
        assert_eq!(anthropic["messages"][0]["role"], "user");
        assert_eq!(anthropic["messages"][0]["content"], "Hello");
    }

    #[test]
    fn openai_to_anthropic_system_message() {
        let openai = json!({
            "messages": [
                { "role": "system", "content": "Be helpful" },
                { "role": "user", "content": "Hi" }
            ]
        });

        let anthropic = openai_to_anthropic_request(&openai);
        assert_eq!(anthropic["system"], "Be helpful");
        // System message should not appear in messages array
        let messages = anthropic["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn openai_to_anthropic_tools() {
        let openai = json!({
            "messages": [{ "role": "user", "content": "Search" }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "web_search",
                    "description": "Search the web",
                    "parameters": { "type": "object" }
                }
            }]
        });

        let anthropic = openai_to_anthropic_request(&openai);
        let tools = anthropic["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "web_search");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn openai_to_anthropic_default_max_tokens() {
        let openai = json!({
            "messages": [{ "role": "user", "content": "Hi" }]
        });

        let anthropic = openai_to_anthropic_request(&openai);
        assert_eq!(anthropic["max_tokens"], 4096);
    }

    #[test]
    fn anthropic_to_openai_basic() {
        let anthropic = json!({
            "id": "msg_123",
            "model": "claude-sonnet-4-20250514",
            "content": [
                { "type": "text", "text": "Hello!" }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        let openai = anthropic_to_openai_response(&anthropic);
        assert_eq!(openai["id"], "msg_123");
        assert_eq!(openai["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(openai["choices"][0]["finish_reason"], "stop");
        assert_eq!(openai["usage"]["prompt_tokens"], 10);
        assert_eq!(openai["usage"]["completion_tokens"], 5);
        assert_eq!(openai["usage"]["total_tokens"], 15);
    }

    #[test]
    fn anthropic_to_openai_multiple_text_blocks() {
        let anthropic = json!({
            "content": [
                { "type": "text", "text": "Part 1" },
                { "type": "text", "text": "Part 2" }
            ],
            "stop_reason": "end_turn"
        });

        let openai = anthropic_to_openai_response(&anthropic);
        assert_eq!(openai["choices"][0]["message"]["content"], "Part 1Part 2");
    }

    #[test]
    fn anthropic_to_openai_empty() {
        let anthropic = json!({});
        let openai = anthropic_to_openai_response(&anthropic);
        assert_eq!(openai["choices"][0]["message"]["content"], "");
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(anthropic_stop_reason_to_openai("end_turn"), "stop");
        assert_eq!(anthropic_stop_reason_to_openai("max_tokens"), "length");
        assert_eq!(anthropic_stop_reason_to_openai("tool_use"), "tool_calls");
        assert_eq!(anthropic_stop_reason_to_openai("unknown"), "stop");
    }

    #[test]
    fn constructs_with_correct_base_url() {
        let km = Arc::new(ApiKeyManager::new("sk-ant-test", None));
        let client = AnthropicRotatingClient::new(km, "https://api.anthropic.com/v1", None, None);
        assert_eq!(client.base_url(), "https://api.anthropic.com");
    }
}
