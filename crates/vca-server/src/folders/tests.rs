//! Folder picker tests. Everything runs in temp dirs; the real user
//! folders and agent transcripts are never listed.

use std::sync::Arc;

use tokio::sync::mpsc;
use vca_core::{
    AgentKind, ClientMsg, ControlTier, Envelope, SessionId, SessionState, protocol::HostOs,
};
use vca_net::Scope;

use super::*;
use crate::api::{Api, Client, DeviceIdentity};
use crate::events::LogLimits;
use crate::sessions::{ManagerConfig, SessionManager};

/// The canonical display form of an existing path.
fn canon(path: &Path) -> String {
    without_verbatim_prefix(std::fs::canonicalize(path).unwrap())
        .to_str()
        .unwrap()
        .to_owned()
}

fn mkdir(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    path.to_owned()
}

fn session(id: &str, dir: &Path, last_activity: UnixMillis) -> SessionInfo {
    SessionInfo {
        id: SessionId(id.into()),
        origin: SessionOrigin::Local {
            agent: AgentKind::ClaudeCode,
            project_dir: dir.to_owned(),
            pid: None,
        },
        display_name: id.into(),
        state: SessionState::Idle,
        control: ControlTier::ObserveOnly,
        profile: None,
        created_at: 0,
        last_activity,
        unread: 0,
        preview: None,
        auto_name: None,
        custom_name: false,
        shell: None,
        pending_decision: None,
        subagents: 0,
        conductor: false,
        terminal_prompt: None,
        question: None,
        progress: None,
        problem: None,
    }
}

fn no_sources() -> TranscriptSources {
    TranscriptSources::default()
}

fn folders(root: &Path) -> Folders {
    Folders::new(WorkspaceRoots::new([root.to_owned()]), no_sources())
}

fn entries(msg: &ServerMsg) -> Vec<String> {
    match msg {
        ServerMsg::Folder { entries, .. } => entries.iter().map(|e| e.name.clone()).collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn roots_and_recent_filter_outside_and_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    let a = mkdir(&root.join("alpha"));
    let b = mkdir(&root.join("beta"));
    mkdir(&b.join(".git"));
    let c = mkdir(&root.join("gamma"));
    let outside = mkdir(&tmp.path().join("outside"));
    let missing = root.join("gone");

    // A Claude Code transcript recording `beta`, a Codex rollout `gamma`,
    // and one Codex rollout outside the roots.
    let claude = mkdir(&tmp.path().join("claude/projects"));
    let enc = mkdir(&claude.join("some-encoded-name"));
    let line = serde_json::json!({"type": "user", "cwd": b, "sessionId": "s"});
    std::fs::write(enc.join("s.jsonl"), format!("{line}\n")).unwrap();
    let codex = mkdir(&tmp.path().join("codex/sessions/2026/10/30"));
    for (i, dir) in [&c, &outside].into_iter().enumerate() {
        let meta = serde_json::json!({"type": "session_meta",
            "payload": {"id": format!("id{i}"), "cwd": dir}});
        std::fs::write(
            codex.join(format!("rollout-2026-10-30T10-00-0{i}-x.jsonl")),
            format!("{meta}\n"),
        )
        .unwrap();
    }
    let f = Folders::new(
        WorkspaceRoots::new([root.clone(), tmp.path().join("no-such-root")]),
        TranscriptSources {
            claude_projects: Some(claude),
            codex_sessions: Some(tmp.path().join("codex/sessions")),
        },
    );
    let sessions = [
        session("s1", &a, 100),
        session("s2", &outside, 200),
        session("s3", &missing, 300),
        // The same folder twice is listed once.
        session("s4", &a, 50),
    ];
    let ServerMsg::ProjectFolders {
        roots,
        recent,
        os,
        separator,
    } = f.project_folders(&sessions)
    else {
        panic!("wrong reply");
    };
    assert_eq!(os, HostOs::current());
    assert_eq!(separator, std::path::MAIN_SEPARATOR_STR);
    // The missing root is not offered.
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, canon(&root));
    assert_eq!(roots[0].name, canon(&root), "roots show their full path");
    let paths: Vec<_> = recent.iter().map(|e| e.path.clone()).collect();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert!(paths.contains(&canon(&b)) && paths.contains(&canon(&c)));
    // Transcripts are newer than the sessions here; `alpha` comes last.
    assert_eq!(paths[2], canon(&a));
    let beta = recent.iter().find(|e| e.name == "beta").unwrap();
    assert!(beta.is_git_repo && beta.has_agent_history && beta.last_used.is_some());
    assert!(!recent.iter().any(|e| e.path.contains("outside")));
}

