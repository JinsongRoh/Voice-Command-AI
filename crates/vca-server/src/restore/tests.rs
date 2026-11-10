//! The service on a fake desktop: nothing here opens, moves or reads a real
//! window, and every list lives in a temporary directory.

use std::path::Path;
use std::sync::Mutex;

use tokio::sync::broadcast;
use vca_core::{AgentKind, RestoreEntryDraft, RestoreResume, SessionId, WindowRect};

use super::backend::FakeCall;
use super::store::{DEFAULT_PROFILE, backup_path};
use super::*;
use crate::config::RestoreConfig;

const WAIT: Duration = Duration::from_secs(10);

fn rect(x: i32) -> WindowRect {
    WindowRect {
        x,
        y: 20,
        w: 800,
        h: 600,
    }
}

fn window(id: u64, title: &str, r: WindowRect) -> LiveWindow {
    LiveWindow {
        id,
        title: title.into(),
        rect: r,
        maximized: false,
        minimized: false,
    }
}

fn session(title: &str, folder: &Path, id: &str, pid: u32) -> LiveSession {
    LiveSession {
        id: SessionId::from(format!("vca-{id}").as_str()),
        agent: AgentKind::ClaudeCode,
        folder: folder.to_owned(),
        session_id: Some(id.into()),
        title: title.into(),
        custom_name: false,
        vca_started: false,
        pids: vec![pid],
        transcript: None,
        args: Vec::new(),
    }
}

struct Rig {
    restore: Restore,
    backend: Arc<FakeBackend>,
    live: Arc<Mutex<Vec<LiveSession>>>,
    pushes: broadcast::Receiver<ServerMsg>,
    dir: tempfile::TempDir,
}

fn rig_with(interval: Option<Duration>, roots: Vec<PathBuf>) -> Rig {
    rig_full(interval, roots, None)
}

fn rig_full(
    interval: Option<Duration>,
    roots: Vec<PathBuf>,
    managed: Option<ManagedLaunch>,
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let backend = FakeBackend::new();
    let live: Arc<Mutex<Vec<LiveSession>>> = Arc::default();
    let source = Arc::clone(&live);
    let (notices, pushes) = broadcast::channel(256);
    let settings = RestoreSettings {
        path: dir.path().join(store::RESTORE_FILE),
        interval,
        auto_register: true,
        title_wait: Duration::from_millis(100),
        title_poll: Duration::from_millis(5),
        managed,
    };
    let restore = Restore::new(
        settings,
        backend.clone(),
        Arc::new(move |_| lock(&source).clone()),
        WorkspaceRoots::new(roots),
        notices,
    );
    Rig {
        restore,
        backend,
        live,
        pushes,
        dir,
    }
}

fn rig() -> Rig {
    rig_with(None, Vec::new())
}

impl Rig {
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn add(&self, title: &str, folder: &Path, x: i32, session: Option<&str>) -> String {
        self.restore
            .edit(
                &RestoreEdit::EntryAdd {
                    profile: DEFAULT_PROFILE.into(),
                    entry: RestoreEntryDraft {
                        title: title.into(),
                        agent: AgentKind::ClaudeCode,
                        folder: folder.to_owned(),
                        session_id: session.map(str::to_owned),
                        rect: Some(rect(x)),
                        maximized: false,
                        enabled: true,
                    },
                },
                true,
            )
            .unwrap();
        lock(&self.restore.inner.state).file.profiles[0]
            .entries
            .last()
            .unwrap()
            .id
            .clone()
    }

    fn set_delay(&self, secs: u32) {
        self.restore
            .edit(
                &RestoreEdit::ProfileSettings {
                    profile: DEFAULT_PROFILE.into(),
                    delay_secs: secs,
                    auto_prompt: None,
                },
                true,
            )
            .unwrap();
    }

