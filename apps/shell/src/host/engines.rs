//! Speech engine setup of this PC's own vcad (desktop only): the speech
//! recognition engines, transcript-correction LLMs and TTS engines in its
//! `config.toml`, their keys and a "Test" request.
//!
//! Everything goes over vcad's owner-only local control channel, which is
//! the only place vcad accepts these requests (paired devices get
//! `forbidden`). The webview gets typed commands, never the channel. A key
//! typed in the app travels once to vcad, which stores it in its credential
//! store; vcad never sends it back, and nothing here logs it.
//!
//! Commands: `vca_host_engines`, `vca_host_engine_save`,
//! `vca_host_engine_remove`, `vca_host_engine_set_default` and
//! `vca_host_engine_test`. Answers are vcad's JSON (`EngineSetup` and
//! `EngineTestResult` in `vca_server::engines`).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::Host;
use super::control::{ControlConn, Request, Response, SecretStr};
use crate::{CommandError, CommandResult};

/// vcad gives a test 25 s; wait a little longer for its answer.
const TEST_TIMEOUT: Duration = Duration::from_secs(35);
/// Longest accepted key.
const MAX_SECRET_CHARS: usize = 4096;

/// Which list a profile belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Stt,
    Llm,
    Tts,
}

impl EngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Llm => "llm",
            Self::Tts => "tts",
        }
    }
}

/// A key from the webview: trimmed, `None` when empty, one bounded line.
fn clean_secret(secret: Option<String>) -> CommandResult<Option<String>> {
    let Some(secret) = secret
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    if secret.chars().count() > MAX_SECRET_CHARS || secret.chars().any(char::is_control) {
        return Err(CommandError::new(
            "invalid_request",
            format!("a key is one line of at most {MAX_SECRET_CHARS} characters"),
        ));
    }
    Ok(Some(secret))
}

fn setup_of(resp: Response) -> CommandResult<serde_json::Value> {
    match resp {
        Response::Engines { setup, .. } => Ok(setup),
        _ => Err(CommandError::new(
            "protocol",
            "unexpected answer to an engine request",
        )),
    }
}

impl Host {
    async fn engine_call<'a>(
        &self,
        make: impl FnOnce(u64) -> Request<'a>,
        timeout: Option<Duration>,
    ) -> CommandResult<Response> {
        let mut conn = ControlConn::connect(&self.endpoint).await?;
        let id = conn.next_id();
        let req = make(id);
        let resp = match timeout {
            Some(t) => conn.call_timeout(&req, t).await?,
            None => conn.call(&req).await?,
        };
        Ok(resp)
    }
}

type HostState<'a> = State<'a, std::sync::Arc<Host>>;

#[tauri::command]
pub async fn vca_host_engines(host: HostState<'_>) -> CommandResult<serde_json::Value> {
    setup_of(
        host.engine_call(|id| Request::EnginesGet { id }, None)
            .await?,
    )
}

/// `gateway_token` stores or replaces the gateway token of an engine behind
/// an authenticating gateway; `clear_gateway_token` deletes it.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn vca_host_engine_save(
    host: HostState<'_>,
    kind: EngineKind,
    original_id: Option<String>,
    profile: serde_json::Value,
    secret: Option<String>,
    make_default: bool,
    gateway_token: Option<String>,
    clear_gateway_token: Option<bool>,
) -> CommandResult<serde_json::Value> {
    let secret = clean_secret(secret)?;
    let gateway_token = clean_secret(gateway_token)?;
    let held = super::security::held_token();
    setup_of(
        host.engine_call(
            |id| Request::EngineSave {
                id,
                kind: kind.as_str(),
                original_id: original_id.as_deref(),
                profile: &profile,
                secret: secret.as_deref().map(SecretStr),
                make_default,
                gateway_token: gateway_token.as_deref().map(SecretStr),
                clear_gateway_token: clear_gateway_token.unwrap_or(false),
                step_up: held.as_deref().map(SecretStr),
            },
            None,
        )
        .await?,
    )
}

#[tauri::command]
pub async fn vca_host_engine_remove(
    host: HostState<'_>,
    kind: EngineKind,
    profile_id: String,
    delete_key: bool,
) -> CommandResult<serde_json::Value> {
    setup_of(
        host.engine_call(
            |id| Request::EngineRemove {
                id,
                kind: kind.as_str(),
                profile_id: &profile_id,
                delete_key,
            },
            None,
        )
        .await?,
    )
}

