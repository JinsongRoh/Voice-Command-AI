//! Which command reopens a conversation, and which agent arguments may be
//! stored at all.
//!
//! Only known agents qualify, and only allow-listed arguments survive. Flags
//! that bypass the agent's permission prompts are never stored or added; a
//! captured session that used one loses it, with a note for the user.

use vca_adapters::generic::AgentSpec;
use vca_core::{AgentKind, RestoreResume};

/// Longest agent session id accepted.
const MAX_SESSION_ID: usize = 128;
/// Longest value of an allow-listed argument.
const MAX_ARG_VALUE: usize = 80;

/// Flags that turn off the agent's permission prompts.
const BYPASS_FLAGS: &[&str] = &[
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "--yolo",
    "--full-auto",
];

/// Shown when a captured session used a flag from [`BYPASS_FLAGS`].
pub const BYPASS_NOTE: &str = "a permission-bypass flag of the running session was dropped: the restored session asks as usual";

/// The agent's executable, for agents VCA knows by name. Generic programs
/// and shells cannot be restored.
pub fn agent_program(agent: &AgentKind) -> Option<String> {
    match agent {
        AgentKind::ClaudeCode => Some("claude".into()),
        AgentKind::Codex => Some("codex".into()),
        AgentKind::Generic(_) | AgentKind::Shell(_) => None,
        other => AgentSpec::builtin()
            .into_iter()
            .find(|s| &s.kind == other)
            .map(|s| s.program),
    }
}

/// True for an agent session id that is safe on a command line.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SESSION_ID
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn valid_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ARG_VALUE
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Flags with one value worth keeping, per agent.
fn value_flags(agent: &AgentKind) -> &'static [&'static str] {
    match agent {
        AgentKind::ClaudeCode => &["--model"],
        AgentKind::Codex => &["--model", "-m"],
        _ => &[],
    }
}

fn is_bypass(flag: &str, value: Option<&str>) -> bool {
    let name = flag.split('=').next().unwrap_or(flag);
    if BYPASS_FLAGS.contains(&name) {
        return true;
    }
    let mode = flag
        .strip_prefix("--permission-mode=")
        .or_else(|| (flag == "--permission-mode").then_some(value.unwrap_or("")));
    mode.is_some_and(|m| m.eq_ignore_ascii_case("bypassPermissions"))
}

/// Keeps the allow-listed arguments of `args`. The second value is true
/// when a permission-bypass flag was dropped.
pub fn filter_args(agent: &AgentKind, args: &[String]) -> (Vec<String>, bool) {
    let allowed = value_flags(agent);
    let mut kept = Vec::new();
    let mut bypass = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let next = args.get(i + 1).map(String::as_str);
        if is_bypass(arg, next) {
            bypass = true;
        } else if let Some((flag, value)) = arg.split_once('=') {
            if allowed.contains(&flag) && valid_value(value) {
                kept.push(arg.to_owned());
            }
        } else if allowed.contains(&arg) {
            if let Some(value) = next.filter(|v| valid_value(v)) {
                kept.push(arg.to_owned());
                kept.push(value.to_owned());
                i += 1;
            }
        }
        i += 1;
    }
    (kept, bypass)
}

/// The command of one restored window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCommand {
    pub program: String,
    pub args: Vec<String>,
    pub mode: RestoreResume,
}

