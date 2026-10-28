//! Shell integration snippets (architecture §4.1).
//!
//! The generated snippet defines a function per agent command (for example
//! `claude`). When the user runs it, the function calls
//! `vca run -- <command> <args...>` so that the agent runs in a managed PTY
//! and the terminal becomes an attached view. It falls back to the real
//! command when:
//!
//! * the `vca` binary is not on `PATH`;
//! * the daemon is not reachable (`vca ping --quiet` fails);
//! * `VCA_BYPASS` is set to anything other than an empty string or `0`;
//! * `VCA_MANAGED` is set, which `vca run` exports to managed sessions so
//!   that agents started inside a managed session are not nested.
//!
//! Arguments are passed through unchanged (including spaces and quotes) and
//! the exit code of the agent (or of `vca run`, which must forward it) is
//! returned. `command claude` (bash, zsh, fish) bypasses the wrapper; in
//! PowerShell, `claude.exe`/`claude.cmd` or `$env:VCA_BYPASS = 1` does.

use std::fmt::Write as _;

use crate::error::PtyError;

/// Shells supported by [`script`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

impl Shell {
    pub const ALL: [Shell; 4] = [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell];

    /// Parses a shell name such as `bash`, `zsh`, `fish`, `pwsh` or `powershell`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "pwsh" | "powershell" | "powershell.exe" | "pwsh.exe" => Some(Self::PowerShell),
            _ => None,
        }
    }
}

/// Markers delimiting the snippet so that installers can find and replace it.
pub const BEGIN_MARKER: &str = "# >>> vca shell integration >>>";
pub const END_MARKER: &str = "# <<< vca shell integration <<<";

/// What the snippet wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegration {
    /// Agent commands to wrap, e.g. `claude`, `codex`, `gemini`.
    pub commands: Vec<String>,
    /// Name or absolute path of the `vca` binary.
    pub vca_bin: String,
}

impl Default for ShellIntegration {
    fn default() -> Self {
        Self {
            commands: ["claude", "codex", "gemini"].map(String::from).to_vec(),
            vca_bin: "vca".into(),
        }
    }
}

fn valid_command(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Quotes a string for POSIX shells (and fish) using single quotes.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Quotes a string for fish, which does not support `'\''` inside quotes
/// the same way; it uses `\'` within single quotes.
fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"))
}

/// Quotes a string for PowerShell single-quoted strings.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Generates the snippet for `shell`. Fails if a command name contains
/// characters other than ASCII letters, digits, `_`, `-` and `.`.
pub fn script(shell: Shell, cfg: &ShellIntegration) -> Result<String, PtyError> {
    if let Some(bad) = cfg.commands.iter().find(|c| !valid_command(c)) {
        return Err(PtyError::Unsupported(format!(
            "invalid command name for shell integration: {bad:?}"
        )));
    }
    if cfg.vca_bin.is_empty() || cfg.vca_bin.contains(['\n', '\r', '\0']) {
        return Err(PtyError::Unsupported("invalid vca binary path".into()));
    }
    Ok(match shell {
        Shell::Bash | Shell::Zsh => posix(cfg),
        Shell::Fish => fish(cfg),
        Shell::PowerShell => powershell(cfg),
    })
}

fn posix(cfg: &ShellIntegration) -> String {
    let vca = sh_quote(&cfg.vca_bin);
    let mut s = String::new();
    let _ = writeln!(s, "{BEGIN_MARKER}");
    let _ = write!(
        s,
        r#"__vca_should_manage() {{
  case "${{VCA_BYPASS:-}}" in ''|0) ;; *) return 1 ;; esac
  [ -z "${{VCA_MANAGED:-}}" ] || return 1
  command -v {vca} >/dev/null 2>&1 || return 1
  command {vca} ping --quiet >/dev/null 2>&1
}}
__vca_run() {{
  local __vca_cmd="$1"
  shift
  if __vca_should_manage; then
    command {vca} run -- "$__vca_cmd" "$@"
  else
    command "$__vca_cmd" "$@"
  fi
}}
"#
    );
    for cmd in &cfg.commands {
        // The `function` keyword keeps an existing alias from being expanded
        // in the definition.
        let _ = writeln!(s, "function {cmd} {{ __vca_run {cmd} \"$@\"; }}");
    }
    let _ = writeln!(s, "{END_MARKER}");
    s
}

