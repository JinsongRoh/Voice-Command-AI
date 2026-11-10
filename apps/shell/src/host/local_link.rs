//! "This PC": the desktop app's own link to the vcad on the same machine,
//! over the owner-only local control channel instead of pairing.
//!
//! To the webview it is one more machine (id [`LOCAL_MACHINE_ID`]) with the
//! same contract as a paired one (`vca_connect`, `vca_send`, status and
//! MessagePack envelopes). Only API messages pass: every envelope from the
//! webview is decoded as a typed `Envelope<ClientMsg>` and sent as
//! `{"op":"api"}`, so the local-only requests of the control channel
//! (pairing codes, TOTP enrollment, hooks, `vca run`, cloud credentials,
//! shutdown) are unreachable through this link. The local channel serves the
//! API without a device scope, exactly as for the `vca` CLI: whoever can open
//! the owner-only pipe or socket is the owner of this PC already.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use vca_client::{LinkSink, LinkState, LinkStatus, MachineInfo, Scope};
use vca_core::error::ErrorCode;
use vca_core::{ClientMsg, Envelope, ServerMsg};

use super::control::{self, Request, SecretStr};

/// Machine id of this PC. Paired machines use 16 hex characters, so the
/// two can never collide.
pub const LOCAL_MACHINE_ID: &str = "local";

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
const OUTBOUND: usize = 64;

/// The machine entry shown for this PC.
pub fn machine_info(name: &str) -> MachineInfo {
    MachineInfo {
        id: LOCAL_MACHINE_ID.to_owned(),
        name: name.to_owned(),
        endpoints: Vec::new(),
        device_id: LOCAL_MACHINE_ID.to_owned(),
        scope: Scope::Admin,
        paired_at: 0,
        alias: None,
        is_default: false,
        last_connected: None,
        server_fingerprint: String::new(),
    }
}

/// Why a send failed.
#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    /// Not a MessagePack `Envelope<ClientMsg>`.
    Protocol,
    /// The link is not connected right now.
    NotConnected,
}

struct Active {
    generation: u64,
    sink: Arc<dyn LinkSink>,
    status: LinkStatus,
    tx: Option<mpsc::Sender<String>>,
    stop: watch::Sender<bool>,
}

#[derive(Default)]
struct State {
    next_generation: u64,
    active: Option<Active>,
}

/// At most one supervised link (one webview).
pub struct LocalLink {
    endpoint: PathBuf,
    state: Arc<Mutex<State>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Encodes one webview envelope as a control-channel line.
#[cfg(test)]
pub fn encode_request(msgpack: &[u8]) -> Result<String, SendError> {
    encode_envelope(&decode_request(msgpack)?)
}

fn decode_request(msgpack: &[u8]) -> Result<Envelope<ClientMsg>, SendError> {
    // The decode error is not reported: it may quote message content.
    rmp_serde::from_slice(msgpack).map_err(|_| SendError::Protocol)
}

/// API messages vcad gates on this channel (`gated_api_target` there). The
/// token cannot travel in an API message.
fn needs_step_up(body: &ClientMsg) -> bool {
    matches!(
        body,
        ClientMsg::ConductorConfigure { .. }
            | ClientMsg::ConductorStart
            | ClientMsg::ConductorAllowSession { .. }
            | ClientMsg::ConductorDecide { allow: true, .. }
            | ClientMsg::TaskConfigure { .. }
    )
}

fn encode_envelope(env: &Envelope<ClientMsg>) -> Result<String, SendError> {
    let line = if let ClientMsg::SetDeviceScope { device_id, scope } = &env.body {
        // Widening a permission needs the local verification, which an API
        // message cannot carry.
        let held = super::security::held_token();
        serde_json::to_string(&Request::DeviceScope {
            id: env.id,
            device_id,
            scope: *scope,
            step_up: held.as_deref().map(SecretStr),
        })
    } else if needs_step_up(&env.body) {
        let held = super::security::held_token();
        serde_json::to_string(&Request::GatedApi {
            id: env.id,
            body: &env.body,
            step_up: held.as_deref().map(SecretStr),
        })
    } else {
        serde_json::to_string(&Request::Api {
            id: env.id,
            body: &env.body,
        })
    };
    line.map_err(|_| SendError::Protocol)
}

/// "Bring to front", clicked in this app: Windows hands the focus only to
/// the app the person is using, so this app passes that right on with the
/// documented `AllowSetForegroundWindow`, once (the next input ends it). vcad
/// then asks for the focus with `SetForegroundWindow`; Windows may still say
/// no, and vcad reports that. Nothing else is done to the focus.
///
/// The right goes to the bundled vcad's process only (`vcad_pid`), never to
/// every process (`ASFW_ANY`). Without that pid (a vcad this app did not
/// start) nothing is granted, and Windows will usually refuse.
#[cfg(windows)]
#[allow(unsafe_code)]
fn allow_foreground_once(vcad_pid: Option<u32>) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn AllowSetForegroundWindow(process_id: u32) -> i32;
    }
    let Some(pid) = vcad_pid else {
        return;
    };
    // SAFETY: takes no pointers; it only grants a one-time permission.
    unsafe {
        AllowSetForegroundWindow(pid);
    }
}