impl ResumeCommand {
    /// The command line, for display.
    pub fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Chooses how the conversation comes back: the exact session by id; the
/// agent's latest conversation in the folder when the id is unknown or its
/// transcript is gone; a plain start for agents without a resume command.
/// `None` for agents that cannot be restored.
pub fn resume_command(
    agent: &AgentKind,
    session_id: Option<&str>,
    transcript_exists: bool,
    stored_args: &[String],
    prompt: Option<&str>,
) -> Option<ResumeCommand> {
    let program = agent_program(agent)?;
    let id = session_id
        .filter(|id| transcript_exists && valid_session_id(id))
        .map(str::to_owned);
    let (mode, mut args): (RestoreResume, Vec<String>) = match (agent, id) {
        (AgentKind::ClaudeCode, Some(id)) => (
            RestoreResume::Exact,
            vca_adapters::claude::ClaudeAdapter::resume_args(&id),
        ),
        (AgentKind::ClaudeCode, None) => (RestoreResume::Latest, vec!["--continue".to_owned()]),
        (AgentKind::Codex, Some(id)) => (
            RestoreResume::Exact,
            vca_adapters::codex::CodexAdapter::resume_args(&id),
        ),
        (AgentKind::Codex, None) => (
            RestoreResume::Latest,
            vec!["resume".to_owned(), "--last".to_owned()],
        ),
        _ => (RestoreResume::Fresh, Vec::new()),
    };
    args.extend(filter_args(agent, stored_args).0);
    if let Some(prompt) = prompt {
        args.push(prompt.to_owned());
    }
    Some(ResumeCommand {
        program,
        args,
        mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn claude_resumes_the_exact_session_by_id() {
        let cmd = resume_command(&AgentKind::ClaudeCode, Some("ab-12"), true, &[], None).unwrap();
        assert_eq!(cmd.mode, RestoreResume::Exact);
        assert_eq!(cmd.display(), "claude --resume ab-12");
    }

    #[test]
    fn claude_falls_back_to_continue_without_an_id_or_transcript() {
        let cmd = resume_command(&AgentKind::ClaudeCode, None, true, &[], None).unwrap();
        assert_eq!(
            (cmd.mode, cmd.display().as_str()),
            (RestoreResume::Latest, "claude --continue")
        );
        // The id is known but the transcript is gone.
        let cmd = resume_command(&AgentKind::ClaudeCode, Some("ab"), false, &[], None).unwrap();
        assert_eq!(cmd.mode, RestoreResume::Latest);
        // A damaged id never reaches a command line.
        let cmd = resume_command(&AgentKind::ClaudeCode, Some("a b;rm"), true, &[], None).unwrap();
        assert_eq!(cmd.display(), "claude --continue");
    }

    #[test]
    fn codex_resumes_by_id_or_the_last_session() {
        let cmd = resume_command(&AgentKind::Codex, Some("abc"), true, &[], None).unwrap();
        assert_eq!(cmd.display(), "codex resume abc");
        let cmd = resume_command(&AgentKind::Codex, None, true, &[], None).unwrap();
        assert_eq!(cmd.display(), "codex resume --last");
    }

    #[test]
    fn agents_without_a_resume_command_start_fresh() {
        let cmd = resume_command(&AgentKind::GeminiCli, Some("x"), true, &[], None).unwrap();
        assert_eq!(cmd.mode, RestoreResume::Fresh);
        assert_eq!(cmd.display(), "gemini");
        assert_eq!(
            resume_command(&AgentKind::CursorCli, None, true, &[], None)
                .unwrap()
                .program,
            "cursor-agent"
        );
    }

    #[test]
    fn unknown_programs_and_shells_cannot_be_restored() {
        let generic = AgentKind::Generic("evil".into());
        assert_eq!(resume_command(&generic, None, true, &[], None), None);
        let shell = AgentKind::Shell("powershell".into());
        assert_eq!(resume_command(&shell, None, true, &[], None), None);
    }

    #[test]
    fn the_prompt_is_the_last_argument() {
        let cmd = resume_command(
            &AgentKind::ClaudeCode,
            Some("ab"),
            true,
            &strings(&["--model", "opus"]),
            Some("go on"),
        )
        .unwrap();
        assert_eq!(cmd.args, ["--resume", "ab", "--model", "opus", "go on"]);
    }

    #[test]
    fn only_allow_listed_arguments_survive() {
        let (kept, bypass) = filter_args(
            &AgentKind::ClaudeCode,
            &strings(&["--model", "sonnet", "--mcp-config", "x.json", "--evil=1"]),
        );
        assert_eq!(kept, ["--model", "sonnet"]);
        assert!(!bypass);
        let (kept, _) = filter_args(&AgentKind::ClaudeCode, &strings(&["--model=opus"]));
        assert_eq!(kept, ["--model=opus"]);
        // Values are checked too.
        let (kept, _) = filter_args(&AgentKind::ClaudeCode, &strings(&["--model", "a;b"]));
        assert!(kept.is_empty());
        // Agents without an allow-list keep nothing.
        assert!(
            filter_args(&AgentKind::Aider, &strings(&["--model", "x"]))
                .0
                .is_empty()
        );
    }

    #[test]
    fn permission_bypass_flags_are_dropped_and_reported() {
        for args in [
            strings(&["--dangerously-skip-permissions"]),
            strings(&["--permission-mode", "bypassPermissions"]),
            strings(&["--permission-mode=bypassPermissions"]),
            strings(&["--dangerously-bypass-approvals-and-sandbox"]),
            strings(&["--yolo"]),
        ] {
            let (kept, bypass) = filter_args(&AgentKind::ClaudeCode, &args);
            assert!(bypass, "{args:?}");
            assert!(kept.is_empty(), "{args:?}");
        }
        // A stored file cannot bring one back.
        let cmd = resume_command(
            &AgentKind::ClaudeCode,
            None,
            true,
            &strings(&["--dangerously-skip-permissions"]),
            None,
        )
        .unwrap();
        assert_eq!(cmd.display(), "claude --continue");
    }
}
