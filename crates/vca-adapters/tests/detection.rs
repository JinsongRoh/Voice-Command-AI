//! Every agent the adapters know is recognized from its process name or its
//! command line on Windows, macOS and Linux, including CLIs that run under
//! `node`, `bun` or `python`; plain shells are never taken for an agent.
//!
//! Made-up process entries only: nothing here reads a real process.

use std::path::PathBuf;

use vca_adapters::{ProcInfo, ProcessTable, builtin_adapters};
use vca_core::AgentKind;

fn proc(name: &str, cmd: &[&str]) -> ProcInfo {
    ProcInfo {
        pid: 4_200_000,
        parent: None,
        name: name.into(),
        cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
        exe: None,
        cwd: Some(PathBuf::from("/work")),
        start_time: 1,
    }
}

/// The agents whose matcher accepts `p`.
fn detect(p: &ProcInfo) -> Vec<AgentKind> {
    builtin_adapters()
        .iter()
        .filter(|a| a.process_matcher().matches(p))
        .map(|a| a.kind())
        .collect()
}

#[track_caller]
fn expect(agent: AgentKind, name: &str, cmd: &[&str]) {
    let found = detect(&proc(name, cmd));
    assert_eq!(found, vec![agent.clone()], "{agent:?} from {name} {cmd:?}");
}

