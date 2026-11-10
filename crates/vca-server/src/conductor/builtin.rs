//! The built-in conductor: a small tool-calling loop against an
//! OpenAI-compatible LLM profile of `[[speech.llms]]` (Ollama, LM Studio,
//! vLLM, OpenAI, ...). Stateless per instruction apart from a short rolling
//! context of the conductor chat; the same tools and safety rules as the
//! `vca mcp` path (every call goes through [`Conductor::execute`]).

use std::time::Duration;

use serde_json::json;
use vca_core::Error;
use vca_core::error::ErrorCode;
use vca_speech::llm::{ChatClient, ChatConfig};

use super::{Conductor, setup, tools};
use crate::api::Api;
use crate::util::{error_text, truncate_chars};

/// Model when the profile names none.
pub const DEFAULT_MODEL: &str = "gpt-4o-mini";
const OPENAI_API: &str = "https://api.openai.com/v1";
/// LLM requests per instruction (each may call several tools).
pub const MAX_STEPS: usize = 6;
/// Chat lines given as context.
const CONTEXT_TURNS: usize = 6;
/// Longest tool result passed back to the model, in characters.
const MAX_TOOL_RESULT_CHARS: usize = 8000;

const BUILTIN_NOTE: &str = "\n\nYou are VCA's built-in conductor: the tools are offered as \
     functions. Call them only when needed, then answer in one to three short sentences.";

/// Builds the chat client of the chosen profile, with its key from the
/// environment or the credential store.
pub fn resolve(conductor: &Conductor) -> Result<ChatClient, Error> {
    let Some(id) = conductor.settings().builtin_llm else {
        return Err(Error::api(
            ErrorCode::InvalidRequest,
            "choose an LLM profile for the built-in conductor first",
        ));
    };
    let profile = conductor
        .llm_profiles()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| {
            Error::api(
                ErrorCode::NotFound,
                format!("no OpenAI-compatible LLM profile {id:?} in [[speech.llms]]"),
            )
        })?;
    let credential = profile
        .credential
        .clone()
        .unwrap_or_else(|| profile.id.clone());
    let secrets = crate::speech_profiles::Secrets::in_data_dir(conductor.data_dir());
    let key = crate::speech_profiles::env_secret(profile.api_key_env.as_deref())
        .or_else(|| secrets.get(&credential));
    let gateway = crate::speech_profiles::lookup_gateway(
        profile.gateway_token_env.as_deref(),
        &credential,
        &secrets,
    );
    let base_url = profile
        .base_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| OPENAI_API.to_owned());
    ChatClient::new(ChatConfig {
        base_url,
        api_key: key,
        model: profile
            .model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        timeout: Duration::from_millis(profile.timeout_ms.unwrap_or(60_000).max(1000)),
        auth: crate::speech_profiles::engine_auth(profile.api_key_header.as_deref(), gateway),
    })
    .map_err(|e| match e {
        vca_speech::SpeechError::Config(message) => Error::api(
            ErrorCode::InvalidRequest,
            format!("LLM profile {id:?}: {message}"),
        ),
        other => Error::api(ErrorCode::Internal, other.to_string()),
    })
}

/// Answers one instruction (or wake-up summary) and pushes the reply.
pub async fn run(conductor: &Conductor, api: &Api, input: &str) {
    let reply = answer(conductor, api, input).await;
    conductor.builtin_done();
    conductor.reply(&reply);
}

async fn answer(conductor: &Conductor, api: &Api, input: &str) -> String {
    let client = match resolve(conductor) {
        Ok(c) => c,
        Err(err) => return format!("The conductor's LLM is not available: {}", error_text(&err)),
    };
    let max_tokens = conductor.settings().builtin_max_tokens;
    let mut messages = vec![json!({
        "role": "system",
        "content": format!("{}{BUILTIN_NOTE}", setup::INSTRUCTIONS),
    })];
    for turn in conductor.recent_chat(CONTEXT_TURNS) {
        let role = if turn.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        messages.push(json!({"role": role, "content": turn.text}));
    }
    messages.push(json!({"role": "user", "content": input}));
    let tool_defs = tools::openai_tools();
    for _ in 0..MAX_STEPS {
        if !conductor.take_llm_call() {
            return "The conductor reached its hourly LLM limit.".to_owned();
        }
        let reply = match client
            .complete_with_tools(&messages, &tool_defs, max_tokens)
            .await
        {
            Ok(r) => r,
            Err(err) => return format!("The conductor's LLM failed: {err}"),
        };
        if reply.tool_calls.is_empty() {
            return reply
                .content
                .unwrap_or_else(|| "(no answer from the model)".to_owned());
        }
        messages.push(reply.message.clone());
        for call in reply.tool_calls {
            let content = match tools::parse_call(&call.name, &call.arguments) {
                Ok(parsed) => match conductor.execute(api, parsed.clone()).await {
                    Ok(result) => tools::label_result(&parsed, result),
                    Err(err) => json!({"error": error_text(&err)}),
                },
                Err(message) => json!({"error": message}),
            };
            let text = truncate_chars(&content.to_string(), MAX_TOOL_RESULT_CHARS, false);
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": text,
            }));
        }
    }
    format!("Stopped after {MAX_STEPS} steps; ask again with a narrower instruction.")
}
