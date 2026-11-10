//! The environment of processes VCA starts (agents, the conductor, remote
//! shells, restored windows, `vca run`).
//!
//! vcad and the desktop app are often started from inside a Claude Code
//! session (an installer or `vca` run by the agent's shell tool). They then
//! inherit that session's identity: its ids, its messaging socket and token
//! (a secret), its entry point and its process id. An agent VCA starts must
//! not run as that other session, so these variables are removed from every
//! child environment.
//!
//! Only per-session identity is removed. User configuration stays, also
//! when it shares the `CLAUDE_CODE_` prefix: `CLAUDE_CODE_USE_BEDROCK`,
//! `CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_MAX_OUTPUT_TOKENS`,
//! `CLAUDE_CONFIG_DIR`, `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, ...
//!
//! `NO_COLOR` is special: Claude Code sets it for the commands it runs, so a
//! vcad started from a Claude Code session has it although the person never
//! asked for it. It is removed only when the same environment also carries
//! a Claude Code session marker; a `NO_COLOR` the person set themselves (no
//! marker) stays, and one VCA sets for a child on purpose is applied after
//! the removal and so wins.

use std::ffi::{OsStr, OsString};

/// Session-identity variables removed by exact name.
pub const SESSION_VARS: &[&str] = &[
    // `1` inside a Claude Code session; agents refuse or change behavior
    // when they think they run nested.
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    // The IDE integration port of the parent session.
    "CLAUDE_CODE_SSE_PORT",
    // Secrets of a VCA session vcad itself may have been started from.
    "VCA_HOOK_TOKEN",
    "VCA_CONDUCTOR_TOKEN",
];

/// Session-identity variables removed by prefix: `CLAUDE_CODE_SESSION_ID`,
/// `CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_CODE_MESSAGING_SOCKET`,
/// `CLAUDE_CODE_MESSAGING_TOKEN` (a secret), ...
pub const SESSION_VAR_PREFIXES: &[&str] = &["CLAUDE_CODE_SESSION_", "CLAUDE_CODE_MESSAGING_"];

/// Removed only next to a session marker (see the module documentation).
pub const NO_COLOR: &str = "NO_COLOR";

/// Variables whose presence shows the environment came from a Claude Code
/// session.
const MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
];

/// Environment names compare without case on Windows; the denylist is
/// upper case, so comparing upper case is right everywhere (nobody sets
/// `claudecode` on purpose).
fn upper(name: &OsStr) -> String {
    name.to_string_lossy().to_ascii_uppercase()
}

