//! Command lines for opening a window. Pure, so they are tested on every
//! platform and without starting anything.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use vca_core::child_env;

use super::backend::{ManagedLaunch, OpenRequest};

/// VCA's own variables (by prefix), which a restored agent does not inherit.
pub const VCA_ENV_PREFIX: &str = "VCA_";

/// Whether a restored agent does not inherit the variable `name` from
/// vcad: VCA's own variables and the identity of a Claude Code session
/// vcad itself may run under ([`child_env::is_session_var`]). User
/// configuration such as `CLAUDE_CODE_USE_BEDROCK` or `ANTHROPIC_API_KEY`
/// stays.
pub fn is_stripped(name: &OsStr) -> bool {
    name.to_string_lossy()
        .to_ascii_uppercase()
        .starts_with(VCA_ENV_PREFIX)
        || child_env::is_session_var(name)
}

/// The names in `names` (an environment's variable names) that a restored
/// agent does not inherit: [`is_stripped`] ones, and `NO_COLOR` next to a
/// Claude Code session marker ([`child_env::removals`]).
pub fn stripped_env<I, K>(names: I) -> Vec<OsString>
where
    I: IntoIterator<Item = K>,
    K: AsRef<OsStr>,
{
    let names: Vec<OsString> = names.into_iter().map(|n| n.as_ref().to_owned()).collect();
    let mut out: Vec<OsString> = names.iter().filter(|n| is_stripped(n)).cloned().collect();
    for extra in child_env::removals(&names) {
        if !out.contains(&extra) {
            out.push(extra);
        }
    }
    out
}

/// The environment a restored window starts with: `base` (the person's
/// default environment, as a window opened from the desktop gets it)
/// without the [`stripped_env`] variables, which stay removed as a floor
/// even if `base` had to fall back to vcad's own environment.
pub fn restored_env<I>(base: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let base: Vec<(OsString, OsString)> = base.into_iter().collect();
    let removed = stripped_env(base.iter().map(|(k, _)| k));
    base.into_iter()
        .filter(|(k, _)| !removed.contains(k))
        .collect()
}

/// Clears the session variables inside the new PowerShell too: Windows
/// Terminal 1.18+ gives a tab opened through `wt.exe` either the caller's
/// environment (`--inheritEnvironment`, or the setting
/// `compatibility.reloadEnvironmentVariables: false`) or a fresh one read
/// from the registry (the default); either way nothing of a Claude Code
/// session survives into the agent.
fn with_cleanup(command: &str) -> String {
    format!("{}; {command}", child_env::powershell_cleanup())
}

/// What the tab's PowerShell runs after loading the person's profile, as
/// any new tab does (no `-NoProfile`): the session cleanup, then back to
/// the entry's folder (a profile may change the location, and an agent
/// resumes its conversation by folder), then the agent. `-NoExit` keeps the
/// interactive shell, profile included, when the agent exits.
fn tab_command(folder: &Path, program: &Path, args: &[String]) -> String {
    with_cleanup(&format!(
        "Set-Location -LiteralPath {}; {}",
        ps_quote(&folder.to_string_lossy()),
        powershell_command(program, args)
    ))
}

/// A PowerShell single-quoted string (every character PowerShell treats
/// as a single quote, typographic ones included, is doubled).
use vca_core::powershell::quote as ps_quote;

/// The PowerShell command that runs the agent.
pub fn powershell_command(program: &Path, args: &[String]) -> String {
    let mut out = format!("& {}", ps_quote(&program.to_string_lossy()));
    for arg in args {
        out.push(' ');
        out.push_str(&ps_quote(arg));
    }
    out
}

