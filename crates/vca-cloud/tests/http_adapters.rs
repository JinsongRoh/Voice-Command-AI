//! Wiremock tests for every HTTP adapter: request shapes, response parsing
//! and error mapping.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use vca_cloud::providers::{
    ClaudeManagedAdapter, ClaudeRoutineAdapter, CopilotAdapter, CursorAdapter, DevinAdapter,
    JulesAdapter,
};
use vca_cloud::{
    ActivityPoller, CloudAgentAdapter, CloudError, CloudLaunch, CloudSessionId, Credential, Cursor,
};
use vca_core::{MessageRole, SessionEvent, SessionState};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn launch(prompt: &str, repo: Option<&str>) -> CloudLaunch {
    CloudLaunch {
        prompt: prompt.into(),
        repository: repo.map(str::to_owned),
        branch: Some("main".into()),
        ..CloudLaunch::default()
    }
}

// ---------------------------------------------------------------- Jules

#[tokio::test]
async fn jules_create_list_message_activity() {
    let server = MockServer::start().await;
    let jules = JulesAdapter::with_base_url(server.uri());
    let cred = Credential::new("jules-key");

    Mock::given(method("POST"))
        .and(path("/sessions"))
        .and(header("X-Goog-Api-Key", "jules-key"))
        .and(body_json(json!({
            "prompt": "Fix tests",
            "sourceContext": {
                "source": "sources/github/octo/hello",
                "githubRepoContext": {"startingBranch": "main"}
            },
            "requirePlanApproval": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "sessions/31415", "id": "31415", "title": "Fix tests",
            "state": "QUEUED", "url": "https://jules.google.com/session/31415",
            "createTime": "2025-01-01T00:00:00Z", "futureField": {"x": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut req = launch("Fix tests", Some("https://github.com/octo/hello"));
    req.require_plan_approval = true;
    let s = jules.create_session(&cred, &req).await.unwrap();
    assert_eq!(s.id.0, "31415");
    assert_eq!(s.state, SessionState::Starting);
    assert_eq!(
        s.url.as_deref(),
        Some("https://jules.google.com/session/31415")
    );

    Mock::given(method("GET"))
        .and(path("/sessions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessions": [{"id": "1", "state": "COMPLETED"}, {"id": "2", "state": "IN_PROGRESS"}]
        })))
        .mount(&server)
        .await;
    let list = jules.list_sessions(&cred).await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].state, SessionState::Exited { code: Some(0) });

    Mock::given(method("POST"))
        .and(path("/sessions/31415:sendMessage"))
        .and(body_json(json!({"prompt": "also update docs"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    jules
        .send_message(&cred, &"31415".into(), "also update docs")
        .await
        .unwrap();

    Mock::given(method("POST"))
        .and(path("/sessions/31415:approvePlan"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    jules.approve_plan(&cred, &"31415".into()).await.unwrap();

    Mock::given(method("GET"))
        .and(path("/sessions/31415/activities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "activities": [
                {"id": "a1", "createTime": "2025-01-01T00:00:01Z", "originator": "user",
                 "userMessaged": {"userMessage": "Fix tests"}},
                {"id": "a2", "createTime": "2025-01-01T00:00:02Z", "originator": "agent",
                 "agentMessaged": {"agentMessage": "On it"}},
                {"id": "a3", "createTime": "2025-01-01T00:00:03Z", "originator": "agent",
                 "progressUpdated": {"title": "Running tests", "description": "cargo test"}}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sessions/31415"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "31415", "state": "AWAITING_USER_FEEDBACK"})),
        )
        .mount(&server)
        .await;

    let adapter: Arc<dyn CloudAgentAdapter> = Arc::new(jules);
    let mut poller = ActivityPoller::new(adapter, cred.clone(), "31415".into(), 1);
    let events = poller.poll_once().await.unwrap();
    assert_eq!(events.len(), 4);
    assert!(matches!(
        &events[0],
        SessionEvent::Message {
            seq: 1,
            role: MessageRole::User,
            ..
        }
    ));
    assert!(
        matches!(&events[1], SessionEvent::Message { seq: 2, role: MessageRole::Assistant, text, .. } if text == "On it")
    );
    assert!(matches!(
        &events[3],
        SessionEvent::StateChanged {
            seq: 4,
            state: SessionState::AwaitingInput,
            ..
        }
    ));
    // Second poll sees the same items: nothing new.
    assert!(poller.poll_once().await.unwrap().is_empty());
}

#[tokio::test]
async fn jules_activity_pagination_uses_page_token() {
    let server = MockServer::start().await;
    let jules = JulesAdapter::with_base_url(server.uri());
    let cred = Credential::new("k");
    Mock::given(method("GET"))
        .and(path("/sessions/9/activities"))
        .and(query_param("pageToken", "t2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"activities": [
                {"id": "b", "agentMessaged": {"agentMessage": "second"}}
            ]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sessions/9/activities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "activities": [{"id": "a", "agentMessaged": {"agentMessage": "first"}}],
            "nextPageToken": "t2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sessions/9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "9", "state": "IN_PROGRESS"})),
        )
        .mount(&server)
        .await;
    let first = jules.activity(&cred, &"9".into(), None).await.unwrap();
    assert!(first.has_more);
    assert_eq!(first.next, Some(Cursor("t2".into())));
    let second = jules
        .activity(&cred, &"9".into(), first.next)
        .await
        .unwrap();
    assert!(!second.has_more);
    assert_eq!(
        second.next,
        Some(Cursor("t2".into())),
        "resume from the last page"
    );
    assert_eq!(second.state, Some(SessionState::Running));
}

#[tokio::test]
async fn jules_errors_are_typed() {
    let server = MockServer::start().await;
    let jules = JulesAdapter::with_base_url(server.uri());
    Mock::given(method("GET"))
        .and(path("/sessions/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": 404, "message": "Session not found", "status": "NOT_FOUND"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sessions"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "7"))
        .mount(&server)
        .await;
    let cred = Credential::new("k");
    let err = jules
        .get_session(&cred, &"missing".into())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CloudError::NotFound(ref m) if m == "Session not found"),
        "{err:?}"
    );
    let err = jules.list_sessions(&cred).await.unwrap_err();
    assert!(
        matches!(err, CloudError::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(7))
    );
    // No credential: rejected before any request.
    let err = jules.list_sessions(&Credential::none()).await.unwrap_err();
    assert!(matches!(err, CloudError::Auth(_)));
}

// --------------------------------------------------------------- Cursor

#[tokio::test]
async fn cursor_launch_followup_conversation_stop() {
    let server = MockServer::start().await;
    let cursor = CursorAdapter::with_base_url(server.uri());
    let cred = Credential::new("cur_key");
    // Basic auth with the key as user name and an empty password.
    let basic = "Basic Y3VyX2tleTo=";

    Mock::given(method("POST"))
        .and(path("/v0/agents"))
        .and(header("Authorization", basic))
        .and(body_json(json!({
            "prompt": {"text": "Add tests"},
            "source": {"repository": "https://github.com/octo/hello", "ref": "main"},
            "target": {"autoCreatePr": true}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "bc_abc123", "name": "Add tests", "status": "CREATING",
            "source": {"repository": "https://github.com/octo/hello", "ref": "main"},
            "target": {"branchName": "cursor/add-tests", "url": "https://cursor.com/agents?id=bc_abc123", "autoCreatePr": true},
            "createdAt": "2025-01-01T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut req = launch("Add tests", Some("octo/hello"));
    req.auto_create_pr = true;
    let s = cursor.create_session(&cred, &req).await.unwrap();
    assert_eq!(s.id.0, "bc_abc123");
    assert_eq!(s.state, SessionState::Starting);
    assert_eq!(
        s.url.as_deref(),
        Some("https://cursor.com/agents?id=bc_abc123")
    );

    Mock::given(method("POST"))
        .and(path("/v0/agents/bc_abc123/followup"))
        .and(body_json(json!({"prompt": {"text": "and docs"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "bc_abc123"})))
        .expect(1)
        .mount(&server)
        .await;
    cursor
        .send_message(&cred, &"bc_abc123".into(), "and docs")
        .await
        .unwrap();

    Mock::given(method("GET"))
        .and(path("/v0/agents/bc_abc123/conversation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "bc_abc123",
            "messages": [
                {"id": "m1", "type": "user_message", "text": "Add tests"},
                {"id": "m2", "type": "assistant_message", "text": "Done"}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/agents/bc_abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "bc_abc123", "status": "FINISHED",
            "target": {"prUrl": "https://github.com/octo/hello/pull/5"}
        })))
        .mount(&server)
        .await;
    let page = cursor
        .activity(&cred, &"bc_abc123".into(), None)
        .await
        .unwrap();
    assert_eq!(page.activities.len(), 2);
    assert_eq!(page.state, Some(SessionState::Exited { code: Some(0) }));
    let got = cursor
        .get_session(&cred, &"bc_abc123".into())
        .await
        .unwrap();
    assert_eq!(
        got.pull_request_url.as_deref(),
        Some("https://github.com/octo/hello/pull/5")
    );

    Mock::given(method("POST"))
        .and(path("/v0/agents/bc_abc123/stop"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "bc_abc123"})))
        .expect(1)
        .mount(&server)
        .await;
    cursor.stop(&cred, &"bc_abc123".into()).await.unwrap();
}

#[tokio::test]
async fn cursor_list_paginates_and_maps_auth_error() {
    let server = MockServer::start().await;
    let cursor = CursorAdapter::with_base_url(server.uri())
        .with_auth(vca_cloud::providers::CursorAuth::Bearer);
    Mock::given(method("GET"))
        .and(path("/v0/agents"))
        .and(query_param("cursor", "c2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"agents": [{"id": "b", "status": "ERROR"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/agents"))
        .and(header("Authorization", "Bearer good"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "agents": [{"id": "a", "status": "RUNNING"}], "nextCursor": "c2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/agents"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "Invalid API key"})))
        .mount(&server)
        .await;
    let list = cursor
        .list_sessions(&Credential::new("good"))
        .await
        .unwrap();
    assert_eq!(list.len(), 2);
    assert!(matches!(list[1].state, SessionState::Error { .. }));
    let err = cursor
        .list_sessions(&Credential::new("bad"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, CloudError::Auth(ref m) if m == "Invalid API key"),
        "{err:?}"
    );
}

// ---------------------------------------------------------------- Devin

#[tokio::test]
async fn devin_session_lifecycle() {
    let server = MockServer::start().await;
    let devin = DevinAdapter::with_base_url(server.uri(), "org-1");
    let cred = Credential::new("cog_secret");
    let base = "/v3/organizations/org-1/sessions";

    Mock::given(method("POST"))
        .and(path(base))
        .and(header("Authorization", "Bearer cog_secret"))
        .and(body_json(
            json!({"prompt": "Refactor", "repos": ["octo/hello"]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "session_id": "devin-abc", "url": "https://app.devin.ai/sessions/abc", "status": "new"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let s = devin
        .create_session(
            &cred,
            &CloudLaunch {
                prompt: "Refactor".into(),
                repository: Some("octo/hello".into()),
                ..CloudLaunch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(s.id.0, "devin-abc");
    assert_eq!(s.state, SessionState::Starting);

    Mock::given(method("POST"))
        .and(path(format!("{base}/devin-abc/messages")))
        .and(body_json(json!({"message": "status?"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    devin
        .send_message(&cred, &"devin-abc".into(), "status?")
        .await
        .unwrap();

    Mock::given(method("GET"))
        .and(path(format!("{base}/devin-abc/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"event_id": "e1", "source": "user", "message": "Refactor", "created_at": "2025-01-01T00:00:00Z"},
                {"event_id": "e2", "source": "devin", "message": "Working on it", "created_at": "2025-01-01T00:00:05Z"}
            ],
            "end_cursor": "e2", "has_next_page": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{base}/devin-abc")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "session_id": "devin-abc", "status": "running", "status_detail": "waiting_for_user"
        })))
        .mount(&server)
        .await;
    let page = devin
        .activity(&cred, &"devin-abc".into(), None)
        .await
        .unwrap();
    assert_eq!(page.activities.len(), 2);
    assert!(!page.has_more);
    assert_eq!(page.state, Some(SessionState::AwaitingInput));

    Mock::given(method("GET"))
        .and(path(base))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"session_id": "devin-abc", "status": "exit"}], "has_next_page": false
        })))
        .mount(&server)
        .await;
    let list = devin.list_sessions(&cred).await.unwrap();
    assert_eq!(list[0].state, SessionState::Exited { code: Some(0) });

    Mock::given(method("DELETE"))
        .and(path(format!("{base}/devin-abc")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    devin.stop(&cred, &"devin-abc".into()).await.unwrap();
}

#[tokio::test]
async fn devin_server_error_is_transient() {
    let server = MockServer::start().await;
    let devin = DevinAdapter::with_base_url(server.uri(), "org-1");
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
        .mount(&server)
        .await;
    let err = devin
        .list_sessions(&Credential::new("cog_x"))
        .await
        .unwrap_err();
    assert!(matches!(err, CloudError::Provider { status: 503, .. }));
    assert!(err.is_transient());
}

// -------------------------------------------------------------- Copilot

#[tokio::test]
async fn copilot_start_and_track_task() {
    let server = MockServer::start().await;
    let copilot = CopilotAdapter::with_base_url(server.uri());
    let cred = Credential::new("ghp_token");
    let task = json!({
        "id": "t-1",
        "url": "https://api.github.com/agents/repos/octo/hello/tasks/t-1",
        "html_url": "https://github.com/octo/hello/copilot/tasks/t-1",
        "name": "Fix the login button", "state": "queued", "session_count": 1,
        "artifacts": [], "archived_at": null,
        "created_at": "2025-01-01T00:00:00Z", "updated_at": "2025-01-01T00:00:00Z"
    });
    Mock::given(method("POST"))
        .and(path("/agents/repos/octo/hello/tasks"))
        .and(header("Authorization", "Bearer ghp_token"))
        .and(header("X-GitHub-Api-Version", "2022-11-28"))
        .and(body_json(json!({"prompt": "Fix the login button", "create_pull_request": false, "base_ref": "main"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(task.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let s = copilot
        .create_session(&cred, &launch("Fix the login button", Some("octo/hello")))
        .await
        .unwrap();
    assert_eq!(s.id.0, "t-1");
    assert_eq!(s.repository.as_deref(), Some("octo/hello"));
    assert_eq!(s.state, SessionState::Starting);

    Mock::given(method("GET"))
        .and(path("/agents/tasks/t-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "t-1", "state": "waiting_for_user",
            "sessions": [{"id": "s-1", "state": "waiting_for_user", "prompt": "Fix the login button",
                          "created_at": "2025-01-01T00:00:00Z"}]
        })))
        .mount(&server)
        .await;
    let page = copilot.activity(&cred, &"t-1".into(), None).await.unwrap();
    assert_eq!(page.state, Some(SessionState::AwaitingInput));
    assert_eq!(page.activities.len(), 2);

    Mock::given(method("GET"))
        .and(path("/agents/tasks"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"tasks": [task], "total_active_count": 1})),
        )
        .mount(&server)
        .await;
    assert_eq!(copilot.list_sessions(&cred).await.unwrap().len(), 1);

    let err = copilot
        .send_message(&cred, &"t-1".into(), "x")
        .await
        .unwrap_err();
    assert!(matches!(err, CloudError::Unsupported(_)));
    assert!(!copilot.capabilities().follow_up);
}

#[tokio::test]
async fn copilot_validation_and_forbidden() {
    let server = MockServer::start().await;
    let copilot = CopilotAdapter::with_base_url(server.uri());
    let cred = Credential::new("t");
    let err = copilot
        .create_session(&cred, &launch("x", None))
        .await
        .unwrap_err();
    assert!(matches!(err, CloudError::InvalidRequest(_)));
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "message": "Resource not accessible by integration", "documentation_url": "https://docs.github.com"
        })))
        .mount(&server)
        .await;
    let err = copilot
        .create_session(&cred, &launch("x", Some("o/r")))
        .await
        .unwrap_err();
    assert!(matches!(err, CloudError::Auth(ref m) if m.contains("not accessible")));
}

// ------------------------------------------------ Claude Managed Agents

#[tokio::test]
async fn claude_managed_session_flow() {
    let server = MockServer::start().await;
    let claude = ClaudeManagedAdapter::new("agent_abc", "env_abc")
        .with_base_url(server.uri())
        .with_workspace("default");
    let cred = Credential::new("sk-ant-api-key");

    Mock::given(method("POST"))
        .and(path("/v1/sessions"))
        .and(header("x-api-key", "sk-ant-api-key"))
        .and(header("anthropic-beta", "managed-agents-2026-04-01"))
        .and(header("anthropic-version", "2026-10-21"))
        .and(body_json(json!({
            "agent": "agent_abc",
            "environment_id": "env_abc",
            "initial_events": [{"type": "user.message", "content": [{"type": "text", "text": "Review auth"}]}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "session", "id": "sesn_1", "status": "running", "title": null,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
            "environment_id": "env_abc", "agent": {}, "resources": [], "metadata": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let s = claude
        .create_session(
            &cred,
            &CloudLaunch {
                prompt: "Review auth".into(),
                ..CloudLaunch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(s.id.0, "sesn_1");
    assert_eq!(s.state, SessionState::Running);
    assert_eq!(
        s.url.as_deref(),
        Some("https://platform.claude.com/workspaces/default/sessions/sesn_1")
    );

    Mock::given(method("POST"))
        .and(path("/v1/sessions/sesn_1/events"))
        .and(body_json(json!({"events": [{"type": "user.interrupt"}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/sessions/sesn_1/events"))
        .and(body_json(json!({"events": [{"type": "user.message", "content": [{"type": "text", "text": "more"}]}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(1)
        .mount(&server)
        .await;
    claude
        .send_message(&cred, &"sesn_1".into(), "more")
        .await
        .unwrap();
    claude.stop(&cred, &"sesn_1".into()).await.unwrap();

    Mock::given(method("GET"))
        .and(path("/v1/sessions/sesn_1/events"))
        .and(query_param("page", "page_2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"type": "session.status_idle", "id": "sevt_4", "stop_reason": {"type": "end_turn"}, "processed_at": "2026-01-01T00:00:04Z"}
            ],
            "next_page": null
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/sessions/sesn_1/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"type": "user.message", "id": "sevt_1", "content": [{"type": "text", "text": "Review auth"}], "processed_at": "2026-01-01T00:00:00Z"},
                {"type": "session.status_running", "id": "sevt_2", "processed_at": "2026-01-01T00:00:01Z"},
                {"type": "span.model_request_start", "id": "sevt_x", "processed_at": "2026-01-01T00:00:01Z"},
                {"type": "agent.message", "id": "sevt_3", "content": [{"type": "text", "text": "Looks fine"}], "processed_at": "2026-01-01T00:00:02Z"}
            ],
            "next_page": "page_2"
        })))
        .mount(&server)
        .await;
    let adapter: Arc<dyn CloudAgentAdapter> = Arc::new(claude);
    let mut poller = ActivityPoller::new(adapter, cred, CloudSessionId::from("sesn_1"), 0);
    let events = poller.poll_once().await.unwrap();
    let seqs: Vec<u64> = events.iter().map(SessionEvent::seq).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3]);
    assert!(matches!(
        &events[3],
        SessionEvent::StateChanged {
            state: SessionState::Idle,
            ..
        }
    ));
    assert_eq!(poller.state().cursor, Some(Cursor("page_2".into())));
}

#[tokio::test]
async fn claude_managed_error_envelope() {
    let server = MockServer::start().await;
    let claude = ClaudeManagedAdapter::new("a", "e").with_base_url(server.uri());
    Mock::given(method("GET"))
        .and(path("/v1/sessions/nope"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "type": "error", "error": {"type": "not_found_error", "message": "session not found"}, "request_id": "req_1"
        })))
        .mount(&server)
        .await;
    let err = claude
        .get_session(&Credential::new("k"), &"nope".into())
        .await
        .unwrap_err();
    assert!(matches!(err, CloudError::NotFound(ref m) if m == "session not found"));
}

// ------------------------------------------------- Claude Code routines

#[tokio::test]
async fn routine_fire_and_limits() {
    let server = MockServer::start().await;
    let routine = ClaudeRoutineAdapter::new("trig_123").with_base_url(server.uri());
    let cred = Credential::new("sk-ant-oat01-token");
    Mock::given(method("POST"))
        .and(path("/v1/claude_code/routines/trig_123/fire"))
        .and(header("Authorization", "Bearer sk-ant-oat01-token"))
        .and(header("anthropic-version", "2026-10-21"))
        .and(body_json(json!({"text": "CI failed on main"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "routine_fire",
            "claude_code_session_id": "session_01HJK",
            "claude_code_session_url": "https://claude.ai/code/session_01HJK"
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/claude_code/routines/trig_123/fire"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "120")
                .set_body_json(json!({"type": "error", "error": {"type": "rate_limit_error", "message": "limit"}})),
        )
        .mount(&server)
        .await;
    let req = CloudLaunch {
        prompt: "CI failed on main".into(),
        ..CloudLaunch::default()
    };
    let s = routine.create_session(&cred, &req).await.unwrap();
    assert_eq!(s.id.0, "session_01HJK");
    assert_eq!(
        s.url.as_deref(),
        Some("https://claude.ai/code/session_01HJK")
    );
    let err = routine.create_session(&cred, &req).await.unwrap_err();
    assert!(matches!(err, CloudError::RateLimited { retry_after: Some(d) } if d.as_secs() == 120));

    let caps = routine.capabilities();
    assert!(caps.create && !caps.list && !caps.follow_up && !caps.progress);
    assert!(matches!(
        routine.list_sessions(&cred).await.unwrap_err(),
        CloudError::Unsupported(_)
    ));
    let too_long = CloudLaunch {
        prompt: "x".repeat(65_537),
        ..CloudLaunch::default()
    };
    assert!(matches!(
        routine.create_session(&cred, &too_long).await.unwrap_err(),
        CloudError::InvalidRequest(_)
    ));
}