#[test]
fn claude_code() {
    let a = AgentKind::ClaudeCode;
    // Native installer.
    expect(
        a.clone(),
        "claude.exe",
        &[r"C:\Users\me\.local\bin\claude.exe"],
    );
    expect(a.clone(), "claude", &["/Users/me/.local/bin/claude"]);
    expect(a.clone(), "claude", &["claude", "--resume", "abc"]);
    // npm, under node.
    expect(
        a.clone(),
        "node.exe",
        &[
            r"C:\Program Files\nodejs\node.exe",
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js",
        ],
    );
    expect(
        a.clone(),
        "node",
        &[
            "node",
            "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        ],
    );
    expect(
        a,
        "node",
        &[
            "/usr/bin/node",
            "--no-warnings",
            "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        ],
    );
}

#[test]
fn codex() {
    let a = AgentKind::Codex;
    expect(
        a.clone(),
        "codex.exe",
        &[
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\vendor\x86_64-pc-windows-msvc\codex\codex.exe",
        ],
    );
    expect(
        a.clone(),
        "node.exe",
        &[
            r"C:\Program Files\nodejs\node.exe",
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js",
        ],
    );
    expect(
        a.clone(),
        "codex",
        &["/opt/homebrew/lib/node_modules/@openai/codex/vendor/aarch64-apple-darwin/codex/codex"],
    );
    expect(
        a,
        "node",
        &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"],
    );
}

#[test]
fn gemini_cli() {
    let a = AgentKind::GeminiCli;
    expect(
        a.clone(),
        "node.exe",
        &[
            r"C:\Program Files\nodejs\node.exe",
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@google\gemini-cli\dist\index.js",
        ],
    );
    // A `#!/usr/bin/env node` script started through its bin link.
    expect(a.clone(), "node", &["node", "/opt/homebrew/bin/gemini"]);
    expect(a.clone(), "node", &["node", "/usr/local/bin/gemini"]);
    expect(
        a.clone(),
        "node",
        &[
            "node",
            "--no-warnings=DEP0040",
            "/usr/lib/node_modules/@google/gemini-cli/dist/index.js",
        ],
    );
    // A node option with its value as the next argument.
    expect(
        a.clone(),
        "node",
        &[
            "node",
            "--max-old-space-size",
            "8192",
            "/usr/lib/node_modules/@google/gemini-cli/dist/index.js",
        ],
    );
    // The bundled build and bun.
    expect(
        a.clone(),
        "node",
        &[
            "node",
            "/usr/lib/node_modules/@google/gemini-cli/bundle/gemini.js",
        ],
    );
    expect(a.clone(), "bun", &["bun", "/home/me/.bun/bin/gemini"]);
    // Renamed process title.
    expect(a, "gemini", &["gemini"]);
}

#[test]
fn grok_cli() {
    let a = AgentKind::Grok;
    expect(a.clone(), "grok.exe", &["grok.exe"]);
    expect(
        a.clone(),
        "node.exe",
        &[
            r"C:\Program Files\nodejs\node.exe",
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@vibe-kit\grok-cli\dist\index.js",
        ],
    );
    expect(a.clone(), "node", &["node", "/usr/local/bin/grok"]);
    expect(
        a,
        "bun",
        &[
            "bun",
            "/opt/homebrew/lib/node_modules/@vibe-kit/grok-cli/dist/index.js",
        ],
    );
}

#[test]
fn cursor_cli() {
    let a = AgentKind::CursorCli;
    expect(
        a.clone(),
        "cursor-agent",
        &["/Users/me/.local/bin/cursor-agent"],
    );
    expect(a.clone(), "cursor-agent.exe", &["cursor-agent.exe"]);
    // The installer ships its own node next to the script.
    expect(
        a.clone(),
        "node",
        &[
            "/home/me/.local/share/cursor-agent/versions/2025.11.06-8fe8a63/node",
            "--use-system-ca",
            "/home/me/.local/share/cursor-agent/versions/2025.11.06-8fe8a63/index.js",
        ],
    );
    expect(
        a,
        "node.exe",
        &[
            r"C:\Users\me\AppData\Local\cursor-agent\versions\2025.11.06\node.exe",
            r"C:\Users\me\AppData\Local\cursor-agent\versions\2025.11.06\index.js",
        ],
    );
}

#[test]
fn copilot_cli() {
    let a = AgentKind::CopilotCli;
    expect(a.clone(), "copilot.exe", &["copilot.exe"]);
    expect(
        a.clone(),
        "node.exe",
        &[
            r"C:\Program Files\nodejs\node.exe",
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@github\copilot\index.js",
        ],
    );
    expect(
        a.clone(),
        "node",
        &[
            "node",
            "/opt/homebrew/lib/node_modules/@github/copilot/index.js",
        ],
    );
    expect(a, "node", &["node", "/usr/local/bin/copilot"]);
}

#[test]
fn aider() {
    let a = AgentKind::Aider;
    expect(
        a.clone(),
        "aider.exe",
        &[r"C:\Users\me\.local\bin\aider.exe"],
    );
    expect(a.clone(), "python.exe", &["python.exe", "-m", "aider"]);
    expect(
        a.clone(),
        "python3.12",
        &["python3.12", "/home/me/.local/bin/aider", "--model", "x"],
    );
    // macOS framework Python names its process `Python`.
    expect(
        a.clone(),
        "Python",
        &[
            "/Library/Frameworks/Python.framework/Versions/3.12/Resources/Python.app/Contents/MacOS/Python",
            "/Users/me/.local/bin/aider",
        ],
    );
    expect(
        a,
        "python",
        &[
            "/home/me/.local/share/uv/tools/aider-chat/bin/python",
            "/home/me/.local/bin/aider",
        ],
    );
}

#[test]
fn opencode() {
    let a = AgentKind::Opencode;
    expect(a.clone(), "opencode.exe", &["opencode.exe"]);
    expect(a.clone(), "opencode", &["/home/me/.opencode/bin/opencode"]);
    expect(
        a,
        "node",
        &["node", "/usr/lib/node_modules/opencode-ai/bin/opencode"],
    );
}

#[test]
fn qwen_code() {
    let a = AgentKind::QwenCode;
    expect(a.clone(), "qwen", &["qwen"]);
    expect(
        a.clone(),
        "node.exe",
        &[
            r"C:\Program Files\nodejs\node.exe",
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@qwen-code\qwen-code\cli.js",
        ],
    );
    expect(a.clone(), "node", &["node", "/opt/homebrew/bin/qwen"]);
    expect(
        a,
        "node",
        &[
            "node",
            "/usr/lib/node_modules/@qwen-code/qwen-code/dist/index.js",
        ],
    );
}

#[test]
fn antigravity() {
    expect(
        AgentKind::Antigravity,
        "Antigravity.exe",
        &["Antigravity.exe"],
    );
    expect(
        AgentKind::Antigravity,
        "antigravity",
        &["/usr/bin/antigravity"],
    );
}

#[test]
fn plain_shells_and_other_programs_are_not_agents() {
    for (name, cmd) in [
        ("cmd.exe", vec!["cmd.exe", "/d", "/s", "/c", "\"claude\""]),
        (
            "powershell.exe",
            vec![
                "powershell.exe",
                "-File",
                r"C:\Users\me\AppData\Roaming\npm\gemini.ps1",
            ],
        ),
        ("pwsh.exe", vec!["pwsh.exe", "-NoLogo"]),
        ("bash", vec!["bash", "-c", "codex exec hi"]),
        ("zsh", vec!["-zsh"]),
        ("sh", vec!["sh", "/usr/local/bin/cursor-agent"]),
        ("fish", vec!["fish"]),
        ("WindowsTerminal.exe", vec!["WindowsTerminal.exe"]),
        ("node", vec!["node", "/srv/app/server.js"]),
        ("node", vec!["node"]),
        ("python3", vec!["python3", "-m", "http.server"]),
        (
            "node",
            vec![
                "node",
                "/usr/lib/node_modules/npm/bin/npx-cli.js",
                "prettier",
            ],
        ),
        ("vim", vec!["vim", "claude.md"]),
        ("git", vec!["git", "commit"]),
    ] {
        assert!(
            detect(&proc(name, &cmd)).is_empty(),
            "{name} {cmd:?} taken for an agent"
        );
    }
}

#[test]
fn a_wrapper_and_its_binary_count_once() {
    let mut node = proc(
        "node",
        &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"],
    );
    node.pid = 4_200_001;
    let mut binary = proc(
        "codex",
        &["/usr/lib/node_modules/@openai/codex/vendor/x86_64-unknown-linux-musl/codex/codex"],
    );
    binary.pid = 4_200_002;
    binary.parent = Some(node.pid);
    let table = ProcessTable::from_procs(vec![node, binary]);
    let codex = builtin_adapters()
        .into_iter()
        .find(|a| a.kind() == AgentKind::Codex)
        .unwrap();
    let found: Vec<u32> = table
        .find(&codex.process_matcher())
        .iter()
        .map(|p| p.pid)
        .collect();
    assert_eq!(found, vec![4_200_001]);
}
