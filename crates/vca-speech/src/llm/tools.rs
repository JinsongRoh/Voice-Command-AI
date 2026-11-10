//! Tool calling through an OpenAI-compatible chat completions endpoint, for
//! VCA's built-in conductor. One request per call: the caller keeps the
//! (short) conversation and runs the tools itself.

use serde_json::{Value, json};

use super::ChatClient;
use crate::error::{Result, SpeechError};
use crate::stt::openai::extract_error_message;

/// One function call the model asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The arguments as the model sent them (a JSON object, or `{}` when
    /// they did not parse).
    pub arguments: Value,
}

/// The assistant's answer: text, tool calls, or both.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolReply {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// The assistant message exactly as returned, to send back in the next
    /// request.
    pub message: Value,
}

/// Reads the first choice of a chat completions answer.
pub fn parse_tool_reply(body: &str) -> Result<ToolReply> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| SpeechError::Decode(e.to_string()))?;
    let message = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .cloned()
        .ok_or_else(|| SpeechError::Decode("no message in the answer".into()))?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.trim().is_empty());
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|c| {
                    let f = c.get("function")?;
                    let name = f.get("name")?.as_str()?.to_owned();
                    let arguments = match f.get("arguments") {
                        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({})),
                        Some(v @ Value::Object(_)) => v.clone(),
                        _ => json!({}),
                    };
                    Some(ToolCall {
                        id: c
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("call")
                            .to_owned(),
                        name,
                        arguments,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ToolReply {
        content,
        tool_calls,
        message,
    })
}

impl ChatClient {
    /// Sends `messages` (OpenAI chat format) with function `tools` and
    /// returns the assistant's answer. Temperature 0, no streaming,
    /// `max_tokens` caps the answer.
    pub async fn complete_with_tools(
        &self,
        messages: &[Value],
        tools: &[Value],
        max_tokens: u32,
    ) -> Result<ToolReply> {
        let mut body = json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0,
            "stream": false,
            "max_tokens": max_tokens,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
            body["tool_choice"] = json!("auto");
        }
        // Resent only when no connection was made (never billed twice).
        let resp = crate::retry::send_unconnected_retry(|| {
            self.authorized(self.client.post(self.endpoint()).json(&body))
                .send()
        })
        .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(SpeechError::Api {
                status: status.as_u16(),
                message: extract_error_message(&text),
            });
        }
        parse_tool_reply(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_calls_and_text() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"list_sessions",
            "arguments":"{\"a\":1}"}},{"id":"c2","type":"function","function":{"name":"x",
            "arguments":"not json"}}]}}]}"#;
        let r = parse_tool_reply(body).unwrap();
        assert_eq!(r.content, None);
        assert_eq!(r.tool_calls.len(), 2);
        assert_eq!(r.tool_calls[0].name, "list_sessions");
        assert_eq!(r.tool_calls[0].arguments, json!({"a": 1}));
        assert_eq!(r.tool_calls[1].arguments, json!({}));
        let r = parse_tool_reply(r#"{"choices":[{"message":{"content":"done"}}]}"#).unwrap();
        assert_eq!(r.content.as_deref(), Some("done"));
        assert!(r.tool_calls.is_empty());
        assert!(parse_tool_reply("{}").is_err());
    }
}
