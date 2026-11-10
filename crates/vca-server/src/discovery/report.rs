//! Classification behind the discovery report: which agent processes are
//! terminal sessions, which run inside a desktop app, and which programs are
//! plain shells that never count as AI sessions.
//!
//! Everything here reads the process table only.

use std::collections::HashMap;

use vca_adapters::{ProcInfo, ProcessTable};
use vca_core::AgentKind;

/// Ancestors inspected above an agent process.
const MAX_ANCESTORS: usize = 16;

/// Where an agent process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Host {
    /// In a terminal window or a shell (label of the terminal when known).
    Terminal(Option<String>),
    /// Inside a desktop app or another agent, without a terminal window of
    /// its own.
    App(String),
    /// Nothing tells either way; treated like a terminal so behavior stays
    /// as before.
    Unknown,
}

/// Lower-case executable name without directory and extension.
fn base(raw: &str) -> String {
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase();
    for ext in [".exe", ".com", ".bat", ".cmd", ".app"] {
        if let Some(stripped) = name.strip_suffix(ext) {
            return stripped.to_owned();
        }
    }
    name
}

/// Names of a process to compare: its name and the program of its command
/// line (they differ when a program renames itself).
fn names(p: &ProcInfo) -> Vec<String> {
    let mut out = vec![base(&p.name)];
    if let Some(first) = p.cmd.first() {
        let b = base(first);
        if !out.contains(&b) {
            out.push(b);
        }
    }
    out
}

const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "mksh",
    "tcsh",
    "csh",
    "ash",
    "nu",
    "elvish",
    "xonsh",
    "cmd",
    "powershell",
    "pwsh",
    "powershell_ise",
    "wsl",
    "wslhost",
    "git-bash",
    "login",
    "-bash",
    "-zsh",
    "-sh",
    "-fish",
];

/// True for a plain shell (PowerShell, cmd, bash, zsh and the like). Shells
/// never appear in the report, whatever their command line runs.
pub(crate) fn is_plain_shell(p: &ProcInfo) -> bool {
    base(&p.name)
        .split_whitespace()
        .next()
        .is_some_and(|n| SHELLS.contains(&n))
}

/// Terminal emulators and multiplexers, with the label shown to the person.
fn terminal_label(name: &str) -> Option<&'static str> {
    let label = match name {
        "windowsterminal" | "openconsole" => "Windows Terminal",
        "conhost" => "Windows console",
        "mintty" => "mintty",
        "alacritty" => "Alacritty",
        "wezterm" | "wezterm-gui" | "wezterm-mux-server" => "WezTerm",
        "kitty" => "kitty",
        "ghostty" => "Ghostty",
        "iterm2" | "iterm" => "iTerm2",
        "terminal" => "Terminal",
        "warp" | "stable" => "Warp",
        "hyper" => "Hyper",
        "tabby" => "Tabby",
        "rio" => "Rio",
        "foot" => "foot",
        "xterm" | "uxterm" => "xterm",
        "urxvt" | "rxvt" => "urxvt",
        "konsole" => "Konsole",
        "gnome-terminal-server" | "gnome-terminal" | "kgx" | "gnome-console" => "GNOME Terminal",
        "xfce4-terminal" => "Xfce Terminal",
        "terminator" => "Terminator",
        "tilix" => "Tilix",
        "st" => "st",
        "zellij" => "zellij",
        "screen" => "screen",
        "sshd" | "sshd-session" => "SSH",
        "mosh-server" => "mosh",
        _ if name.starts_with("tmux") => "tmux",
        _ => return None,
    };
    Some(label)
}

/// Desktop apps that run agents without a terminal window, by process
/// name (helpers of Electron apps included).
fn app_label(name: &str) -> Option<&'static str> {
    let label = if name.starts_with("chatgpt") {
        "ChatGPT"
    } else if name == "code" || name == "code - insiders" || name.starts_with("code helper") {
        "VS Code"
    } else if name == "cursor" || name.starts_with("cursor helper") {
        "Cursor"
    } else if name == "windsurf" || name.starts_with("windsurf helper") {
        "Windsurf"
    } else if name == "antigravity" || name.starts_with("antigravity helper") {
        "Antigravity"
    } else if name.starts_with("claude helper") {
        "Claude"
    } else if name == "electron" || name.starts_with("electron helper") {
        "Electron"
    } else {
        return None;
    };
    Some(label)
}