    async fn finished(&self) -> RestoreJob {
        tokio::time::timeout(WAIT, async {
            loop {
                let job = lock(&self.restore.inner.state).job.clone();
                if let Some(job) = job.filter(|j| j.finished_at.is_some()) {
                    return job;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the run did not finish")
    }

    async fn run_all(&self) -> RestoreJob {
        self.restore
            .run("local", &RestoreRunRequest::default(), false, None)
            .await
            .unwrap();
        self.finished().await
    }
}

#[tokio::test]
async fn an_unchanged_snapshot_writes_nothing() {
    let rig = rig();
    let folder = rig.folder("app");
    *lock(&rig.live) = vec![session("Payments", &folder, "s1", 11)];
    rig.backend
        .set_windows(vec![window(7, "Payments", rect(100))]);
    rig.backend.set_owner(11, 7);

    assert!(rig.restore.snapshot(None).await.unwrap());
    assert_eq!(rig.restore.writes(), 1);
    let path = rig.dir.path().join(store::RESTORE_FILE);
    let first = std::fs::read(&path).unwrap();
    for _ in 0..3 {
        assert!(!rig.restore.snapshot(None).await.unwrap());
    }
    assert_eq!(rig.restore.writes(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), first);
    assert!(!backup_path(&path).exists());

    // Moving the window is a change; the old file becomes the backup.
    rig.backend
        .set_windows(vec![window(7, "Payments", rect(200))]);
    assert!(rig.restore.snapshot(None).await.unwrap());
    assert_eq!(rig.restore.writes(), 2);
    assert_eq!(std::fs::read(backup_path(&path)).unwrap(), first);
}

#[tokio::test]
async fn the_snapshot_runs_on_its_interval() {
    let rig = rig_with(Some(Duration::from_millis(20)), Vec::new());
    let folder = rig.folder("app");
    *lock(&rig.live) = vec![session("A", &folder, "s1", 1)];
    rig.backend.set_windows(vec![window(7, "A", rect(100))]);
    rig.backend.set_owner(1, 7);
    rig.restore.start();
    tokio::time::timeout(WAIT, async {
        while rig.restore.writes() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("no timed snapshot");
    // Quiet from here on: nothing changes, nothing is written, but it runs.
    let first = lock(&rig.restore.inner.state).last_snapshot_at;
    tokio::time::timeout(WAIT, async {
        while lock(&rig.restore.inner.state).last_snapshot_at == first {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the timer stopped");
    assert_eq!(rig.restore.writes(), 1);
}

#[test]
fn the_default_interval_is_five_minutes_with_a_floor() {
    let config = RestoreConfig::default();
    assert_eq!(config.snapshot_interval(), Duration::from_secs(300));
    let fast = RestoreConfig {
        snapshot_interval_secs: 1,
        ..RestoreConfig::default()
    };
    assert_eq!(fast.snapshot_interval(), Duration::from_secs(30));
    let dir = tempfile::tempdir().unwrap();
    let off = RestoreConfig {
        enabled: false,
        ..RestoreConfig::default()
    };
    let endpoint = Path::new("vca-control-test");
    assert_eq!(
        RestoreSettings::new(dir.path(), &off, endpoint).interval,
        None
    );
}

#[test]
fn windows_run_vca_next_to_vcad_unless_the_launch_is_direct() {
    use crate::config::RestoreLaunchMode;

    let config = RestoreConfig::default();
    // Direct by default: managed sessions end when vcad restarts or updates.
    assert_eq!(config.launch_mode, RestoreLaunchMode::Direct);
    let vcad = std::env::temp_dir().join("VCA").join("vcad.exe");
    let endpoint = Path::new(r"\\.\pipe\vca-control-me");
    let managed = super::managed_launch(RestoreLaunchMode::Managed, Some(&vcad), endpoint).unwrap();
    assert_eq!(
        managed.vca,
        vcad.parent()
            .unwrap()
            .join(format!("vca{}", std::env::consts::EXE_SUFFIX))
    );
    assert_eq!(managed.endpoint, endpoint);
    // Direct, or no absolute vcad path: the agent runs directly.
    assert_eq!(
        super::managed_launch(RestoreLaunchMode::Direct, Some(&vcad), endpoint),
        None
    );
    assert_eq!(
        super::managed_launch(
            RestoreLaunchMode::Managed,
            Some(Path::new("vcad")),
            endpoint
        ),
        None
    );
    assert_eq!(
        super::managed_launch(RestoreLaunchMode::Managed, None, endpoint),
        None
    );
    // The setting parses from the config file.
    let parsed: RestoreConfig = toml::from_str("launch_mode = \"direct\"").unwrap();
    assert_eq!(parsed.launch_mode, RestoreLaunchMode::Direct);
    assert!(toml::from_str::<RestoreConfig>("launch_mode = \"shell\"").is_err());
}

#[tokio::test]
async fn a_run_hands_the_managed_launch_to_the_backend() {
    let managed = ManagedLaunch {
        vca: PathBuf::from(r"C:\VCA\vca.exe"),
        endpoint: PathBuf::from("vca-control-me"),
    };
    let rig = rig_full(None, Vec::new(), Some(managed.clone()));
    let a = rig.folder("a");
    rig.add("One", &a, 100, None);
    rig.run_all().await;
    let calls = rig.backend.calls();
    let Some(FakeCall::Open(request)) = calls.first() else {
        panic!("{calls:?}");
    };
    assert_eq!(request.managed.as_ref(), Some(&managed));
}

#[tokio::test]
async fn a_run_opens_and_places_only_what_is_missing() {
    let rig = rig();
    let (a, b, gone) = (
        rig.folder("a"),
        rig.folder("b"),
        rig.dir.path().join("gone"),
    );
    rig.add("Open already", &a, 100, None);
    rig.add("Second", &b, 200, Some("s2"));
    rig.add("Folder gone", &gone, 300, None);
    rig.add("Third", &a, 400, None);
    rig.set_delay(0);
    let other = window(77, "Open already", rect(1));
    rig.backend.set_windows(vec![other.clone()]);

    let job = rig.run_all().await;
    let statuses: Vec<RestoreItemStatus> = job.items.iter().map(|i| i.status).collect();
    assert_eq!(
        statuses,
        [
            RestoreItemStatus::SkippedOpen,
            RestoreItemStatus::Placed,
            RestoreItemStatus::SkippedMissing,
            RestoreItemStatus::Placed,
        ]
    );
    assert_eq!(job.items[1].resume, RestoreResume::Exact);
    assert_eq!(job.items[1].command, "claude --resume s2");
    assert_eq!(rig.backend.opened_titles(), ["Second", "Third"]);

    // The window that was already there was never touched.
    let calls = rig.backend.calls();
    let placed: Vec<u64> = calls
        .iter()
        .filter_map(|c| match c {
            FakeCall::Place { id, .. } => Some(*id),
            FakeCall::Open(_) | FakeCall::Focus { .. } | FakeCall::Minimize { .. } => None,
        })
        .collect();
    assert_eq!(placed.len(), 2);
    assert!(!placed.contains(&other.id));
    let moved = rig.backend.windows().unwrap();
    assert_eq!(
        moved.iter().find(|w| w.title == "Second").unwrap().rect,
        rect(200)
    );
    assert_eq!(moved[0], other);
}

#[tokio::test]
async fn progress_reaches_every_client_step_by_step() {
    let mut rig = rig();
    let a = rig.folder("a");
    rig.add("One", &a, 100, None);
    rig.add("Two", &a, 200, None);
    rig.backend.fail_open("Two");
    rig.set_delay(0);
    let job = rig.run_all().await;
    assert_eq!(job.items[1].status, RestoreItemStatus::Failed);
    assert!(
        job.items[1]
            .message
            .as_deref()
            .unwrap()
            .contains("cannot start")
    );

    let mut seen = Vec::new();
    while let Ok(msg) = rig.pushes.try_recv() {
        let ServerMsg::RestoreProgress { job } = msg else {
            continue;
        };
        seen.push((job.items[0].status, job.items[1].status));
    }
    use RestoreItemStatus::{Failed, Opening, Placed, Waiting};
    assert_eq!(seen.first(), Some(&(Waiting, Waiting)));
    assert!(seen.contains(&(Opening, Waiting)));
    assert!(seen.contains(&(Placed, Opening)));
    assert_eq!(seen.last(), Some(&(Placed, Failed)));
}

#[tokio::test]
async fn a_window_that_never_appears_fails_without_moving_anything() {
    let rig = rig();
    let a = rig.folder("a");
    rig.add("Slow", &a, 100, None);
    rig.backend.never_appears("Slow");
    let job = rig.run_all().await;
    assert_eq!(job.items[0].status, RestoreItemStatus::Failed);
    assert_eq!(rig.backend.calls().len(), 1);
    // Retry: the same entry alone.
    let retry = RestoreRunRequest {
        entry_ids: vec![job.items[0].entry_id.clone()],
        ..RestoreRunRequest::default()
    };
    rig.restore.run("local", &retry, false, None).await.unwrap();
    rig.finished().await;
    assert_eq!(rig.backend.opened_titles(), ["Slow", "Slow"]);
}

#[tokio::test]
async fn a_dry_run_touches_nothing() {
    let rig = rig();
    let a = rig.folder("a");
    rig.add("One", &a, 100, None);
    let request = RestoreRunRequest {
        dry_run: true,
        ..RestoreRunRequest::default()
    };
    let job = rig
        .restore
        .run("local", &request, false, None)
        .await
        .unwrap();
    assert!(job.dry_run);
    assert!(job.finished_at.is_some());
    assert_eq!(job.items[0].status, RestoreItemStatus::Waiting);
    assert_eq!(job.items[0].rect, Some(rect(100)));
    assert!(rig.backend.calls().is_empty());
    assert!(rig.restore.overview().await.unwrap().job.is_none());
}

#[tokio::test]
async fn only_one_run_at_a_time() {
    let rig = rig();
    let a = rig.folder("a");
    rig.add("One", &a, 100, None);
    rig.add("Two", &a, 200, None);
    // Long enough to still be running when the second request arrives.
    rig.set_delay(5);
    rig.restore
        .run("local", &RestoreRunRequest::default(), false, None)
        .await
        .unwrap();
    let again = rig
        .restore
        .run("local", &RestoreRunRequest::default(), false, None)
        .await
        .unwrap_err();
    assert_eq!(again.code(), ErrorCode::Conflict);
}

#[tokio::test]
async fn a_remote_run_stays_inside_the_workspace_roots() {
    let dir = tempfile::tempdir().unwrap();
    let inside = dir.path().join("inside");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let rig = rig_with(None, vec![inside.clone()]);
    rig.add("In", &inside, 100, None);
    rig.add("Out", &outside, 200, None);
    rig.set_delay(0);
    rig.restore
        .run("phone", &RestoreRunRequest::default(), true, None)
        .await
        .unwrap();
    let job = rig.finished().await;
    assert_eq!(job.by, "phone");
    assert_eq!(job.items[0].status, RestoreItemStatus::Placed);
    assert_eq!(job.items[1].status, RestoreItemStatus::SkippedOutside);
    assert_eq!(rig.backend.opened_titles(), ["In"]);
    // The owner on the computer itself may open it.
    rig.restore
        .run("local", &RestoreRunRequest::default(), false, None)
        .await
        .unwrap();
    let job = rig.finished().await;
    assert_eq!(job.items[1].status, RestoreItemStatus::Placed);
}

#[tokio::test]
async fn the_kill_switch_stops_a_remote_run_between_windows() {
    let rig = rig();
    let a = rig.folder("a");
    rig.add("One", &a, 100, None);
    let stop: StopCheck = Arc::new(|| true);
    rig.restore
        .run("phone", &RestoreRunRequest::default(), false, Some(stop))
        .await
        .unwrap();
    let job = rig.finished().await;
    assert_eq!(job.items[0].status, RestoreItemStatus::Failed);
    assert!(rig.backend.calls().is_empty());
}

#[tokio::test]
async fn deleting_an_entry_leaves_its_window_alone() {
    let rig = rig();
    let a = rig.folder("a");
    let id = rig.add("One", &a, 100, None);
    rig.backend.set_windows(vec![window(7, "One", rect(100))]);
    rig.restore
        .edit(
            &RestoreEdit::EntryDelete {
                profile: DEFAULT_PROFILE.into(),
                entry_id: id,
            },
            true,
        )
        .unwrap();
    assert!(rig.backend.calls().is_empty());
    assert_eq!(rig.backend.windows().unwrap().len(), 1);
}

#[tokio::test]
async fn the_list_survives_a_restart_and_a_torn_write() {
    let rig = rig();
    let a = rig.folder("a");
    rig.add("One", &a, 100, None);
    rig.add("Two", &a, 200, None);
    let path = rig.dir.path().join(store::RESTORE_FILE);
    // Cut the newest file short: the previous write takes over.
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 3]).unwrap();
    let file = store::load(&path);
    assert_eq!(file.profiles[0].entries.len(), 1);
    assert_eq!(file.profiles[0].entries[0].title, "One");
}

#[tokio::test]
async fn import_goes_into_the_list() {
    let rig = rig();
    let layout = r#"{"delaySeconds": 9, "windows": [
        {"title": "Alpha", "dir": "E:\\Alpha", "x": -1929, "y": 0, "w": 1938, "h": 1053}
    ]}"#;
    assert_eq!(rig.restore.import(layout, None).await.unwrap(), 1);
    let overview = rig.restore.overview().await.unwrap();
    assert_eq!(overview.profiles[0].delay_secs, 9);
    assert_eq!(overview.profiles[0].entries[0].title, "Alpha");
    assert!(!overview.live[0].open);
}

#[tokio::test]
async fn the_overview_reports_platform_and_interval() {
    let rig = rig_with(Some(Duration::from_secs(300)), Vec::new());
    let overview = rig.restore.overview().await.unwrap();
    assert!(overview.supported);
    assert_eq!(overview.platform, "windows");
    assert_eq!(overview.snapshot_interval_secs, 300);
    assert_eq!(overview.active_profile, DEFAULT_PROFILE);
    assert_eq!(overview.job, None);
}

#[tokio::test]
async fn an_unsupported_platform_refuses_runs_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let (notices, _) = broadcast::channel(4);
    let restore = Restore::new(
        RestoreSettings {
            path: dir.path().join(store::RESTORE_FILE),
            interval: Some(Duration::from_secs(1)),
            auto_register: true,
            title_wait: Duration::from_millis(1),
            title_poll: Duration::from_millis(1),
            managed: None,
        },
        Arc::new(UnsupportedBackend::new("macos")),
        Arc::new(|_| Vec::new()),
        WorkspaceRoots::new(Vec::new()),
        notices,
    );
    let overview = restore.overview().await.unwrap();
    assert!(!overview.supported);
    assert!(overview.message.unwrap().contains("Windows only"));
    let err = restore
        .run("local", &RestoreRunRequest::default(), false, None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported);
}

#[tokio::test]
async fn a_desktop_that_cannot_move_windows_reports_opened_not_failed() {
    let rig = rig();
    let a = rig.folder("a");
    rig.add("One", &a, 100, None);
    rig.add("Two", &a, 200, None);
    rig.set_delay(0);
    rig.backend
        .cannot_place("opened, but not placed: Wayland does not let an application move windows");
    let job = rig.run_all().await;
    for item in &job.items {
        assert_eq!(item.status, RestoreItemStatus::Opened, "{item:?}");
        assert!(
            item.message.as_deref().unwrap().contains("Wayland"),
            "{item:?}"
        );
    }
    assert_eq!(rig.backend.opened_titles(), ["One", "Two"]);
    assert!(
        rig.backend
            .calls()
            .iter()
            .all(|c| matches!(c, FakeCall::Open(_)))
    );
}

#[tokio::test]
async fn an_entry_without_a_position_is_opened_and_left_where_it_is() {
    let rig = rig();
    let a = rig.folder("a");
    rig.restore
        .edit(
            &RestoreEdit::EntryAdd {
                profile: DEFAULT_PROFILE.into(),
                entry: RestoreEntryDraft {
                    title: "Nowhere".into(),
                    agent: AgentKind::ClaudeCode,
                    folder: a.clone(),
                    session_id: None,
                    rect: None,
                    maximized: false,
                    enabled: true,
                },
            },
            true,
        )
        .unwrap();
    let job = rig.run_all().await;
    assert_eq!(job.items[0].status, RestoreItemStatus::Opened);
    assert!(
        job.items[0]
            .message
            .as_deref()
            .unwrap()
            .contains("no position")
    );
    assert_eq!(rig.backend.calls().len(), 1);
}

#[tokio::test]
async fn the_open_request_names_its_entry() {
    let rig = rig();
    let a = rig.folder("a");
    let id = rig.add("One", &a, 100, None);
    rig.run_all().await;
    let calls = rig.backend.calls();
    let Some(FakeCall::Open(request)) = calls.first() else {
        panic!("{calls:?}");
    };
    assert_eq!(request.entry_id, id);
    assert_eq!(request.title, "One");
}

fn focused(rig: &Rig) -> Vec<u64> {
    rig.backend
        .calls()
        .iter()
        .filter_map(|c| match c {
            FakeCall::Focus { id } => Some(*id),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn bring_to_front_uses_the_console_owner_first() {
    let rig = rig();
    let a = rig.folder("a");
    // Two windows with the session's title: only the console can tell.
    *lock(&rig.live) = vec![session("Payments", &a, "s1", 11)];
    rig.backend.set_windows(vec![
        window(7, "Payments", rect(100)),
        window(8, "Payments", rect(200)),
    ]);
    rig.backend.set_owner(11, 8);
    let id = SessionId::from("vca-s1");
    rig.restore.focus_session(&id).await.unwrap();
    assert_eq!(focused(&rig), [8]);
    // Nothing else happened to any window.
    assert_eq!(rig.backend.calls().len(), 1);
}

#[tokio::test]
async fn bring_to_front_falls_back_to_the_exact_entry_title() {
    let rig = rig();
    let a = rig.folder("a");
    rig.add("Payments", &a, 100, Some("s1"));
    // VCA shows another name; the entry (same conversation) has the title.
    *lock(&rig.live) = vec![session("claude 2", &a, "s1", 11)];
    rig.backend.set_windows(vec![
        window(7, "Other", rect(100)),
        window(9, "Payments", rect(200)),
    ]);
    rig.restore
        .focus_session(&SessionId::from("vca-s1"))
        .await
        .unwrap();
    assert_eq!(focused(&rig), [9]);
}

#[tokio::test]
async fn bring_to_front_reports_what_it_could_not_do() {
    let rig = rig();
    let a = rig.folder("a");
    let missing = rig
        .restore
        .focus_session(&SessionId::from("vca-none"))
        .await
        .unwrap_err();
    assert_eq!(missing.code(), ErrorCode::NotFound);

    // Running, but no window can be named: two with its title, no console.
    *lock(&rig.live) = vec![session("Same", &a, "s1", 11)];
    rig.backend.set_windows(vec![
        window(7, "Same", rect(100)),
        window(8, "Same", rect(200)),
    ]);
    let id = SessionId::from("vca-s1");
    let err = rig.restore.focus_session(&id).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotFound);
    assert!(focused(&rig).is_empty());

    // Windows keeps the focus where it is: said plainly, not worked around.
    rig.backend.set_owner(11, 7);
    rig.backend.refuse_focus();
    let err = rig.restore.focus_session(&id).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unavailable);
    assert!(crate::util::error_text(&err).contains("bring"), "{err}");
    assert_eq!(focused(&rig), [7]);
}

#[tokio::test]
async fn bring_to_front_is_unsupported_off_windows() {
    let dir = tempfile::tempdir().unwrap();
    let (notices, _) = broadcast::channel(4);
    let a = dir.path().join("a");
    std::fs::create_dir_all(&a).unwrap();
    let live = vec![session("Payments", &a, "s1", 11)];
    let restore = Restore::new(
        RestoreSettings {
            path: dir.path().join(store::RESTORE_FILE),
            interval: None,
            auto_register: true,
            title_wait: Duration::from_millis(1),
            title_poll: Duration::from_millis(1),
            managed: None,
        },
        Arc::new(UnsupportedBackend::new("linux")),
        Arc::new(move |_| live.clone()),
        WorkspaceRoots::new(Vec::new()),
        notices,
    );
    let err = restore
        .focus_session(&SessionId::from("vca-s1"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported);
    assert!(crate::util::error_text(&err).contains("Windows"), "{err}");
}