/// True for a per-session Claude Code variable that a child must not
/// inherit.
pub fn is_session_var(name: impl AsRef<OsStr>) -> bool {
    let name = upper(name.as_ref());
    SESSION_VARS.contains(&name.as_str())
        || SESSION_VAR_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// True when the names include a Claude Code session marker.
pub fn has_session_marker<I, K>(names: I) -> bool
where
    I: IntoIterator<Item = K>,
    K: AsRef<OsStr>,
{
    names
        .into_iter()
        .any(|n| MARKERS.contains(&upper(n.as_ref()).as_str()))
}

/// The names (from `names`, an environment's variable names) to remove from
/// a child's environment: every session variable, and `NO_COLOR` when a
/// session marker is present.
pub fn removals<I, K>(names: I) -> Vec<OsString>
where
    I: IntoIterator<Item = K>,
    K: AsRef<OsStr>,
{
    let names: Vec<OsString> = names.into_iter().map(|n| n.as_ref().to_owned()).collect();
    let marked = has_session_marker(&names);
    names
        .into_iter()
        .filter(|n| is_session_var(n) || (marked && upper(n) == NO_COLOR))
        .collect()
}

/// [`removals`] for this process's environment, the one children inherit.
pub fn inherited_removals() -> Vec<OsString> {
    removals(std::env::vars_os().map(|(k, _)| k))
}

/// Drops the [`removals`] from a list of variables (for example the
/// caller's environment that `vca run` forwards).
pub fn filter_vars(vars: &[(String, String)]) -> Vec<(String, String)> {
    let removed = removals(vars.iter().map(|(k, _)| k.as_str()));
    vars.iter()
        .filter(|(k, _)| !removed.iter().any(|r| r.as_os_str() == OsStr::new(k)))
        .cloned()
        .collect()
}

/// A PowerShell statement that removes the session variables (and, next to
/// a marker, `NO_COLOR`) from the shell's own environment. Used inside
/// windows VCA opens through `wt.exe`, whose new tabs may get an
/// environment other than the one `wt.exe` was started with.
pub fn powershell_cleanup() -> String {
    let markers = MARKERS
        .iter()
        .map(|m| format!("(Test-Path Env:{m})"))
        .collect::<Vec<_>>()
        .join(" -or ");
    let paths = SESSION_VARS
        .iter()
        .map(|v| format!("Env:{v}"))
        .chain(SESSION_VAR_PREFIXES.iter().map(|p| format!("Env:{p}*")))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "if ({markers}) {{ Remove-Item Env:{NO_COLOR} -ErrorAction SilentlyContinue }}; \
         Remove-Item {paths} -ErrorAction SilentlyContinue"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The variables seen in a vcad started from a Claude Code session.
    const FROM_A_SESSION: &[&str] = &[
        "CLAUDECODE",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ATTENDED",
        "CLAUDE_CODE_BRIDGE_SESSION_ID",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_PID",
    ];

    /// User configuration that must stay.
    const CONFIGURATION: &[&str] = &[
        "PATH",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
        "CLAUDE_CODE_GIT_BASH_PATH",
        "MY_CLAUDECODE",
        "VCA_SESSION_ID",
    ];

    #[test]
    fn session_identity_is_removed_and_configuration_kept() {
        for name in FROM_A_SESSION {
            assert!(is_session_var(name), "{name}");
        }
        assert!(is_session_var("claudecode"));
        assert!(is_session_var("Claude_Code_Messaging_Token"));
        for name in CONFIGURATION {
            assert!(!is_session_var(name), "{name}");
        }
    }

    #[test]
    fn no_color_goes_only_with_a_session_marker() {
        let mut env: Vec<&str> = FROM_A_SESSION.to_vec();
        env.extend_from_slice(CONFIGURATION);
        env.push("NO_COLOR");
        let removed = removals(&env);
        let mut expected: Vec<OsString> = FROM_A_SESSION.iter().map(OsString::from).collect();
        expected.push("NO_COLOR".into());
        assert_eq!(removed, expected);

        // The person's own NO_COLOR, outside any Claude Code session.
        let own = removals(["PATH", "NO_COLOR", "CLAUDE_CODE_USE_BEDROCK"]);
        assert!(own.is_empty(), "{own:?}");
        // A marker alone is enough.
        assert_eq!(
            removals(["no_color", "CLAUDE_CODE_ENTRYPOINT"]),
            [OsString::from("no_color"), "CLAUDE_CODE_ENTRYPOINT".into()]
        );
    }

    #[test]
    fn forwarded_variables_are_filtered() {
        let vars: Vec<(String, String)> = [
            ("PATH", "/bin"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "secret"),
            ("NO_COLOR", "1"),
            ("ANTHROPIC_API_KEY", "sk-x"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let filtered = filter_vars(&vars);
        let kept: Vec<&str> = filtered.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kept, ["PATH", "ANTHROPIC_API_KEY"]);
        // Without a marker NO_COLOR stays.
        let plain = vec![("NO_COLOR".to_owned(), "1".to_owned())];
        assert_eq!(filter_vars(&plain), plain);
    }

    #[test]
    fn powershell_cleanup_names_every_variable() {
        let ps = powershell_cleanup();
        for v in SESSION_VARS {
            assert!(ps.contains(&format!("Env:{v},")), "{v}: {ps}");
        }
        assert!(ps.contains("Env:CLAUDE_CODE_SESSION_*"));
        assert!(ps.contains("Env:CLAUDE_CODE_MESSAGING_*"));
        assert!(ps.contains("(Test-Path Env:CLAUDECODE)"));
        assert!(ps.contains("Remove-Item Env:NO_COLOR"));
        // `wt.exe` splits commands at `;`; callers escape it.
        assert!(!ps.contains('"'));
    }
}
