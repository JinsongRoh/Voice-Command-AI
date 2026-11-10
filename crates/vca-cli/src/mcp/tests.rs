//! The MCP server against a fake vcad (a real local socket) and a fake MCP
//! client (in-memory pipes). No agent and no vcad is started.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use vca_core::error::ErrorCode;
use vca_server::conductor::{ConductorCall, UNTRUSTED_NOTICE};
use vca_server::control::{LocalRequest, LocalResponse, read_line, split_stream, write_line};

use super::*;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A vcad that answers conductor calls with an echo of the call.
async fn fake_vcad() -> (
    PathBuf,
    tokio::task::JoinHandle<()>,
    Option<tempfile::TempDir>,
) {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let (endpoint, dir) = if cfg!(windows) {
        (
            PathBuf::from(format!(r"\\.\pipe\vca-mcp-test-{}-{n}", std::process::id())),
            None,
        )
    } else {
        let dir = tempfile::tempdir().unwrap();
        (dir.path().join("control.sock"), Some(dir))
    };
    let listener = ListenerOptions::new()
        .name(endpoint.as_path().to_fs_name::<GenericFilePath>().unwrap())
        .create_tokio()
        .unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok(stream) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let (recv, mut send) = split_stream(stream);
                let mut recv = tokio::io::BufReader::new(recv);
                let mut buf = Vec::new();
                while let Ok(Some(req)) = read_line::<_, LocalRequest>(&mut recv, &mut buf).await {
                    let LocalRequest::Conductor { id, call, .. } = req else {
                        continue;
                    };
                    let resp = match &call {
                        ConductorCall::SendToSession { text, .. } if text == "deny me" => {
                            LocalResponse::Conductor {
                                id,
                                result: json!({"outcome": "denied", "session": "api"}),
                            }
                        }
                        ConductorCall::Interrupt { session_id } if session_id == "shell" => {
                            LocalResponse::error(
                                id,
                                ErrorCode::Forbidden,
                                "remote shells are never conductor targets",
                            )
                        }
                        _ => LocalResponse::Conductor {
                            id,
                            result: json!({"echo": serde_json::to_value(&call).unwrap()}),
                        },
                    };
                    if write_line(&mut send, &resp).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (endpoint, task, dir)
}

/// A connected MCP client.
struct McpClient {
    write: tokio::io::DuplexStream,
    read: BufReader<tokio::io::DuplexStream>,
    next: u64,
}

impl McpClient {
    fn start<B: Backend>(backend: B) -> Self {
        let (client_w, server_r) = tokio::io::duplex(1 << 20);
        let (server_w, client_r) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            serve(BufReader::new(server_r), server_w, backend)
                .await
                .unwrap();
        });
        Self {
            write: client_w,
            read: BufReader::new(client_r),
            next: 1,
        }
    }

    async fn raw(&mut self, line: &str) {
        self.write.write_all(line.as_bytes()).await.unwrap();
        self.write.write_all(b"\n").await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), self.read.read_line(&mut line))
            .await
            .expect("an answer")
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.raw(&msg.to_string()).await;
        let reply = self.recv().await;
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    async fn initialize(&mut self) -> Value {
        let reply = self
            .request(
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "test", "version": "1"}}),
            )
            .await;
        self.raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        reply
    }

    async fn call(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": args}))
            .await
    }
}

