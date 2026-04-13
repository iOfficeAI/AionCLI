use std::sync::Arc;

use serde_json::{Value, json};

use super::client::{ApiClientError, RotatingClient};
use super::key_manager::ApiKeyManager;

/// Gemini API client with multi-key rotation.
///
/// Supports both the native Gemini `generateContent` endpoint and an
/// OpenAI-compatible `createChatCompletion` that performs protocol
/// conversion under the hood.
pub struct GeminiRotatingClient {
    inner: RotatingClient,
}

impl GeminiRotatingClient {
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

    /// Native Gemini `generateContent` call.
    ///
    /// POST /v1beta/models/{model}:generateContent?key={api_key}
    pub async fn generate_content(
        &self,
        model: &str,
        request: &Value,
    ) -> Result<Value, ApiClientError> {
        let model = model.to_string();
        self.inner
            .execute_with_retry(|client, base_url, api_key| {
                client
                    .post(format!(
                        "{base_url}/v1beta/models/{model}:generateContent?key={api_key}"
                    ))
                    .header("Content-Type", "application/json")
                    .json(request)
            })
            .await
    }

    /// OpenAI-compatible chat completion, converted to/from Gemini format.
    pub async fn create_chat_completion(
        &self,
        request: &Value,
    ) -> Result<Value, ApiClientError> {
        let gemini_request = openai_to_gemini_request(request);
        let model = request
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("gemini-2.0-flash")
            .to_string();

        let response = self
            .inner
            .execute_with_retry(|client, base_url, api_key| {
                client
                    .post(format!(
                        "{base_url}/v1beta/models/{model}:generateContent?key={api_key}"
                    ))
                    .header("Content-Type", "application/json")
                    .json(&gemini_request)
            })
            .await?;

        Ok(gemini_to_openai_response(&response))
    }
}

// ---------------------------------------------------------------------------
// OpenAI → Gemini protocol conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI ChatCompletion request to Gemini generateContent format.
fn openai_to_gemini_request(openai: &Value) -> Value {
    let mut contents = Vec::new();

    if let Some(messages) = openai.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("user");
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");

            let gemini_role = match role {
                "assistant" => "model",
                "system" => "user", // Gemini handles system via systemInstruction
                _ => "user",
            };

            contents.push(json!({
                "role": gemini_role,
                "parts": [{ "text": content }]
            }));
        }
    }

    let mut generation_config = json!({});
    if let Some(temp) = openai.get("temperature") {
        generation_config["temperature"] = temp.clone();
    }
    if let Some(max_tokens) = openai.get("max_tokens") {
        generation_config["maxOutputTokens"] = max_tokens.clone();
    }
    if let Some(top_p) = openai.get("top_p") {
        generation_config["topP"] = top_p.clone();
    }

    // Image generation detection: if prompt mentions image generation,
    // set responseModalities.
    let has_image_request = contents.iter().any(|c| {
        c.get("parts")
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts.iter().any(|p| {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .is_some_and(is_image_generation_prompt)
                })
            })
            .unwrap_or(false)
    });
    if has_image_request {
        generation_config["responseMimeType"] = json!("text/plain");
        generation_config["responseModalities"] = json!(["IMAGE", "TEXT"]);
    }

    let mut result = json!({
        "contents": contents,
        "generationConfig": generation_config,
    });

    // Convert system message to systemInstruction
    if let Some(messages) = openai.get("messages").and_then(|v| v.as_array())
        && let Some(system_msg) = messages
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        && let Some(content) = system_msg.get("content").and_then(|c| c.as_str())
    {
        result["systemInstruction"] = json!({
            "parts": [{ "text": content }]
        });
    }

    // Convert tools (function declarations)
    if let Some(tools) = openai.get("tools").and_then(|v| v.as_array()) {
        let declarations: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                if tool.get("type").and_then(|t| t.as_str()) != Some("function") {
                    return None;
                }
                let func = tool.get("function")?;
                let name = func.get("name").and_then(|n| n.as_str())?;
                let mut decl = json!({
                    "name": clean_function_name(name),
                });
                if let Some(desc) = func.get("description") {
                    decl["description"] = desc.clone();
                }
                if let Some(params) = func.get("parameters") {
                    decl["parameters"] = params.clone();
                }
                Some(decl)
            })
            .collect();

        if !declarations.is_empty() {
            result["tools"] = json!([{ "functionDeclarations": declarations }]);
        }
    }

    result
}

/// Convert a Gemini generateContent response to OpenAI ChatCompletion format.
fn gemini_to_openai_response(gemini: &Value) -> Value {
    let candidates = gemini
        .get("candidates")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let first = candidates.first();

    let content = first
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .and_then(|parts| parts.first())
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");

    let finish_reason = first
        .and_then(|c| c.get("finishReason"))
        .and_then(|r| r.as_str())
        .map(gemini_finish_reason_to_openai)
        .unwrap_or("stop");

    json!({
        "id": "gemini-converted",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content
            },
            "finish_reason": finish_reason
        }],
        "usage": gemini.get("usageMetadata").cloned().unwrap_or(json!({}))
    })
}