#[cfg(not(windows))]
fn allow_foreground_once(_vcad_pid: Option<u32>) {}

/// Turns one control-channel line into a MessagePack server envelope for
/// the webview; `None` for lines that are not API traffic or that this
/// build cannot decode (a newer server).
pub fn decode_response(line: &[u8]) -> Option<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    let id = value.get("id")?.as_u64()?;
    let body = match value.get("op")?.as_str()? {
        "api" => serde_json::from_value::<ServerMsg>(value.get("body")?.clone()).ok()?,
        // A failed request (for example an unparsable one) answers with a
        // local error; the webview expects an API error for that id.
        "error" if id != 0 => ServerMsg::Error {
            code: serde_json::from_value(value.get("code")?.clone()).unwrap_or(ErrorCode::Internal),
            message: value.get("message")?.as_str()?.to_owned(),
        },
        // The webview sees the error code; the nonce stays in this process.
        "step_up_required" => {
            let challenge = serde_json::from_value(value.get("challenge")?.clone()).ok()?;
            super::security::note_challenge(challenge);
            ServerMsg::Error {
                code: ErrorCode::StepUpRequired,
                message: "local verification needed".to_owned(),
            }
        }
        _ => return None,
    };
    rmp_serde::to_vec_named(&Envelope { id, body }).ok()
}

impl LocalLink {
    pub fn new(endpoint: PathBuf) -> Self {
        Self {
            endpoint,
            state: Arc::default(),
        }
    }

    /// Starts (or restarts) the supervised link; events go to `sink`.
    pub fn connect(&self, sink: Arc<dyn LinkSink>) {
        let (stop_tx, stop_rx) = watch::channel(false);
        let generation = {
            let mut state = lock(&self.state);
            if let Some(old) = state.active.take() {
                old.stop.send_replace(true);
            }
            state.next_generation += 1;
            let generation = state.next_generation;
            state.active = Some(Active {
                generation,
                sink: Arc::clone(&sink),
                status: LinkStatus::new(LinkState::Connecting),
                tx: None,
                stop: stop_tx,
            });
            generation
        };
        sink.status(LinkStatus::new(LinkState::Connecting));
        tauri::async_runtime::spawn(supervise(
            self.endpoint.clone(),
            Arc::clone(&self.state),
            generation,
            stop_rx,
        ));
    }

    pub fn status(&self) -> LinkStatus {
        lock(&self.state)
            .active
            .as_ref()
            .map(|a| a.status.clone())
            .unwrap_or_else(LinkStatus::disconnected)
    }

    pub async fn send_from(&self, msgpack: &[u8], vcad_pid: Option<u32>) -> Result<(), SendError> {
        let env = decode_request(msgpack)?;
        let line = encode_envelope(&env)?;
        let tx = lock(&self.state)
            .active
            .as_ref()
            .and_then(|a| a.tx.clone())
            .ok_or(SendError::NotConnected)?;
        if matches!(env.body, ClientMsg::FocusTerminal { .. }) {
            allow_foreground_once(vcad_pid);
        }
        tx.send(line).await.map_err(|_| SendError::NotConnected)
    }