#[test]
fn recent_is_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    let sessions: Vec<_> = (0..40u64)
        .map(|i| session(&format!("s{i}"), &mkdir(&root.join(format!("p{i:02}"))), i))
        .collect();
    let ServerMsg::ProjectFolders { recent, .. } = folders(&root).project_folders(&sessions) else {
        panic!("wrong reply");
    };
    assert_eq!(recent.len(), MAX_RECENT);
    assert_eq!(recent[0].name, "p39", "most recent first");
}

#[test]
fn lists_folders_inside_the_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    let app = mkdir(&root.join("App"));
    mkdir(&app.join(".git"));
    mkdir(&root.join("zeta"));
    mkdir(&root.join("beta/inner"));
    std::fs::write(root.join("notes.txt"), "never listed").unwrap();
    let f = folders(&root);
    let sessions = [session("s1", &root.join("zeta"), 1)];
    let msg = f
        .list_folder(root.to_str().unwrap(), false, &sessions)
        .unwrap();
    assert_eq!(entries(&msg), ["App", "beta", "zeta"]);
    let ServerMsg::Folder {
        path,
        parent,
        root: at_root,
        breadcrumbs,
        entries: listed,
        truncated,
        ..
    } = msg
    else {
        unreachable!()
    };
    assert_eq!(path, canon(&root));
    assert_eq!(at_root, canon(&root));
    assert_eq!(parent, None, "browsing never leaves the root");
    assert_eq!(breadcrumbs.len(), 1);
    assert!(!truncated);
    assert!(listed[0].is_git_repo && !listed[1].is_git_repo);
    assert!(listed[2].has_agent_history && !listed[0].has_agent_history);
    assert_eq!(listed[1].path, canon(&root.join("beta")));

    // One level down: parent and breadcrumbs from the server.
    let inner = f
        .list_folder(&canon(&root.join("beta")), false, &[])
        .unwrap();
    let ServerMsg::Folder {
        name,
        parent,
        breadcrumbs,
        ..
    } = &inner
    else {
        unreachable!()
    };
    assert_eq!(name, "beta");
    assert_eq!(parent.as_deref(), Some(canon(&root).as_str()));
    let crumbs: Vec<_> = breadcrumbs.iter().map(|c| c.path.clone()).collect();
    assert_eq!(crumbs, [canon(&root), canon(&root.join("beta"))]);
    assert_eq!(breadcrumbs[1].name, "beta");
    assert_eq!(entries(&inner), ["inner"]);
    // `..` inside the root resolves.
    let up = f
        .list_folder(root.join("beta/inner/..").to_str().unwrap(), false, &[])
        .unwrap();
    assert_eq!(entries(&up), ["inner"]);
}

#[test]
fn refuses_outside_relative_and_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    let outside = mkdir(&tmp.path().join("outside"));
    let f = folders(&root);
    let code = |p: &str| f.list_folder(p, false, &[]).unwrap_err().code();
    assert_eq!(code(outside.to_str().unwrap()), ErrorCode::Forbidden);
    assert_eq!(code(tmp.path().to_str().unwrap()), ErrorCode::Forbidden);
    assert_eq!(
        code(root.join("../outside").to_str().unwrap()),
        ErrorCode::Forbidden
    );
    assert_eq!(code("relative/dir"), ErrorCode::InvalidRequest);
    assert_eq!(
        code(root.join("missing").to_str().unwrap()),
        ErrorCode::NotFound
    );
    std::fs::write(root.join("file"), "x").unwrap();
    assert_eq!(
        code(root.join("file").to_str().unwrap()),
        ErrorCode::InvalidRequest
    );
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_refused_and_hidden() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    let outside = mkdir(&tmp.path().join("outside"));
    mkdir(&outside.join("secret"));
    let inside = mkdir(&root.join("real"));
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    std::os::unix::fs::symlink(&inside, root.join("alias")).unwrap();
    let f = folders(&root);
    let err = f
        .list_folder(root.join("escape").to_str().unwrap(), false, &[])
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    let listed = f.list_folder(root.to_str().unwrap(), false, &[]).unwrap();
    assert_eq!(entries(&listed), ["alias", "real"]);
}