#[tauri::command]
pub async fn vca_host_engine_set_default(
    host: HostState<'_>,
    kind: EngineKind,
    profile_id: Option<String>,
) -> CommandResult<serde_json::Value> {
    setup_of(
        host.engine_call(
            |id| Request::EngineSetDefault {
                id,
                kind: kind.as_str(),
                profile_id: profile_id.as_deref(),
            },
            None,
        )
        .await?,
    )
}

#[tauri::command]
pub async fn vca_host_engine_test(
    host: HostState<'_>,
    kind: EngineKind,
    profile: Option<serde_json::Value>,
    profile_id: Option<String>,
    secret: Option<String>,
    gateway_token: Option<String>,
) -> CommandResult<serde_json::Value> {
    let secret = clean_secret(secret)?;
    let gateway_token = clean_secret(gateway_token)?;
    let resp = host
        .engine_call(
            |id| Request::EngineTest {
                id,
                kind: kind.as_str(),
                profile: profile.as_ref(),
                profile_id: profile_id.as_deref(),
                secret: secret.as_deref().map(SecretStr),
                gateway_token: gateway_token.as_deref().map(SecretStr),
            },
            Some(TEST_TIMEOUT),
        )
        .await?;
    match resp {
        Response::EngineTest { result, .. } => Ok(result),
        _ => Err(CommandError::new(
            "protocol",
            "unexpected answer to engine_test",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_match_vcad_and_hide_the_key() {
        let profile = json!({ "id": "local", "kind": "openai_compatible", "base_url": "http://127.0.0.1:8000/v1" });
        let req = Request::EngineSave {
            id: 2,
            kind: EngineKind::Stt.as_str(),
            original_id: None,
            profile: &profile,
            secret: Some(SecretStr("sk-hidden-1")),
            make_default: true,
            gateway_token: Some(SecretStr("gw-hidden-2")),
            clear_gateway_token: false,
            step_up: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(
            v,
            json!({
                "op": "engine_save", "id": 2, "kind": "stt", "original_id": null,
                "profile": profile, "secret": "sk-hidden-1", "make_default": true,
                "gateway_token": "gw-hidden-2", "clear_gateway_token": false
            })
        );
        assert!(!format!("{req:?}").contains("sk-hidden-1"));
        assert!(!format!("{req:?}").contains("gw-hidden-2"));
        assert_eq!(
            serde_json::to_value(Request::EnginesGet { id: 1 }).unwrap(),
            json!({ "op": "engines_get", "id": 1 })
        );
        let v = serde_json::to_value(Request::EngineTest {
            id: 3,
            kind: "tts",
            profile: None,
            profile_id: Some("melo"),
            secret: None,
            gateway_token: None,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({ "op": "engine_test", "id": 3, "kind": "tts", "profile": null, "profile_id": "melo", "secret": null, "gateway_token": null })
        );
        let v = serde_json::to_value(Request::EngineRemove {
            id: 4,
            kind: "llm",
            profile_id: "x",
            delete_key: true,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({ "op": "engine_remove", "id": 4, "kind": "llm", "profile_id": "x", "delete_key": true })
        );
        let v = serde_json::to_value(Request::EngineSetDefault {
            id: 5,
            kind: "llm",
            profile_id: None,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({ "op": "engine_set_default", "id": 5, "kind": "llm", "profile_id": null })
        );
    }

    #[test]
    fn answers_from_vcad() {
        let r: Response = serde_json::from_value(json!({
            "op": "engines", "id": 1,
            "setup": { "config_path": "c", "stt": [], "llm": [], "tts": [], "restart_required": false }
        }))
        .unwrap();
        assert_eq!(r.id(), Some(1));
        assert_eq!(setup_of(r).unwrap()["config_path"], "c");
        let r: Response = serde_json::from_value(json!({
            "op": "engine_test", "id": 2, "result": { "ok": true, "latency_ms": 12, "message": "m" }
        }))
        .unwrap();
        assert!(matches!(r, Response::EngineTest { .. }));
        assert_eq!(setup_of(r).unwrap_err().code, "protocol");
    }

    #[test]
    fn keys_are_cleaned() {
        assert_eq!(clean_secret(None).unwrap(), None);
        assert_eq!(clean_secret(Some("  ".into())).unwrap(), None);
        assert_eq!(
            clean_secret(Some(" sk-1 \n".into())).unwrap().as_deref(),
            Some("sk-1")
        );
        assert!(clean_secret(Some("a\nb".into())).is_err());
        assert!(clean_secret(Some("x".repeat(MAX_SECRET_CHARS + 1))).is_err());
        assert_eq!(
            serde_json::from_value::<EngineKind>(json!("tts")).unwrap(),
            EngineKind::Tts
        );
        assert!(serde_json::from_value::<EngineKind>(json!("asr")).is_err());
    }
}