/// What the window runs: the agent itself (`agent`, already resolved to a
/// file), or, with `managed`, `vca run` holding the agent as a session
/// managed by vcad, attached to the window. The name is passed as
/// `--name=<title>` so a title starting with `-` stays a value, and every
/// agent argument follows `--`, so none is read as an option of `vca`.
/// Double quotes in the name become `'`, as in the window title: neither
/// `wt.exe` nor Windows PowerShell passes a `"` inside an argument on to a
/// program intact (they drop it, and the words around it can split).
pub fn window_command(
    request: &OpenRequest,
    agent: &Path,
    managed: Option<&ManagedLaunch>,
) -> (PathBuf, Vec<String>) {
    let Some(managed) = managed else {
        return (agent.to_owned(), request.args.clone());
    };
    let endpoint = crate::control::endpoint_env_value(&managed.endpoint);
    let mut args = vec![
        "--endpoint".to_owned(),
        endpoint.to_string_lossy().into_owned(),
        "run".to_owned(),
        format!("--name={}", request.title.replace('"', "'")),
        "--".to_owned(),
        agent.to_string_lossy().into_owned(),
    ];
    args.extend(request.args.iter().cloned());
    (managed.vca.clone(), args)
}

/// `wt.exe` treats `;` as a command separator in every argument.
fn wt_escape(value: &str) -> String {
    value.replace(';', r"\;")
}

/// Arguments of `wt.exe`: a new window, the fixed title (the application
/// cannot change it), the folder, and PowerShell running the agent, so the
/// window stays open when the agent exits. PowerShell loads the person's
/// profile first, like any new tab (see [`tab_command`]). Double quotes cannot be passed
/// through `wt.exe`, so they do not reach the title. `program` and `args`
/// come from [`window_command`].
pub fn wt_args(
    request: &OpenRequest,
    powershell: &Path,
    program: &Path,
    args: &[String],
) -> Vec<String> {
    let title = request.title.replace('"', "'");
    vec![
        "-w".into(),
        "new".into(),
        "new-tab".into(),
        "--title".into(),
        wt_escape(&title),
        "--suppressApplicationTitle".into(),
        "-d".into(),
        wt_escape(&request.folder.to_string_lossy()),
        powershell.to_string_lossy().into_owned(),
        "-NoExit".into(),
        "-Command".into(),
        wt_escape(&tab_command(&request.folder, program, args)),
    ]
}