#[cfg(windows)]
#[test]
fn junction_escape_is_refused_and_hidden() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    let outside = mkdir(&tmp.path().join("outside"));
    mkdir(&root.join("real"));
    let link = root.join("escape");
    // Junctions need no special rights; skip when the shell cannot make one.
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .is_ok_and(|o| o.status.success());
    if !made || !link.exists() {
        eprintln!("skipped: cannot create a junction here");
        return;
    }
    let f = folders(&root);
    let err = f
        .list_folder(link.to_str().unwrap(), false, &[])
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Forbidden);
    let listed = f.list_folder(root.to_str().unwrap(), false, &[]).unwrap();
    assert_eq!(entries(&listed), ["real"]);
}

#[test]
fn hidden_and_system_folders_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    for name in [
        "src",
        ".hidden",
        ".git",
        "node_modules",
        "target",
        "$RECYCLE.BIN",
        "System Volume Information",
    ] {
        mkdir(&root.join(name));
    }
    let f = folders(&root);
    let path = root.to_str().unwrap();
    assert_eq!(entries(&f.list_folder(path, false, &[]).unwrap()), ["src"]);
    // `show_hidden` still never lists system folders.
    assert_eq!(
        entries(&f.list_folder(path, true, &[]).unwrap()),
        [".git", ".hidden", "node_modules", "src", "target"]
    );
}

#[cfg(windows)]
#[test]
fn windows_hidden_attribute_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    let secret = mkdir(&root.join("AppData"));
    mkdir(&root.join("Documents"));
    let hidden = std::process::Command::new("attrib")
        .arg("+h")
        .arg(&secret)
        .output()
        .is_ok_and(|o| o.status.success());
    if !hidden {
        eprintln!("skipped: attrib is not available");
        return;
    }
    let f = folders(&root);
    let path = root.to_str().unwrap();
    assert_eq!(
        entries(&f.list_folder(path, false, &[]).unwrap()),
        ["Documents"]
    );
    assert_eq!(
        entries(&f.list_folder(path, true, &[]).unwrap()),
        ["AppData", "Documents"]
    );
}

#[test]
fn large_folders_are_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    for i in 0..MAX_ENTRIES + 5 {
        mkdir(&root.join(format!("d{i:04}")));
    }
    let msg = folders(&root)
        .list_folder(root.to_str().unwrap(), false, &[])
        .unwrap();
    let ServerMsg::Folder {
        entries, truncated, ..
    } = msg
    else {
        unreachable!()
    };
    assert_eq!(entries.len(), MAX_ENTRIES);
    assert!(truncated);
}

#[test]
fn folder_path_is_built_and_checked_by_the_server() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    mkdir(&root.join("exists"));
    let outside = mkdir(&tmp.path().join("outside"));
    let f = folders(&root);
    let parent = canon(&root);
    let ServerMsg::FolderPath { path, exists } = f.folder_path(&parent, " new-app ").unwrap()
    else {
        panic!()
    };
    assert!(!exists);
    assert_eq!(PathBuf::from(&path), PathBuf::from(&parent).join("new-app"));
    assert!(!root.join("new-app").exists(), "nothing is created");
    let ServerMsg::FolderPath { exists, .. } = f.folder_path(&parent, "exists").unwrap() else {
        panic!()
    };
    assert!(exists);
    for bad in ["", "  ", ".", "..", "a/b", r"a\b", "../escape", "x\u{7}"] {
        assert_eq!(
            f.folder_path(&parent, bad).unwrap_err().code(),
            ErrorCode::InvalidRequest,
            "{bad:?}"
        );
    }
    if cfg!(windows) {
        for bad in ["CON", "nul.txt", "COM1", "a:b", "what?", "trailing."] {
            assert_eq!(
                f.folder_path(&parent, bad).unwrap_err().code(),
                ErrorCode::InvalidRequest,
                "{bad:?}"
            );
        }
    }
    assert_eq!(
        f.folder_path(outside.to_str().unwrap(), "x")
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
}

#[test]
fn rate_limit_is_per_client() {
    let f = folders(Path::new("/"));
    for _ in 0..RATE_BURST as usize {
        f.check_rate("a").unwrap();
    }
    let err = f.check_rate("a").unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unavailable);
    f.check_rate("b").unwrap();
}

