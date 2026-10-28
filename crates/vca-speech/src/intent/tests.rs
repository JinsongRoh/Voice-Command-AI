use super::*;

fn router() -> IntentRouter {
    IntentRouter::new()
}

fn sessions() -> IntentContext {
    IntentContext {
        sessions: vec![
            SessionAlias {
                id: "s-1".into(),
                number: Some(1),
                names: vec!["frontend".into(), "web-app".into()],
            },
            SessionAlias {
                id: "s-2".into(),
                number: Some(2),
                names: vec!["backend".into(), "api server".into()],
            },
            SessionAlias {
                id: "s-3".into(),
                number: Some(3),
                names: vec!["Claude".into()],
            },
        ],
        templates: vec!["run tests".into(), "open PR".into()],
        pending_decision: false,
    }
}

fn expect_control(intent: &Intent) -> (ControlAction, Option<Target>, f32) {
    match intent {
        Intent::Control {
            action,
            target,
            confidence,
        } => (*action, target.clone(), *confidence),
        other => panic!("expected control, got {other:?}"),
    }
}

fn assert_control(text: &str, ctx: &IntentContext, action: ControlAction, target: Option<Target>) {
    let intent = router().route(text, ctx);
    let (a, t, c) = expect_control(&intent);
    assert_eq!((a, t), (action, target), "utterance: {text}");
    assert!(c >= 0.6, "low confidence {c} for {text}");
}

fn assert_prompt(text: &str, ctx: &IntentContext) {
    let intent = router().route(text, ctx);
    assert!(
        matches!(intent, Intent::Prompt { .. }),
        "expected prompt for {text:?}, got {intent:?}"
    );
}

fn assert_ambiguous(text: &str, ctx: &IntentContext) -> Vec<Intent> {
    match router().route(text, ctx) {
        Intent::Ambiguous { candidates } => candidates,
        other => panic!("expected ambiguous for {text:?}, got {other:?}"),
    }
}

use ControlAction::*;

fn s(id: &str) -> Option<Target> {
    Some(Target::Session(id.into()))
}

#[test]
fn english_commands() {
    let ctx = sessions();
    assert_control("pause session three", &ctx, PauseSession, s("s-3"));
    assert_control("Pause session 2, please.", &ctx, PauseSession, s("s-2"));
    assert_control("stop the backend", &ctx, StopSession, s("s-2"));
    assert_control("restart it", &ctx, RestartSession, Some(Target::Current));
    assert_control("resume", &ctx, ResumeSession, None);
    assert_control("switch to frontend", &ctx, SelectSession, s("s-1"));
    assert_control("go to the api server", &ctx, SelectSession, s("s-2"));
    assert_control("session 1", &ctx, SelectSession, s("s-1"));
    assert_control("show resources", &ctx, ShowResources, None);
    assert_control("what's the memory usage", &ctx, ShowResources, None);
    assert_control("read the last output", &ctx, ReadLastOutput, None);
    assert_control(
        "run template run tests",
        &ctx,
        RunTemplate,
        Some(Target::Template("run tests".into())),
    );
}

#[test]
fn english_prompts() {
    let ctx = sessions();
    assert_prompt("fix the login bug in the auth module", &ctx);
    assert_prompt("stop using var in the test files", &ctx);
    assert_prompt("open the readme and add an install section", &ctx);
    assert_prompt("no, use the other approach with a hash map", &ctx);
    assert_prompt("", &ctx);
}

#[test]
fn approvals_depend_on_pending_decision() {
    let mut ctx = sessions();
    let c = assert_ambiguous("yes", &ctx);
    assert!(matches!(
        c[0],
        Intent::Control {
            action: Approve,
            ..
        }
    ));
    assert!(matches!(c[1], Intent::Prompt { .. }));
    ctx.pending_decision = true;
    assert_control("yes", &ctx, Approve, None);
    assert_control("approve", &ctx, Approve, None);
    assert_control("deny", &ctx, Deny, None);
    assert_control("no", &ctx, Deny, None);
    assert_control("approve session 2", &ctx, Approve, s("s-2"));
}

#[test]
fn unknown_targets_are_ambiguous() {
    let ctx = sessions();
    let c = assert_ambiguous("stop the server", &ctx);
    assert!(matches!(
        &c[0],
        Intent::Control { action: StopSession, target: Some(Target::SessionName(n)), .. } if n == "server"
    ));
    // Without a session directory, an explicit "session" word is enough.
    let empty = IntentContext::default();
    assert_control(
        "pause session docs",
        &empty,
        PauseSession,
        Some(Target::SessionName("docs".into())),
    );
    assert_control(
        "pause session 4",
        &empty,
        PauseSession,
        Some(Target::SessionNumber(4)),
    );
    let c = assert_ambiguous("stop and restart", &ctx);
    assert_eq!(c.len(), 2);
}