/// PowerShell arguments for a plain console window (no `wt.exe`): it sets
/// the title itself, because a console has no fixed-title option.
pub fn console_args(request: &OpenRequest, program: &Path, args: &[String]) -> Vec<String> {
    let title = ps_quote(&request.title);
    let command = tab_command(&request.folder, program, args);
    vec![
        "-NoExit".into(),
        "-Command".into(),
        format!("$Host.UI.RawUI.WindowTitle={title}; {command}"),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn request(title: &str, args: &[&str]) -> OpenRequest {
        OpenRequest {
            entry_id: "e1".into(),
            title: title.into(),
            folder: PathBuf::from("E:\\work\\app"),
            program: "claude".into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            managed: None,
        }
    }

    fn wt(req: &OpenRequest, program: &str) -> Vec<String> {
        wt_args(req, Path::new(PS), Path::new(program), &req.args)
    }

    fn console(req: &OpenRequest, program: &str) -> Vec<String> {
        console_args(req, Path::new(program), &req.args)
    }

    const PS: &str = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
    const CLAUDE: &str = "C:\\Users\\me\\.local\\bin\\claude.exe";

    #[test]
    fn wt_gets_a_new_window_with_a_fixed_title() {
        let args = wt(&request("Payments", &["--resume", "ab-1"]), CLAUDE);
        assert_eq!(&args[..3], ["-w", "new", "new-tab"]);
        assert_eq!(&args[3..5], ["--title", "Payments"]);
        assert_eq!(args[5], "--suppressApplicationTitle");
        assert_eq!(&args[6..8], ["-d", "E:\\work\\app"]);
        assert_eq!(args[8], PS);
        // The tab's shell loads the person's profile like any new tab:
        // no `-NoProfile`, and `-NoExit` keeps it open after the agent.
        assert_eq!(args.len(), 12);
        assert_eq!(&args[9..11], ["-NoExit", "-Command"]);
        assert!(
            !args.iter().any(|a| a.eq_ignore_ascii_case("-NoProfile")),
            "{args:?}"
        );
        let cleanup = wt_escape(&child_env::powershell_cleanup());
        assert_eq!(
            args[11],
            format!(
                "{cleanup}\\; Set-Location -LiteralPath 'E:\\work\\app'\\; \
                 & 'C:\\Users\\me\\.local\\bin\\claude.exe' '--resume' 'ab-1'"
            )
        );
        // The cleanup runs inside the tab: `;` stays escaped for `wt.exe`.
        assert!(!args[11].replace("\\;", "").contains(';'), "{}", args[11]);
    }

    #[test]
    fn separators_and_quotes_cannot_break_out() {
        let mut req = request("a; b \"c\" 'd'", &["it's; here"]);
        req.folder = PathBuf::from("E:\\x;y");
        let args = wt(&req, CLAUDE);
        assert_eq!(args[4], "a\\; b 'c' 'd'");
        assert_eq!(args[7], "E:\\x\\;y");
        assert!(args[11].ends_with("'it''s\\; here'"), "{}", args[11]);
    }

    #[test]
    fn a_console_window_sets_its_own_title() {
        let args = console(&request("it's", &[]), "claude.cmd");
        assert_eq!(args.len(), 3);
        assert_eq!(&args[..2], ["-NoExit", "-Command"]);
        assert_eq!(
            args[2],
            format!(
                "$Host.UI.RawUI.WindowTitle='it''s'; {}; \
                 Set-Location -LiteralPath 'E:\\work\\app'; & 'claude.cmd'",
                child_env::powershell_cleanup()
            )
        );
    }

    #[test]
    fn a_folder_cannot_break_out_of_set_location() {
        let mut req = request("x", &[]);
        req.folder = PathBuf::from("E:\\it's; calc");
        let console = console(&req, "claude.cmd");
        assert!(
            console[2].contains("Set-Location -LiteralPath 'E:\\it''s; calc'; & 'claude.cmd'"),
            "{}",
            console[2]
        );
        let wt = wt(&req, CLAUDE);
        assert!(
            wt[11].contains("Set-Location -LiteralPath 'E:\\it''s\\; calc'\\; "),
            "{}",
            wt[11]
        );
        assert!(!wt[11].replace("\\;", "").contains(';'), "{}", wt[11]);
    }

    #[test]
    fn typographic_quotes_cannot_break_out() {
        let title = "Bob\u{2019}s fix\u{2018}; calc; \u{201A}x\u{201B}";
        let args = console(
            &request(title, &["\u{2019}; Remove-Item x; \u{2019}"]),
            "claude.cmd",
        );
        assert!(
            args[2].starts_with(
                "$Host.UI.RawUI.WindowTitle='Bob\u{2019}\u{2019}s fix\u{2018}\u{2018}; calc; \
                 \u{201A}\u{201A}x\u{201B}\u{201B}'; "
            ),
            "{}",
            args[2]
        );
        assert!(
            args[2].ends_with("& 'claude.cmd' '\u{2019}\u{2019}; Remove-Item x; \u{2019}\u{2019}'"),
            "{}",
            args[2]
        );
        let wt = wt(&request(title, &["it\u{2019}s"]), CLAUDE);
        assert!(wt[11].ends_with("'it\u{2019}\u{2019}s'"), "{}", wt[11]);
    }

    const VCA: &str = "C:\\Users\\me\\AppData\\Local\\Voice Command AI\\vca.exe";

    fn managed() -> ManagedLaunch {
        ManagedLaunch {
            vca: PathBuf::from(VCA),
            endpoint: PathBuf::from("vca-control-me"),
        }
    }

    #[test]
    fn a_direct_window_runs_the_agent_itself() {
        let req = request("Payments", &["--resume", "ab-1"]);
        let (program, args) = window_command(&req, Path::new(CLAUDE), None);
        assert_eq!(program, PathBuf::from(CLAUDE));
        assert_eq!(args, ["--resume", "ab-1"]);
    }

    #[test]
    fn a_managed_window_runs_vca_run_with_the_agent_after_the_separator() {
        let req = request("-rf Payments", &["--resume", "ab-1", "go on"]);
        let (program, args) = window_command(&req, Path::new(CLAUDE), Some(&managed()));
        assert_eq!(program, PathBuf::from(VCA));
        assert_eq!(
            args,
            [
                "--endpoint",
                "vca-control-me",
                "run",
                "--name=-rf Payments",
                "--",
                CLAUDE,
                "--resume",
                "ab-1",
                "go on",
            ]
        );
        // Through PowerShell every piece stays one quoted argument.
        let wt = wt_args(&req, Path::new(PS), &program, &args);
        assert_eq!(&wt[3..5], ["--title", "-rf Payments"]);
        assert_eq!(&wt[6..8], ["-d", "E:\\work\\app"]);
        assert!(
            wt[11].ends_with(
                "& 'C:\\Users\\me\\AppData\\Local\\Voice Command AI\\vca.exe' \
                 '--endpoint' 'vca-control-me' 'run' '--name=-rf Payments' '--' \
                 'C:\\Users\\me\\.local\\bin\\claude.exe' '--resume' 'ab-1' 'go on'"
            ),
            "{}",
            wt[11]
        );
    }

    #[test]
    fn a_managed_title_cannot_break_out_of_its_quotes() {
        let req = request("x'; Remove-Item y; '", &[]);
        let (program, args) = window_command(&req, Path::new(CLAUDE), Some(&managed()));
        let console = console_args(&req, &program, &args);
        assert!(
            console[2].contains("'--name=x''; Remove-Item y; '''"),
            "{}",
            console[2]
        );
        let wt = wt_args(&req, Path::new(PS), &program, &args);
        assert!(!wt[11].replace("\\;", "").contains(';'), "{}", wt[11]);
    }

    #[test]
    fn a_managed_name_carries_no_double_quotes() {
        let req = request("Fix \"login\" race", &[]);
        let (program, args) = window_command(&req, Path::new(CLAUDE), Some(&managed()));
        assert!(
            args.contains(&"--name=Fix 'login' race".to_owned()),
            "{args:?}"
        );
        let wt = wt_args(&req, Path::new(PS), &program, &args);
        assert!(!wt[11].contains('"'), "{}", wt[11]);
        // A console window still shows the title as typed (PowerShell
        // itself receives it intact), but `vca run` gets the safe name.
        let console = console_args(&req, &program, &args);
        assert!(
            console[2].contains("'--name=Fix ''login'' race'"),
            "{}",
            console[2]
        );
    }

    #[test]
    fn restored_agents_lose_session_identity_but_keep_configuration() {
        for name in [
            "CLAUDECODE",
            "claude_code_entrypoint",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_PID",
            "VCA_CONTROL",
        ] {
            assert!(is_stripped(OsStr::new(name)), "{name}");
        }
        for name in [
            "PATH",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_CODE_USE_BEDROCK",
            "ANTHROPIC_API_KEY",
            "NO_COLOR",
        ] {
            assert!(!is_stripped(OsStr::new(name)), "{name}");
        }
        assert_eq!(
            stripped_env(["PATH", "NO_COLOR", "CLAUDECODE", "VCA_X"]),
            ["CLAUDECODE", "VCA_X", "NO_COLOR"].map(OsString::from)
        );
        assert!(stripped_env(["PATH", "NO_COLOR"]).is_empty());
    }

    #[test]
    fn the_restored_environment_keeps_the_floor() {
        let base = [
            ("Path", "C:\\Windows"),
            ("CLAUDECODE", "1"),
            ("VCA_SESSION_ID", "s1"),
            ("VCA_HOOK_TOKEN", "secret"),
            ("NO_COLOR", "1"),
            ("ANTHROPIC_API_KEY", "sk"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let env = restored_env(base);
        let names: Vec<&OsStr> = env.iter().map(|(k, _)| k.as_os_str()).collect();
        assert_eq!(names, ["Path", "ANTHROPIC_API_KEY"]);
        // Without a session marker the person's NO_COLOR stays.
        let own = restored_env([(OsString::from("NO_COLOR"), OsString::from("1"))]);
        assert_eq!(own.len(), 1);
    }
}