#[cfg(windows)]
#[test]
fn windows_paths_use_drive_letters_and_backslashes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    mkdir(&root.join("proj"));
    let f = folders(&root);
    let ServerMsg::ProjectFolders {
        roots,
        os,
        separator,
        ..
    } = f.project_folders(&[])
    else {
        panic!()
    };
    assert_eq!(os, HostOs::Windows);
    assert_eq!(separator, "\\");
    let r = &roots[0].path;
    assert!(!r.starts_with(r"\\?\"), "{r}");
    assert_eq!(&r[1..3], r":\", "{r}");
    // A forward-slash path from a client works too.
    let slashed = r.replace('\\', "/");
    let msg = f.list_folder(&slashed, false, &[]).unwrap();
    let ServerMsg::Folder { path, entries, .. } = msg else {
        panic!()
    };
    assert_eq!(&path, r);
    assert!(entries[0].path.ends_with(r"\proj"), "{}", entries[0].path);
    // A drive root as workspace root: listed, with no parent.
    let drive = PathBuf::from(&r[..3]);
    let f = folders(&drive);
    let ServerMsg::Folder {
        parent,
        breadcrumbs,
        name,
        ..
    } = f.list_folder(&r[..3], false, &[]).unwrap()
    else {
        panic!()
    };
    assert_eq!(parent, None);
    assert_eq!(name, &r[..3]);
    assert_eq!(breadcrumbs.len(), 1);
}

#[cfg(unix)]
#[test]
fn posix_paths_use_slashes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    mkdir(&root.join("proj"));
    let f = folders(&root);
    let ServerMsg::ProjectFolders {
        roots,
        os,
        separator,
        ..
    } = f.project_folders(&[])
    else {
        panic!()
    };
    assert_ne!(os, HostOs::Windows);
    assert_eq!(separator, "/");
    assert!(roots[0].path.starts_with('/'));
    let msg = f.list_folder(&roots[0].path, false, &[]).unwrap();
    let ServerMsg::Folder { entries, .. } = msg else {
        panic!()
    };
    assert!(entries[0].path.ends_with("/proj"));
}

// --- Through the API: scope and the `Welcome` feature. ---

type Conn = (Arc<Client>, mpsc::Receiver<Envelope<ServerMsg>>);

fn api(root: &Path) -> Api {
    let mgr = SessionManager::new(
        ManagerConfig {
            limits: LogLimits {
                max_events: 100,
                max_bytes: 1 << 20,
            },
            exited_retention: Duration::from_secs(60),
            pause_scope: vca_broker::contain::Scope::ChildrenOnly,
            heuristics_interval: Duration::from_millis(500),
            control_endpoint: root.join("ctl"),
            attach_dir: None,
            workspace_roots: WorkspaceRoots::new([root.to_owned()]),
            session_names: None,
            allow_generic_launch: false,
        },
        None,
    );
    Api::new(mgr, "test".into(), Duration::from_secs(60))
}

async fn ask(api: &Api, conn: &mut Conn, body: ClientMsg) -> ServerMsg {
    api.handle(&conn.0, Envelope { id: 9, body }).await;
    loop {
        let env = conn.1.recv().await.unwrap();
        if env.id == 9 {
            return env.body;
        }
    }
}

#[tokio::test]
async fn api_scope_and_welcome_feature() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mkdir(&tmp.path().join("root"));
    mkdir(&root.join("proj"));
    let api = api(&root);
    let device = |scope| {
        api.connect_as(
            "remote",
            Some(DeviceIdentity {
                device_id: format!("dev_{scope:?}"),
                scope,
            }),
        )
    };
    let mut view = device(Scope::View);
    let reply = ask(&api, &mut view, ClientMsg::ProjectFolders).await;
    assert!(
        matches!(
            reply,
            ServerMsg::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ),
        "{reply:?}"
    );
    let reply = ask(
        &api,
        &mut view,
        ClientMsg::ListFolder {
            path: canon(&root),
            show_hidden: false,
        },
    )
    .await;
    assert!(matches!(
        reply,
        ServerMsg::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));

    let mut command = device(Scope::Command);
    let welcome = ask(
        &api,
        &mut command,
        ClientMsg::Hello {
            protocol_version: vca_core::PROTOCOL_VERSION,
            app_version: "t".into(),
        },
    )
    .await;
    let ServerMsg::Welcome { features, .. } = welcome else {
        panic!("{welcome:?}")
    };
    assert!(
        features
            .iter()
            .any(|f| f == vca_core::protocol::FEATURE_PROJECT_FOLDERS)
    );
    let reply = ask(&api, &mut command, ClientMsg::ProjectFolders).await;
    let ServerMsg::ProjectFolders { roots, .. } = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(roots[0].path, canon(&root));
    let reply = ask(
        &api,
        &mut command,
        ClientMsg::ListFolder {
            path: roots[0].path.clone(),
            show_hidden: false,
        },
    )
    .await;
    assert_eq!(entries(&reply), ["proj"]);
    let reply = ask(
        &api,
        &mut command,
        ClientMsg::ListFolder {
            path: canon(tmp.path()),
            show_hidden: false,
        },
    )
    .await;
    assert!(matches!(
        reply,
        ServerMsg::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
}