#[test]
fn japanese() {
    let ctx = sessions();
    assert_control("セッション3を一時停止して", &ctx, PauseSession, s("s-3"));
    assert_control("セッション2を停止してください", &ctx, StopSession, s("s-2"));
    assert_control("リソースを表示", &ctx, ShowResources, None);
    assert_control("最後の出力を読んで", &ctx, ReadLastOutput, None);
    assert_control(
        "二番のセッションに切り替えて",
        &ctx,
        SelectSession,
        s("s-2"),
    );
    assert_prompt("ログイン画面のバグを直してテストを追加して", &ctx);
}

#[test]
fn korean() {
    let ctx = sessions();
    assert_control("세션 3 일시정지 해줘", &ctx, PauseSession, s("s-3"));
    assert_control("2번 세션 재시작", &ctx, RestartSession, s("s-2"));
    assert_control("세 번째 세션으로 전환", &ctx, SelectSession, s("s-3"));
    assert_control("Claude 세션 멈춰", &ctx, PauseSession, s("s-3"));
    assert_control("리소스 보여줘", &ctx, ShowResources, None);
    assert_prompt("로그인 버그 고치고 테스트 추가해줘", &ctx);
    // A Korean word must not be matched inside another word: "네" (yes)
    // inside "네트워크" (network).
    assert_prompt("네트워크 설정 파일을 정리해줘", &ctx);
}

#[test]
fn chinese() {
    let ctx = sessions();
    assert_control("暂停会话2", &ctx, PauseSession, s("s-2"));
    assert_control("请重启第三个会话", &ctx, RestartSession, s("s-3"));
    assert_control("显示资源", &ctx, ShowResources, None);
    assert_control("切换到一号会话", &ctx, SelectSession, s("s-1"));
    assert_prompt("帮我修复登录页面的错误并添加测试", &ctx);
}

#[test]
fn spanish_german_french_portuguese() {
    let ctx = sessions();
    assert_control("pausa la sesión dos", &ctx, PauseSession, s("s-2"));
    assert_control(
        "reinicia la sesión 1 por favor",
        &ctx,
        RestartSession,
        s("s-1"),
    );
    assert_control("muestra los recursos", &ctx, ShowResources, None);
    assert_prompt("crea un test para el login y arregla el bug", &ctx);
    assert_control("Sitzung drei pausieren", &ctx, PauseSession, s("s-3"));
    assert_control("Sitzung 2 neu starten", &ctx, RestartSession, s("s-2"));
    assert_control("lies die letzte Ausgabe", &ctx, ReadLastOutput, None);
    assert_control("mets en pause la session 1", &ctx, PauseSession, s("s-1"));
    assert_control("arrête la session trois", &ctx, StopSession, s("s-3"));
    assert_control("pausa a sessão 2", &ctx, PauseSession, s("s-2"));
}

#[test]
fn mixed_language_utterances() {
    let ctx = sessions();
    assert_control("세션 2 pause 해줘", &ctx, PauseSession, s("s-2"));
    assert_control("session 3を停止して", &ctx, StopSession, s("s-3"));
    assert_control("frontend 세션으로 전환", &ctx, SelectSession, s("s-1"));
    assert_control("backend を再起動", &ctx, RestartSession, s("s-2"));
    assert_control("pausa la session 2", &ctx, PauseSession, s("s-2"));
    assert_control("重启 backend", &ctx, RestartSession, s("s-2"));
    // A mixed-language prompt stays a prompt.
    assert_prompt("useEffect の依存配列を修正して lint を通して", &ctx);
}

#[test]
fn templates() {
    let ctx = sessions();
    assert_control(
        "use template open PR",
        &ctx,
        RunTemplate,
        Some(Target::Template("open PR".into())),
    );
    assert_control(
        "テンプレートを実行 run tests",
        &ctx,
        RunTemplate,
        Some(Target::Template("run tests".into())),
    );
    assert_control(
        "run the open PR template",
        &ctx,
        RunTemplate,
        Some(Target::Template("open PR".into())),
    );
    let c = assert_ambiguous("run template deploy to staging", &ctx);
    assert!(matches!(c[1], Intent::Prompt { .. }));
}

#[test]
fn custom_language_table() {
    let mut r = IntentRouter::with_languages(vec![]);
    r.add_language_toml(
        r#"
language = "it"
session_words = ["sessione"]
fillers = ["la", "per favore"]
[actions]
pause = ["metti in pausa"]
[numbers]
due = 2
"#,
    )
    .unwrap();
    let intent = r.route("metti in pausa la sessione due per favore", &sessions());
    assert_eq!(expect_control(&intent).0, PauseSession);
    assert!(r.add_language_toml("not toml [").is_err());
}

#[test]
fn session_action_mapping() {
    assert_eq!(
        PauseSession.session_action(),
        Some(vca_core::SessionAction::Pause)
    );
    assert_eq!(ShowResources.session_action(), None);
}