fn gemini_finish_reason_to_openai(reason: &str) -> &str {
    match reason {
        "STOP" => "stop",
        "MAX_TOKENS" => "length",
        "SAFETY" => "content_filter",
        _ => "stop",
    }
}

/// Heuristic check for image generation prompts.
fn is_image_generation_prompt(text: &str) -> bool {
    let lower = text.to_lowercase();
    (lower.contains("generate") || lower.contains("create") || lower.contains("draw"))
        && (lower.contains("image") || lower.contains("picture") || lower.contains("photo"))
}

/// Clean a function name to match Gemini's `[a-zA-Z_][a-zA-Z0-9_]*` requirement.
pub fn clean_function_name(name: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());
    for (i, c) in name.chars().enumerate() {
        if i == 0 {
            if c.is_ascii_alphabetic() || c == '_' {
                cleaned.push(c);
            } else {
                cleaned.push('_');
            }
        } else if c.is_ascii_alphanumeric() || c == '_' {
            cleaned.push(c);
        } else {
            cleaned.push('_');
        }
    }
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_function_name_valid() {
        assert_eq!(clean_function_name("my_func"), "my_func");
    }

    #[test]
    fn clean_function_name_leading_digit() {
        assert_eq!(clean_function_name("123abc"), "_23abc");
    }

    #[test]
    fn clean_function_name_special_chars() {
        assert_eq!(clean_function_name("foo-bar.baz"), "foo_bar_baz");
    }

    #[test]
    fn clean_function_name_empty() {
        assert_eq!(clean_function_name(""), "_");
    }

    #[test]
    fn clean_function_name_underscore_start() {
        assert_eq!(clean_function_name("_private"), "_private");
    }

    #[test]
    fn image_generation_prompt_detected() {
        assert!(is_image_generation_prompt("Please generate an image of a cat"));
        assert!(is_image_generation_prompt("Create a picture of sunset"));
        assert!(is_image_generation_prompt("Draw me a photo"));
    }

    #[test]
    fn non_image_prompt_not_detected() {
        assert!(!is_image_generation_prompt("What is the weather?"));
        assert!(!is_image_generation_prompt("Generate a report"));
        assert!(!is_image_generation_prompt("Create a function"));
    }

    #[test]
    fn openai_to_gemini_basic_conversion() {
        let openai = json!({
            "model": "gemini-2.0-flash",
            "messages": [
                { "role": "user", "content": "Hello" }
            ],
            "temperature": 0.7,
            "max_tokens": 100
        });

        let gemini = openai_to_gemini_request(&openai);

        let contents = gemini["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
        assert_eq!(gemini["generationConfig"]["temperature"], 0.7);
        assert_eq!(gemini["generationConfig"]["maxOutputTokens"], 100);
    }

    #[test]
    fn openai_to_gemini_system_message() {
        let openai = json!({
            "messages": [
                { "role": "system", "content": "You are helpful" },
                { "role": "user", "content": "Hi" }
            ]
        });

        let gemini = openai_to_gemini_request(&openai);
        assert_eq!(gemini["systemInstruction"]["parts"][0]["text"], "You are helpful");
    }

    #[test]
    fn openai_to_gemini_tools_conversion() {
        let openai = json!({
            "messages": [{ "role": "user", "content": "Search" }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "web-search",
                    "description": "Search the web",
                    "parameters": { "type": "object" }
                }
            }]
        });

        let gemini = openai_to_gemini_request(&openai);
        let decls = &gemini["tools"][0]["functionDeclarations"];
        assert_eq!(decls[0]["name"], "web_search");
        assert_eq!(decls[0]["description"], "Search the web");
    }

    #[test]
    fn gemini_to_openai_basic_response() {
        let gemini = json!({
            "candidates": [{
                "content": {
                    "parts": [{ "text": "Hello there!" }],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        });

        let openai = gemini_to_openai_response(&gemini);
        assert_eq!(openai["choices"][0]["message"]["content"], "Hello there!");
        assert_eq!(openai["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn gemini_to_openai_empty_response() {
        let gemini = json!({});
        let openai = gemini_to_openai_response(&gemini);
        assert_eq!(openai["choices"][0]["message"]["content"], "");
    }

    #[test]
    fn constructs_with_correct_base_url() {
        let km = Arc::new(ApiKeyManager::new("test-key", None));
        let client = GeminiRotatingClient::new(km, "https://generativelanguage.googleapis.com", None, None);
        assert_eq!(client.base_url(), "https://generativelanguage.googleapis.com");
    }
}