    pub fn disconnect(&self) {
        if let Some(old) = lock(&self.state).active.take() {
            old.stop.send_replace(true);
        }
    }
}

/// Updates the status of `generation` (ignored once replaced) and reports it.
fn set_status(
    state: &Mutex<State>,
    generation: u64,
    status: LinkStatus,
    tx: Option<Option<mpsc::Sender<String>>>,
) -> bool {
    let sink = {
        let mut st = lock(state);
        let Some(active) = st.active.as_mut().filter(|a| a.generation == generation) else {
            return false;
        };
        if active.status == status && tx.is_none() {
            return true;
        }
        active.status = status.clone();
        if let Some(tx) = tx {
            active.tx = tx;
        }
        Arc::clone(&active.sink)
    };
    sink.status(status);
    true
}

fn sink_of(state: &Mutex<State>, generation: u64) -> Option<Arc<dyn LinkSink>> {
    lock(state)
        .active
        .as_ref()
        .filter(|a| a.generation == generation)
        .map(|a| Arc::clone(&a.sink))
}

async fn supervise(
    endpoint: PathBuf,
    state: Arc<Mutex<State>>,
    generation: u64,
    mut stop: watch::Receiver<bool>,
) {
    let mut delay = BACKOFF_MIN;
    loop {
        if *stop.borrow() {
            return;
        }
        match control::connect_stream(&endpoint, control::CONNECT_TIMEOUT).await {
            Ok(stream) => {
                delay = BACKOFF_MIN;
                let (tx, rx) = mpsc::channel::<String>(OUTBOUND);
                if !set_status(
                    &state,
                    generation,
                    LinkStatus::new(LinkState::Connected),
                    Some(Some(tx)),
                ) {
                    return;
                }
                let Some(sink) = sink_of(&state, generation) else {
                    return;
                };
                if serve(stream, rx, sink, &mut stop).await {
                    return;
                }
                let lost = LinkStatus::with_error(
                    LinkState::Reconnecting,
                    "unreachable",
                    "the connection to the VCA server on this PC was lost",
                );
                if !set_status(&state, generation, lost, Some(None)) {
                    return;
                }
            }
            Err(e) => {
                let status = LinkStatus::with_error(
                    LinkState::Reconnecting,
                    "unreachable",
                    format!("the VCA server on this PC is not running ({e})"),
                );
                if !set_status(&state, generation, status, None) {
                    return;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stopped(&mut stop) => return,
        }
        delay = (delay * 2).min(BACKOFF_MAX);
    }
}

/// Resolves once the link is stopped (or its controller is gone).
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// Pumps one connection. Returns true when stopped, false when it dropped.
async fn serve(
    stream: interprocess::local_socket::tokio::Stream,
    mut rx: mpsc::Receiver<String>,
    sink: Arc<dyn LinkSink>,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    let (recv, mut send) = tokio::io::split(stream);
    // Reading lines is not cancel safe, so it gets its own task.
    let mut reader = tokio::spawn(async move {
        let mut recv = BufReader::new(recv);
        let mut buf = Vec::new();
        loop {
            match control::read_raw_line(&mut recv, &mut buf).await {
                Ok(Some(())) => {
                    if let Some(bytes) = decode_response(buf.trim_ascii()) {
                        sink.message(bytes);
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::debug!("local link read failed: {e}");
                    return;
                }
            }
        }
    });
    let stopped = loop {
        tokio::select! {
            _ = stopped(stop) => break true,
            _ = &mut reader => break false,
            line = rx.recv() => match line {
                Some(line) => {
                    let mut bytes = line.into_bytes();
                    bytes.push(b'\n');
                    let written = async {
                        send.write_all(&bytes).await?;
                        send.flush().await
                    };
                    if let Err(e) = written.await {
                        tracing::debug!("local link write failed: {e}");
                        break false;
                    }
                }
                None => break true,
            },
        }
    };
    reader.abort();
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_core::Input;

    #[test]
    fn webview_envelopes_become_api_requests() {
        let env = Envelope {
            id: 7,
            body: ClientMsg::SendInput {
                session_id: "s1".into(),
                input: Input::Text {
                    text: "hi".into(),
                    submit: true,
                    at_prompt: false,
                },
            },
        };
        let line = encode_request(&rmp_serde::to_vec_named(&env).unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["op"], "api");
        assert_eq!(v["id"], 7);
        assert_eq!(v["body"]["type"], "send_input");
    }

    #[test]
    fn only_api_messages_pass() {
        // A control-channel request smuggled as MessagePack is not a ClientMsg.
        let pair = serde_json::json!({ "id": 1, "body": { "type": "pair", "scope": "admin" } });
        let bytes = rmp_serde::to_vec_named(&pair).unwrap();
        assert_eq!(encode_request(&bytes), Err(SendError::Protocol));
        assert_eq!(encode_request(b"\x00garbage"), Err(SendError::Protocol));
    }

    #[test]
    fn server_lines_become_msgpack_envelopes() {
        let line = br#"{"op":"api","id":0,"body":{"type":"pong","at":5}}"#;
        let bytes = decode_response(line).unwrap();
        let env: Envelope<ServerMsg> = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(env.id, 0);
        assert_eq!(env.body, ServerMsg::Pong { at: 5 });

        let line = br#"{"op":"error","id":9,"code":"invalid_request","message":"bad"}"#;
        let env: Envelope<ServerMsg> =
            rmp_serde::from_slice(&decode_response(line).unwrap()).unwrap();
        assert_eq!(env.id, 9);
        assert_eq!(
            env.body,
            ServerMsg::Error {
                code: ErrorCode::InvalidRequest,
                message: "bad".into()
            }
        );

        // Other local traffic and unknown messages are dropped.
        assert!(decode_response(br#"{"op":"done","id":3}"#).is_none());
        assert!(
            decode_response(br#"{"op":"api","id":1,"body":{"type":"from_the_future"}}"#).is_none()
        );
        assert!(
            decode_response(br#"{"op":"error","id":0,"code":"internal","message":"x"}"#).is_none()
        );
        assert!(decode_response(b"not json").is_none());
    }

    #[test]
    fn this_pc_is_an_admin_machine_with_a_reserved_id() {
        let info = machine_info("studio");
        assert_eq!(info.id, LOCAL_MACHINE_ID);
        assert!(!vca_client::is_valid_server_id(LOCAL_MACHINE_ID));
        assert_eq!(info.scope, Scope::Admin);
    }

    #[test]
    fn status_is_disconnected_until_connected() {
        let link = LocalLink::new(PathBuf::from("vca-test-none"));
        assert_eq!(link.status(), LinkStatus::disconnected());
    }

    #[test]
    fn gated_messages_carry_the_token_and_ordinary_ones_do_not() {
        let line = |body: ClientMsg| {
            let env = Envelope { id: 3, body };
            let line = encode_request(&rmp_serde::to_vec_named(&env).unwrap()).unwrap();
            serde_json::from_str::<serde_json::Value>(&line).unwrap()
        };
        assert_eq!(line(ClientMsg::ConductorStart)["op"], "gated_api");
        let allow = ClientMsg::ConductorDecide {
            request_id: "r".into(),
            allow: true,
        };
        assert_eq!(line(allow)["op"], "gated_api");
        let deny = ClientMsg::ConductorDecide {
            request_id: "r".into(),
            allow: false,
        };
        assert_eq!(line(deny)["op"], "api");
        assert_eq!(line(ClientMsg::ListSessions)["op"], "api");
    }

    #[test]
    fn a_challenge_reaches_the_webview_as_an_error_without_its_nonce() {
        let line = serde_json::json!({
            "op": "step_up_required",
            "id": 9,
            "challenge": { "nonce": "secret-nonce", "action": "conductor_setup", "expires_at": 1 }
        });
        let bytes = decode_response(line.to_string().as_bytes()).unwrap();
        let env: Envelope<ServerMsg> = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(env.id, 9);
        let ServerMsg::Error { code, message } = env.body else {
            panic!("not an error");
        };
        assert_eq!(code, ErrorCode::StepUpRequired);
        assert!(!message.contains("secret-nonce"));
    }
}