fn fish(cfg: &ShellIntegration) -> String {
    let vca = fish_quote(&cfg.vca_bin);
    let mut s = String::new();
    let _ = writeln!(s, "{BEGIN_MARKER}");
    let _ = write!(
        s,
        r#"function __vca_should_manage
    if set -q VCA_BYPASS; and test -n "$VCA_BYPASS"; and test "$VCA_BYPASS" != 0
        return 1
    end
    if set -q VCA_MANAGED; and test -n "$VCA_MANAGED"
        return 1
    end
    command -q {vca}; or return 1
    command {vca} ping --quiet >/dev/null 2>&1
end
function __vca_run
    set -l __vca_cmd $argv[1]
    set -e argv[1]
    if __vca_should_manage
        command {vca} run -- $__vca_cmd $argv
    else
        command $__vca_cmd $argv
    end
end
"#
    );
    for cmd in &cfg.commands {
        let _ = writeln!(
            s,
            "function {cmd} --wraps {cmd} --description 'Run {cmd} under VCA'\n    __vca_run {cmd} $argv\nend"
        );
    }
    let _ = writeln!(s, "{END_MARKER}");
    s
}

fn powershell(cfg: &ShellIntegration) -> String {
    let vca = ps_quote(&cfg.vca_bin);
    let mut s = String::new();
    let _ = writeln!(s, "{BEGIN_MARKER}");
    let _ = write!(
        s,
        r#"function global:__vca_app([string]$Name) {{
    Get-Command -Name $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
}}
function global:__vca_should_manage {{
    if ($env:VCA_BYPASS -and $env:VCA_BYPASS -ne '0') {{ return $false }}
    if ($env:VCA_MANAGED) {{ return $false }}
    $vca = __vca_app {vca}
    if (-not $vca) {{ return $false }}
    & $vca.Source ping --quiet *> $null
    return ($LASTEXITCODE -eq 0)
}}
function global:__vca_run {{
    $name = $args[0]
    $rest = @($args | Select-Object -Skip 1)
    if (__vca_should_manage) {{
        $vca = __vca_app {vca}
        & $vca.Source run '--' $name @rest
    }} else {{
        $app = __vca_app $name
        if (-not $app) {{
            Write-Error "${{name}}: command not found"
            $global:LASTEXITCODE = 127
            return
        }}
        & $app.Source @rest
    }}
}}
"#
    );
    for cmd in &cfg.commands {
        let _ = writeln!(s, "function global:{cmd} {{ __vca_run '{cmd}' @args }}");
    }
    let _ = writeln!(s, "{END_MARKER}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ShellIntegration {
        ShellIntegration {
            commands: vec!["claude".into(), "codex".into()],
            vca_bin: "vca".into(),
        }
    }

    #[test]
    fn rejects_unsafe_command_names() {
        for bad in ["", "-x", "a b", "a;rm", "$(x)", "a'b"] {
            let c = ShellIntegration {
                commands: vec![bad.into()],
                ..cfg()
            };
            assert!(script(Shell::Bash, &c).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn posix_snippet_shape() {
        let s = script(Shell::Bash, &cfg()).unwrap();
        assert!(s.starts_with(BEGIN_MARKER));
        assert!(s.trim_end().ends_with(END_MARKER));
        assert!(s.contains(r#"function claude { __vca_run claude "$@"; }"#));
        assert!(s.contains(r#"function codex { __vca_run codex "$@"; }"#));
        assert!(s.contains(r#"command 'vca' run -- "$__vca_cmd" "$@""#));
        assert!(s.contains(r#"command "$__vca_cmd" "$@""#));
        assert!(s.contains("VCA_BYPASS"));
        assert!(s.contains("VCA_MANAGED"));
        assert_eq!(s, script(Shell::Zsh, &cfg()).unwrap());
    }

    #[test]
    fn fish_snippet_shape() {
        let s = script(Shell::Fish, &cfg()).unwrap();
        assert!(s.contains("function claude --wraps claude"));
        assert!(s.contains("    __vca_run claude $argv\nend"));
        assert!(s.contains("command 'vca' run -- $__vca_cmd $argv"));
        assert!(s.contains("set -q VCA_BYPASS"));
    }

    #[test]
    fn powershell_snippet_shape() {
        let s = script(Shell::PowerShell, &cfg()).unwrap();
        assert!(s.contains("function global:claude { __vca_run 'claude' @args }"));
        assert!(s.contains("& $vca.Source run '--' $name @rest"));
        assert!(s.contains("-CommandType Application"));
        assert!(s.contains("$env:VCA_BYPASS"));
    }

    #[test]
    fn quotes_vca_path() {
        let c = ShellIntegration {
            vca_bin: "/opt/it's here/vca".into(),
            ..cfg()
        };
        assert!(
            script(Shell::Bash, &c)
                .unwrap()
                .contains(r"'/opt/it'\''s here/vca'")
        );
        assert!(
            script(Shell::Fish, &c)
                .unwrap()
                .contains(r"'/opt/it\'s here/vca'")
        );
        assert!(
            script(Shell::PowerShell, &c)
                .unwrap()
                .contains("'/opt/it''s here/vca'")
        );
    }

    #[test]
    fn shell_names() {
        assert_eq!(Shell::from_name("PWSH"), Some(Shell::PowerShell));
        assert_eq!(Shell::from_name("zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_name("csh"), None);
    }

    /// Runs the generated bash snippet against dummy `vca` and `claude`
    /// executables on PATH.
    #[cfg(unix)]
    #[test]
    fn bash_wrapper_end_to_end() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        if Command::new("bash").arg("-c").arg("true").status().is_err() {
            eprintln!("bash not available; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let write_exe = |name: &str, body: &str| {
            let p = bin.join(name);
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // Dummy vca: `ping` succeeds unless DAEMON_DOWN is set; `run` prints
        // its arguments one per line and exits 7.
        write_exe(
            "vca",
            "#!/bin/sh\nif [ \"$1\" = ping ]; then [ -z \"$DAEMON_DOWN\" ]; exit $?; fi\n\
             echo VCA; for a in \"$@\"; do echo \"[$a]\"; done; exit 7\n",
        );
        write_exe(
            "claude",
            "#!/bin/sh\necho REAL; for a in \"$@\"; do echo \"[$a]\"; done; exit 3\n",
        );
        let snippet = dir.path().join("vca.bash");
        std::fs::write(&snippet, script(Shell::Bash, &cfg()).unwrap()).unwrap();
        let path = format!("{}:/usr/bin:/bin", bin.display());

        let run = |path: &str, env: &[(&str, &str)], body: &str| {
            let mut cmd = Command::new("bash");
            cmd.arg("--noprofile")
                .arg("--norc")
                .arg("-c")
                .arg(format!(". '{}'\n{body}", snippet.display()))
                .env("PATH", path)
                .env_remove("VCA_BYPASS")
                .env_remove("VCA_MANAGED")
                .env_remove("DAEMON_DOWN");
            for (k, v) in env {
                cmd.env(k, v);
            }
            let out = cmd.output().unwrap();
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                out.status.code(),
            )
        };

        // Managed: args with spaces preserved, exit code of `vca run`.
        let (out, code) = run(&path, &[], "claude 'hello world' -p \"a  b\"");
        assert_eq!(
            out,
            "VCA\n[run]\n[--]\n[claude]\n[hello world]\n[-p]\n[a  b]\n"
        );
        assert_eq!(code, Some(7));

        // Daemon unreachable: falls back to the real command.
        let (out, code) = run(&path, &[("DAEMON_DOWN", "1")], "claude 'x y'");
        assert_eq!(out, "REAL\n[x y]\n");
        assert_eq!(code, Some(3));

        // Bypass via env var and via `command`.
        let (out, _) = run(&path, &[("VCA_BYPASS", "1")], "claude a");
        assert_eq!(out, "REAL\n[a]\n");
        let (out, _) = run(&path, &[("VCA_BYPASS", "0")], "claude a");
        assert!(out.starts_with("VCA\n"));
        let (out, code) = run(&path, &[], "command claude a");
        assert_eq!(out, "REAL\n[a]\n");
        assert_eq!(code, Some(3));

        // Nested managed session: no recursion.
        let (out, _) = run(&path, &[("VCA_MANAGED", "1")], "claude");
        assert_eq!(out, "REAL\n");

        // No vca on PATH: real command.
        std::fs::remove_file(bin.join("vca")).unwrap();
        let (out, code) = run(&path, &[], "claude b; echo \"status=$?\"");
        assert_eq!(out, "REAL\n[b]\nstatus=3\n");
        assert_eq!(code, Some(0));
    }
}