/// A desktop app recognized by where its executable lives (a macOS app
/// bundle, a Microsoft Store package, the Claude desktop app) or by an
/// Electron helper argument.
fn desktop_app(p: &ProcInfo) -> Option<String> {
    let path = p
        .exe
        .as_ref()
        .map(|e| e.to_string_lossy().into_owned())
        .or_else(|| p.cmd.first().cloned())
        .unwrap_or_default()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let label = || {
        names(p)
            .iter()
            .find_map(|n| app_label(n).map(str::to_owned))
            .unwrap_or_else(|| pretty(&p.name))
    };
    if path.contains("/chatgpt") || path.contains("openai.chatgpt") {
        return Some("ChatGPT".into());
    }
    if path.contains("/anthropicclaude/") || path.contains("/claude.app/") {
        return Some("Claude".into());
    }
    if path.contains(".app/contents/") || path.contains("/windowsapps/") {
        return Some(label());
    }
    if p.cmd.iter().skip(1).any(|a| a.starts_with("--type=")) {
        return Some(label());
    }
    None
}

/// The name as the person knows it: no directory, no `.exe`.
fn pretty(raw: &str) -> String {
    let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    name.strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))
        .unwrap_or(name)
        .to_owned()
}

/// Codex modes that serve an app over a protocol instead of a terminal.
fn serves_an_app(p: &ProcInfo) -> bool {
    let sub = p.cmd.iter().skip(1).find(|a| !a.starts_with('-'));
    sub.is_some_and(|s| matches!(s.as_str(), "app-server" | "mcp-server" | "proto"))
}

/// Where agent process `pid` runs. `agents` holds every agent process of
/// the pass, so an agent started by another agent (a tool call) is not
/// taken for a terminal session of its own.
pub(crate) fn classify_host(
    pid: u32,
    procs: &ProcessTable,
    agents: &HashMap<u32, AgentKind>,
) -> Host {
    let Some(me) = procs.get(pid) else {
        return Host::Unknown;
    };
    if let Some(app) = desktop_app(me) {
        return Host::App(app);
    }
    let mut shell_seen = false;
    for ancestor in procs.ancestors(pid).into_iter().take(MAX_ANCESTORS) {
        let Some(p) = procs.get(ancestor) else {
            break;
        };
        if let Some(agent) = agents.get(&ancestor) {
            return Host::App(agent_label(agent));
        }
        let n = names(p);
        if let Some(label) = n.iter().find_map(|n| terminal_label(n)) {
            return Host::Terminal(Some(label.to_owned()));
        }
        if let Some(label) = n.iter().find_map(|n| app_label(n)) {
            // An editor's integrated terminal runs a shell first.
            return if shell_seen {
                Host::Terminal(Some(format!("{label} terminal")))
            } else {
                Host::App(label.to_owned())
            };
        }
        if let Some(app) = desktop_app(p) {
            return if shell_seen {
                Host::Terminal(None)
            } else {
                Host::App(app)
            };
        }
        if is_plain_shell(p) {
            shell_seen = true;
        }
    }
    if serves_an_app(me) {
        return Host::App("app-server".into());
    }
    if shell_seen {
        Host::Terminal(None)
    } else {
        Host::Unknown
    }
}

/// True for agents that run as a terminal program and can be followed as a
/// session. IDE-style agents and arbitrary commands are reported only.
pub(crate) fn is_terminal_agent(agent: &AgentKind) -> bool {
    !matches!(
        agent,
        AgentKind::Antigravity | AgentKind::Generic(_) | AgentKind::Shell(_)
    )
}

/// The product name of an agent.
pub(crate) fn agent_label(agent: &AgentKind) -> String {
    match agent {
        AgentKind::ClaudeCode => "Claude Code".into(),
        AgentKind::Codex => "Codex".into(),
        AgentKind::GeminiCli => "Gemini CLI".into(),
        AgentKind::Antigravity => "Antigravity".into(),
        AgentKind::Grok => "Grok CLI".into(),
        AgentKind::CursorCli => "Cursor CLI".into(),
        AgentKind::CopilotCli => "Copilot CLI".into(),
        AgentKind::Aider => "Aider".into(),
        AgentKind::Opencode => "opencode".into(),
        AgentKind::QwenCode => "Qwen Code".into(),
        AgentKind::Generic(name) | AgentKind::Shell(name) => name.clone(),
    }
}