#[tokio::test]
async fn initialize_ping_and_unknown_methods() {
    let (endpoint, _vcad, _dir) = fake_vcad().await;
    let mut c = McpClient::start(Vcad::new(&endpoint));
    let init = c.initialize().await;
    let r = &init["result"];
    assert_eq!(r["protocolVersion"], "2025-06-18");
    assert_eq!(r["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(r["serverInfo"]["name"], "vca");
    assert!(r["instructions"].as_str().unwrap().contains("untrusted"));
    // An older revision is accepted; an unknown one gets ours.
    let old = c
        .request("initialize", json!({"protocolVersion": "2024-11-05"}))
        .await;
    assert_eq!(old["result"]["protocolVersion"], "2024-11-05");
    let future = c
        .request("initialize", json!({"protocolVersion": "2099-01-01"}))
        .await;
    assert_eq!(future["result"]["protocolVersion"], PROTOCOL_VERSION);

    assert_eq!(c.request("ping", Value::Null).await["result"], json!({}));
    let missing = c.request("resources/list", json!({})).await;
    assert_eq!(missing["error"]["code"], METHOD_NOT_FOUND);
    c.raw("this is not json").await;
    assert_eq!(c.recv().await["error"]["code"], PARSE_ERROR);
    c.raw(r#"[{"jsonrpc":"2.0","id":9,"method":"ping"}]"#).await;
    assert_eq!(c.recv().await["error"]["code"], INVALID_REQUEST);
}

#[tokio::test]
async fn tools_list_decision_tools_are_escalate_and_rule_bound_answer() {
    let (endpoint, _vcad, _dir) = fake_vcad().await;
    let mut c = McpClient::start(Vcad::new(&endpoint));
    c.initialize().await;
    let list = c.request("tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names[..11],
        [
            "list_sessions",
            "session_status",
            "session_summary",
            "pending_decisions",
            "briefing",
            "recent_activity",
            "send_to_session",
            "restore_windows",
            "focus_terminal",
            "interrupt",
            "new_session",
        ]
    );
    assert_eq!(names[11..13], ["escalate_decision", "answer_decision"]);
    // The to-do list tools of `vca_server::tasks::mcp` follow; none of them
    // completes a task or answers a decision.
    assert_eq!(names.len(), tool_specs().len());
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        let name = t["name"].as_str().unwrap();
        // vcad allows these two only under the person's allow rules.
        if matches!(name, "escalate_decision" | "answer_decision") {
            continue;
        }
        for word in ["approve", "answer", "decide", "choose"] {
            assert!(!name.contains(word), "{name}");
        }
    }
    let read_only: Vec<bool> = tools[..11]
        .iter()
        .map(|t| t["annotations"]["readOnlyHint"].as_bool().unwrap())
        .collect();
    assert_eq!(read_only.iter().filter(|r| **r).count(), 6);
    // An answer without its option is refused before vcad.
    let r = c
        .call(
            "answer_decision",
            json!({"session_id": "a", "option_id": "y"}),
        )
        .await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    // A tool that does not exist is not a tool.
    let r = c.call("approve_decision", json!({"session_id": "a"})).await;
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
}

#[tokio::test]
async fn every_tool_reaches_vcad_and_session_content_is_untrusted() {
    let (endpoint, _vcad, _dir) = fake_vcad().await;
    let mut c = McpClient::start(Vcad::new(&endpoint));
    c.initialize().await;
    let cases = [
        ("list_sessions", json!({}), true),
        ("session_status", json!({"session_id": "abc"}), true),
        (
            "session_summary",
            json!({"session_id": "abc", "items": 5}),
            true,
        ),
        ("pending_decisions", json!({}), true),
        ("briefing", json!({"language": "ko"}), true),
        ("recent_activity", json!({"since_minutes": 10}), true),
        (
            "send_to_session",
            json!({"session_id": "abc", "text": "run tests"}),
            true,
        ),
        ("restore_windows", json!({"profile": "work"}), false),
        ("focus_terminal", json!({"session_id": "abc"}), false),
        ("interrupt", json!({"session_id": "abc"}), false),
        (
            "new_session",
            json!({"agent": "codex", "folder": "/src/app"}),
            false,
        ),
    ];
    for (name, args, untrusted) in cases {
        let r = c.call(name, args.clone()).await;
        let result = &r["result"];
        assert_eq!(result["isError"], false, "{name}: {r}");
        let structured = &result["structuredContent"];
        let echo = if untrusted {
            assert_eq!(structured["notice"], UNTRUSTED_NOTICE, "{name}");
            &structured["untrusted_output"]["echo"]
        } else {
            assert!(structured.get("untrusted_output").is_none(), "{name}");
            &structured["result"]["echo"]
        };
        assert_eq!(echo["tool"], name, "{name}: {r}");
        for (k, v) in args.as_object().unwrap() {
            assert_eq!(&echo[k], v, "{name}.{k}");
        }
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), *structured);
    }
}

#[tokio::test]
async fn refusals_and_bad_arguments_are_tool_errors() {
    let (endpoint, _vcad, _dir) = fake_vcad().await;
    let mut c = McpClient::start(Vcad::new(&endpoint));
    c.initialize().await;
    let r = c.call("interrupt", json!({"session_id": "shell"})).await;
    assert_eq!(r["result"]["isError"], true);
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("remote shells")
    );
    let r = c.call("send_to_session", json!({"session_id": "x"})).await;
    assert_eq!(r["result"]["isError"], true);
    let r = c
        .call(
            "send_to_session",
            json!({"session_id": "x", "text": "deny me"}),
        )
        .await;
    assert_eq!(
        r["result"]["structuredContent"]["untrusted_output"]["outcome"],
        "denied"
    );
}

#[tokio::test]
async fn vcad_unreachable_is_a_tool_error() {
    let endpoint = if cfg!(windows) {
        PathBuf::from(r"\\.\pipe\vca-mcp-test-nobody-here")
    } else {
        PathBuf::from("/nonexistent/vca/control.sock")
    };
    let mut c = McpClient::start(Vcad::new(&endpoint));
    c.initialize().await;
    let r = c.call("list_sessions", json!({})).await;
    assert_eq!(r["result"]["isError"], true);
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cannot reach vcad")
    );
}

/// A slow call (a send waiting for the person) does not block others.
#[derive(Clone)]
struct Slow;

impl Backend for Slow {
    async fn call(&self, call: ConductorCall) -> Result<Value, String> {
        if matches!(call, ConductorCall::SendToSession { .. }) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok(json!({"tool": call.name()}))
    }
}

#[tokio::test]
async fn calls_run_concurrently() {
    let mut c = McpClient::start(Slow);
    c.initialize().await;
    let send = json!({"jsonrpc": "2.0", "id": 100, "method": "tools/call",
        "params": {"name": "send_to_session", "arguments": {"session_id": "a", "text": "x"}}});
    c.raw(&send.to_string()).await;
    let list = c.call("list_sessions", json!({})).await;
    assert_eq!(list["result"]["isError"], false);
    let later = c.recv().await;
    assert_eq!(later["id"], 100);
}