/// Label of a terminal found through its scripting interface.
pub(crate) fn target_label(target: &vca_adapters::TerminalTarget) -> &'static str {
    use vca_adapters::TerminalTarget as T;
    match target {
        T::Tmux { .. } => "tmux",
        T::WezTerm { .. } => "WezTerm",
        T::Kitty { .. } => "kitty",
        T::ITerm2 { .. } => "iTerm2",
        T::WindowsConsole { .. } => "Windows console",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn proc(pid: u32, parent: Option<u32>, name: &str, cmd: &[&str]) -> ProcInfo {
        ProcInfo {
            pid,
            parent,
            name: name.into(),
            cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
            exe: None,
            cwd: Some(PathBuf::from("/work")),
            start_time: 1,
        }
    }

    fn with_exe(mut p: ProcInfo, exe: &str) -> ProcInfo {
        p.exe = Some(PathBuf::from(exe));
        p
    }

    fn host(procs: Vec<ProcInfo>, pid: u32) -> Host {
        host_with(procs, pid, &HashMap::new())
    }

    fn host_with(procs: Vec<ProcInfo>, pid: u32, agents: &HashMap<u32, AgentKind>) -> Host {
        classify_host(pid, &ProcessTable::from_procs(procs), agents)
    }

    #[test]
    fn shells_are_plain() {
        for name in [
            "bash",
            "zsh",
            "fish",
            "sh",
            "cmd.exe",
            "powershell.exe",
            "pwsh.exe",
            "pwsh",
            "-zsh",
            "nu",
        ] {
            assert!(is_plain_shell(&proc(1, None, name, &[name])), "{name}");
        }
        for name in ["claude", "node", "codex.exe", "python3", "gemini"] {
            assert!(!is_plain_shell(&proc(1, None, name, &[name])), "{name}");
        }
    }

    #[test]
    fn windows_terminal_tab() {
        let procs = vec![
            proc(1, None, "explorer.exe", &["explorer.exe"]),
            proc(2, Some(1), "WindowsTerminal.exe", &["WindowsTerminal.exe"]),
            proc(3, Some(2), "pwsh.exe", &["pwsh.exe"]),
            proc(4, Some(3), "claude.exe", &["claude.exe"]),
        ];
        assert_eq!(
            host(procs, 4),
            Host::Terminal(Some("Windows Terminal".into()))
        );
    }

    #[test]
    fn classic_console_window() {
        // A console window: the shell's parent is Explorer, conhost is a
        // child of the shell (not an ancestor).
        let procs = vec![
            proc(1, None, "explorer.exe", &["explorer.exe"]),
            proc(3, Some(1), "cmd.exe", &["cmd.exe"]),
            proc(5, Some(3), "conhost.exe", &["conhost.exe"]),
            proc(4, Some(3), "codex.exe", &["codex.exe"]),
        ];
        assert_eq!(host(procs, 4), Host::Terminal(None));
    }

    #[test]
    fn macos_and_linux_terminals() {
        let mac = vec![
            proc(1, None, "launchd", &["/sbin/launchd"]),
            with_exe(
                proc(
                    2,
                    Some(1),
                    "iTerm2",
                    &["/Applications/iTerm.app/Contents/MacOS/iTerm2"],
                ),
                "/Applications/iTerm.app/Contents/MacOS/iTerm2",
            ),
            proc(3, Some(2), "login", &["login", "-fp", "me"]),
            proc(4, Some(3), "-zsh", &["-zsh"]),
            proc(5, Some(4), "node", &["node", "/opt/homebrew/bin/gemini"]),
        ];
        assert_eq!(host(mac, 5), Host::Terminal(Some("iTerm2".into())));
        let linux = vec![
            proc(1, None, "systemd", &["/sbin/init"]),
            proc(2, Some(1), "tmux: server", &["tmux", "new", "-s", "w"]),
            proc(3, Some(2), "bash", &["-bash"]),
            proc(
                4,
                Some(3),
                "aider",
                &["python3", "/home/u/.local/bin/aider"],
            ),
        ];
        assert_eq!(host(linux, 4), Host::Terminal(Some("tmux".into())));
        let ssh = vec![
            proc(1, None, "systemd", &["/sbin/init"]),
            proc(2, Some(1), "sshd", &["sshd: me@pts/0"]),
            proc(3, Some(2), "bash", &["-bash"]),
            proc(4, Some(3), "claude", &["claude"]),
        ];
        assert_eq!(host(ssh, 4), Host::Terminal(Some("SSH".into())));
    }

    #[test]
    fn codex_inside_the_chatgpt_app() {
        let procs = vec![
            proc(1, None, "explorer.exe", &["explorer.exe"]),
            with_exe(
                proc(2, Some(1), "ChatGPT.exe", &["ChatGPT.exe"]),
                r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.0_x64\app\ChatGPT.exe",
            ),
            with_exe(
                proc(3, Some(2), "codex.exe", &["codex.exe", "app-server"]),
                r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.0_x64\app\resources\codex.exe",
            ),
        ];
        assert_eq!(host(procs, 3), Host::App("ChatGPT".into()));
        // Without the package path: the parent app gives it away.
        let mac = vec![
            proc(1, None, "launchd", &["/sbin/launchd"]),
            proc(2, Some(1), "ChatGPT", &["ChatGPT"]),
            proc(3, Some(2), "codex", &["codex", "app-server"]),
        ];
        assert_eq!(host(mac, 3), Host::App("ChatGPT".into()));
        // An app server with no known parent is still not a terminal.
        let bare = vec![proc(3, None, "codex", &["codex", "app-server"])];
        assert_eq!(host(bare, 3), Host::App("app-server".into()));
    }

    #[test]
    fn desktop_apps_and_editors() {
        // The Claude desktop app shares the `claude.exe` name with the CLI.
        let app = vec![with_exe(
            proc(7, None, "claude.exe", &["claude.exe"]),
            r"C:\Users\me\AppData\Local\AnthropicClaude\app-0.9.0\claude.exe",
        )];
        assert_eq!(host(app, 7), Host::App("Claude".into()));
        let cli = vec![with_exe(
            proc(7, None, "claude.exe", &["claude.exe"]),
            r"C:\Users\me\.local\bin\claude.exe",
        )];
        assert_eq!(host(cli, 7), Host::Unknown);
        // An editor extension starts the agent directly...
        let ext = vec![
            proc(1, None, "Code.exe", &["Code.exe"]),
            proc(2, Some(1), "codex.exe", &["codex.exe", "app-server"]),
        ];
        assert_eq!(host(ext, 2), Host::App("VS Code".into()));
        // ...its integrated terminal runs a shell first.
        let term = vec![
            proc(1, None, "Code.exe", &["Code.exe"]),
            proc(2, Some(1), "pwsh.exe", &["pwsh.exe"]),
            proc(3, Some(2), "claude.exe", &["claude.exe"]),
        ];
        assert_eq!(
            host(term, 3),
            Host::Terminal(Some("VS Code terminal".into()))
        );
        // A macOS app bundle.
        let bundle = vec![with_exe(
            proc(
                4,
                None,
                "Codex",
                &["/Applications/Codex.app/Contents/MacOS/Codex"],
            ),
            "/Applications/Codex.app/Contents/MacOS/Codex",
        )];
        assert!(matches!(host(bundle, 4), Host::App(_)));
    }

    #[test]
    fn agents_started_by_agents() {
        let procs = vec![
            proc(1, None, "WindowsTerminal.exe", &["WindowsTerminal.exe"]),
            proc(2, Some(1), "pwsh.exe", &["pwsh.exe"]),
            proc(3, Some(2), "claude.exe", &["claude.exe"]),
            proc(4, Some(3), "bash.exe", &["bash.exe", "-c", "codex exec hi"]),
            proc(5, Some(4), "codex.exe", &["codex.exe", "exec", "hi"]),
        ];
        let agents = HashMap::from([(3, AgentKind::ClaudeCode), (5, AgentKind::Codex)]);
        assert_eq!(
            host_with(procs.clone(), 5, &agents),
            Host::App("Claude Code".into())
        );
        assert_eq!(
            host_with(procs, 3, &agents),
            Host::Terminal(Some("Windows Terminal".into()))
        );
    }

    #[test]
    fn terminal_agents() {
        assert!(is_terminal_agent(&AgentKind::ClaudeCode));
        assert!(is_terminal_agent(&AgentKind::QwenCode));
        assert!(!is_terminal_agent(&AgentKind::Antigravity));
        assert!(!is_terminal_agent(&AgentKind::Generic("x".into())));
    }
}
