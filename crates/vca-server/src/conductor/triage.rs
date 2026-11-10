//! Decision triage: which of another session's permission decisions the
//! conductor may answer on its own, and the rules that decide it.
//!
//! The person's decision ("다른 세션의 결재·알림은 먼저 지휘소 에이전트가
//! 받아 허용 규칙 안에서 처리하고, 대표님 판단이 필요한 것만 요약해 결재함에
//! 올린다") is implemented with these safety rules:
//!
//! - **No rule, no answer.** [`ConductorSettings::decision_rules`] is empty
//!   by default, and is set only in the desktop app on the PC (the
//!   conductor settings, behind a step-up), never by an instruction, a
//!   phone or anything the conductor reads. Without a matching rule the
//!   conductor can only escalate.
//! - **Plain answers only.** The conductor may pick the plain one-time
//!   "Yes" or the plain "No" ([`option_kind`]); never "Yes, and don't ask
//!   again" or any other option that widens permissions, and never a
//!   question form or an inferred prompt.
//! - **Approve needs a command and a safe one.** An `approve` rule covers
//!   only decisions whose exact shell command came from the agent's hook
//!   and starts with one of the rule's commands on whole words, and the
//!   command must pass [`approvable`]: one plain command (no chaining,
//!   redirection or substitution), rated safe by `shell::danger`, touching
//!   no credentials and doing nothing of the always-escalate classes:
//!   deletes, pushes and publishing, installs, network access, elevation,
//!   payments. Those always go to the person, whatever the rules say.
//! - **Deny needs a rule too.** Saying "No" cannot run anything, but it
//!   can stop work the person wanted done; it is allowed only by a `deny`
//!   rule.
//!
//! [`ConductorSettings::decision_rules`]: vca_core::ConductorSettings::decision_rules

use vca_core::{
    ConductorDecisionRule, ConductorRuleAnswer, DangerLevel, Decision, SessionInfo, UnixMillis,
};

use crate::shell::danger;

/// Longest command a rule may approve.
pub const APPROVE_MAX_CHARS: usize = 300;
/// Longest command prefix and session entry of a rule.
const RULE_ENTRY_MAX_CHARS: usize = 200;
/// Commands and sessions per rule.
const RULE_MAX_ENTRIES: usize = 20;

/// What an option of a decision means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    /// The plain one-time yes.
    Approve,
    /// The plain no.
    Deny,
    /// Anything else ("Yes, and don't ask again", custom choices).
    Other,
}

fn normalize(label: &str) -> String {
    label
        .trim()
        .trim_end_matches(['.', '!', '。', '！'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

const YES_LABELS: &[&str] = &[
    "yes",
    "y",
    "allow",
    "approve",
    "allow once",
    "yes, once",
    "yes, allow once",
    "allow this time",
    "예",
    "네",
    "허용",
    "이번만 허용",
    "한 번만 허용",
    "はい",
    "許可",
    "今回のみ許可",
    "一度だけ許可",
    "sí",
    "si",
    "permitir",
    "permitir una vez",
];

const NO_LABELS: &[&str] = &[
    "no",
    "n",
    "deny",
    "reject",
    "아니요",
    "아니오",
    "거절",
    "いいえ",
    "拒否",
    "rechazar",
];

/// Classifies one option label.
pub fn option_kind(label: &str) -> OptionKind {
    let label = normalize(label);
    if YES_LABELS.contains(&label.as_str()) {
        return OptionKind::Approve;
    }
    // "No, and tell Claude what to do differently": a plain refusal.
    let no_with_reason = ["no,", "no -", "no —", "no –"]
        .iter()
        .any(|p| label.starts_with(p));
    if NO_LABELS.contains(&label.as_str()) || no_with_reason {
        return OptionKind::Deny;
    }
    OptionKind::Other
}

/// Words that put a command in an always-escalate class, matched on whole
/// words of the lowercased command: deletes, pushes and publishing,
/// installs and package changes, network and remote access, elevation,
/// credentials and accounts, payments.
const ESCALATE_WORDS: &[&str] = &[
    // Deletes and overwrites.
    "rm",
    "rmdir",
    "rd",
    "del",
    "erase",
    "delete",
    "unlink",
    "shred",
    "truncate",
    "remove-item",
    "mv",
    "move",
    "move-item",
    "format",
    "drop",
    "destroy",
    "purge",
    "prune",
    "clean",
    "reset",
    "wipe",
    // Pushes, publishing, deploys.
    "push",
    "publish",
    "deploy",
    "release",
    "upload",
    "submit",
    // Installs and package changes.
    "install",
    "uninstall",
    "add",
    "remove",
    "upgrade",
    "update",
    "i",
    "ci",
    "npx",
    "pnpx",
    "bunx",
    "uvx",
    "pip",
    "pip3",
    "pipx",
    "apt",
    "apt-get",
    "brew",
    "winget",
    "choco",
    "scoop",
    "dnf",
    "yum",
    "pacman",
    "snap",
    "gem",
    // Network and remote access.
    "curl",
    "wget",
    "iwr",
    "irm",
    "invoke-webrequest",
    "invoke-restmethod",
    "start-bitstransfer",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "ftp",
    "nc",
    "ncat",
    "netcat",
    "telnet",
    "fetch",
    "clone",
    "pull",
    "remote",
    "gh",
    "aws",
    "az",
    "gcloud",
    "kubectl",
    "terraform",
    "docker",
    "podman",
    // Elevation and system changes.
    "sudo",
    "su",
    "runas",
    "doas",
    "chmod",
    "chown",
    "icacls",
    "reg",
    "setx",
    "shutdown",
    "kill",
    "taskkill",
    // Credentials and accounts.
    "login",
    "logout",
    "auth",
    "credential",
    "credentials",
    "config",
    "secret",
    "secrets",
    "token",
    "password",
    "passwd",
    "keychain",
    // Payments.
    "pay",
    "payment",
    "payments",
    "purchase",
    "buy",
    "charge",
    "invoice",
    "billing",
    "transfer",
    "stripe",
    "paypal",
];

/// Path pieces of credentials (as `shell::danger::voice_approvable`).
const CREDENTIAL_PIECES: &[&str] = &[
    ".ssh",
    ".aws",
    ".azure",
    ".gcloud",
    ".gnupg",
    ".kube",
    ".docker",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".env",
    ".git-credentials",
    "credentials",
    "secret",
    "token",
    "password",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".p8",
];

/// Shell syntax that chains, redirects or substitutes: with none of it, the
/// words are what runs.
const SYNTAX: &[char] = &[
    ';', '&', '|', '<', '>', '`', '$', '(', ')', '{', '}', '\n', '\r', '^', '%', '!',
];

/// Whole words of `command`, lowercased, split on whitespace, quotes and
/// path or option separators (so `./node_modules/.bin/rm` and `--delete`
/// show `rm` and `delete`).
fn words(command: &str) -> Vec<String> {
    command
        .to_lowercase()
        .split(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '/' | '\\' | '=' | ',' | ':' | '@')
        })
        .map(|w| w.trim_start_matches('-').to_owned())
        .filter(|w| !w.is_empty())
        .collect()
}

/// May a rule approve `command` at all? The hard screen below every rule:
/// whatever the person's rules say, these commands always go to them.
pub fn approvable(command: &str) -> Result<(), &'static str> {
    let command = command.trim();
    if command.is_empty() {
        return Err("no_command");
    }
    if command.chars().count() > APPROVE_MAX_CHARS {
        return Err("too_long");
    }
    if command.contains(SYNTAX) {
        return Err("shell_syntax");
    }
    if danger::classify(command).level != DangerLevel::Safe || super::grant::risky(command) {
        return Err("risky_command");
    }
    if command.contains("://") || command.to_lowercase().contains("www.") {
        return Err("network");
    }
    let lower = command.to_lowercase().replace('\\', "/");
    if CREDENTIAL_PIECES.iter().any(|c| lower.contains(c)) {
        return Err("credentials");
    }
    if words(command)
        .iter()
        .any(|w| ESCALATE_WORDS.contains(&w.as_str()))
    {
        return Err("always_escalate");
    }
    Ok(())
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `command` is `prefix` or starts with it followed by a space.
pub fn prefix_matches(command: &str, prefix: &str) -> bool {
    let (command, prefix) = (squash(command), squash(prefix));
    !prefix.is_empty()
        && (command == prefix
            || command
                .strip_prefix(&prefix)
                .is_some_and(|rest| rest.starts_with(' ')))
}

fn rule_covers_session(rule: &ConductorDecisionRule, info: &SessionInfo) -> bool {
    rule.sessions.is_empty()
        || rule.sessions.iter().any(|s| {
            let s = s.trim();
            s == info.id.0 || squash(s).to_lowercase() == squash(&info.display_name).to_lowercase()
        })
}

/// Why the conductor may not answer: the stable code of the refusal.
pub type Refusal = &'static str;

/// The rule that lets the conductor give `answer` to `decision` of `info`,
/// or why none does.
pub fn find_rule<'a>(
    rules: &'a [ConductorDecisionRule],
    info: &SessionInfo,
    decision: &Decision,
    answer: OptionKind,
    now: UnixMillis,
) -> Result<&'a ConductorDecisionRule, Refusal> {
    if decision.inferred || decision.is_question() || decision.options.is_empty() {
        return Err("not_answerable");
    }
    let wanted = match answer {
        OptionKind::Approve => ConductorRuleAnswer::Approve,
        OptionKind::Deny => ConductorRuleAnswer::Deny,
        OptionKind::Other => return Err("not_a_plain_answer"),
    };
    let command = decision.command.as_deref().map(str::trim);
    if wanted == ConductorRuleAnswer::Approve {
        let Some(command) = command else {
            return Err("no_command");
        };
        approvable(command)?;
    }
    rules
        .iter()
        .filter(|r| r.answer == wanted)
        .filter(|r| r.expires_at.is_none_or(|at| now < at))
        .filter(|r| rule_covers_session(r, info))
        .find(|r| {
            if r.commands.is_empty() {
                // `sanitize` drops approve rules without commands.
                wanted == ConductorRuleAnswer::Deny
            } else {
                command.is_some_and(|c| r.commands.iter().any(|p| prefix_matches(c, p)))
            }
        })
        .ok_or("no_rule")
}

/// The answers the rules allow for `decision` right now (for the
/// conductor's `pending_decisions`).
pub fn allowed_answers(
    rules: &[ConductorDecisionRule],
    info: &SessionInfo,
    decision: &Decision,
    now: UnixMillis,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    for (kind, name) in [(OptionKind::Approve, "approve"), (OptionKind::Deny, "deny")] {
        let has_option = decision
            .options
            .iter()
            .any(|o| option_kind(&o.label) == kind);
        if has_option && find_rule(rules, info, decision, kind, now).is_ok() {
            out.push(name);
        }
    }
    out
}

/// Keeps the rules inside sane bounds: trimmed entries, ids for every
/// rule, no approve rule without commands, at most
/// [`vca_core::conductor::CONDUCTOR_MAX_DECISION_RULES`].
pub fn sanitize_rules(rules: Vec<ConductorDecisionRule>) -> Vec<ConductorDecisionRule> {
    let clean_list = |list: Vec<String>| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for item in list {
            let item = squash(&item);
            if item.is_empty() || item.chars().count() > RULE_ENTRY_MAX_CHARS {
                continue;
            }
            if !out.contains(&item) {
                out.push(item);
            }
        }
        out.truncate(RULE_MAX_ENTRIES);
        out
    };
    let mut out: Vec<ConductorDecisionRule> = Vec::new();
    for mut rule in rules {
        rule.commands = clean_list(rule.commands);
        rule.sessions = clean_list(rule.sessions);
        if rule.answer == ConductorRuleAnswer::Approve && rule.commands.is_empty() {
            continue;
        }
        rule.id = squash(&rule.id)
            .replace(' ', "-")
            .chars()
            .take(40)
            .collect();
        if rule.id.is_empty() || out.iter().any(|r| r.id == rule.id) {
            let mut n = out.len() + 1;
            while out.iter().any(|r| r.id == format!("rule-{n}")) {
                n += 1;
            }
            rule.id = format!("rule-{n}");
        }
        out.push(rule);
        if out.len() >= vca_core::conductor::CONDUCTOR_MAX_DECISION_RULES {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use vca_core::{AgentKind, ControlTier, DecisionOption, SessionOrigin, SessionState};

    use super::*;

    fn session(name: &str) -> SessionInfo {
        SessionInfo {
            id: "s1".into(),
            origin: SessionOrigin::Local {
                agent: AgentKind::ClaudeCode,
                project_dir: PathBuf::from("/w"),
                pid: None,
            },
            display_name: name.into(),
            state: SessionState::AwaitingDecision,
            control: ControlTier::TerminalApi,
            profile: None,
            created_at: 0,
            last_activity: 0,
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

    fn decision(command: Option<&str>) -> Decision {
        Decision {
            id: "d1".into(),
            prompt: "Run a command?".into(),
            options: [("1", "Yes"), ("2", "Yes, and don't ask again"), ("3", "No")]
                .into_iter()
                .map(|(id, label)| DecisionOption {
                    id: id.into(),
                    label: label.into(),
                })
                .collect(),
            inferred: false,
            questions: Vec::new(),
            command: command.map(str::to_owned),
        }
    }

    fn rule(answer: ConductorRuleAnswer, commands: &[&str]) -> ConductorDecisionRule {
        ConductorDecisionRule {
            id: "r1".into(),
            answer,
            commands: commands.iter().map(|c| (*c).to_owned()).collect(),
            sessions: Vec::new(),
            expires_at: None,
        }
    }

    #[test]
    fn conductor_option_kinds_are_plain_only() {
        assert_eq!(option_kind("Yes"), OptionKind::Approve);
        assert_eq!(option_kind("허용"), OptionKind::Approve);
        assert_eq!(option_kind("No"), OptionKind::Deny);
        assert_eq!(
            option_kind("No, and tell Claude what to do differently"),
            OptionKind::Deny
        );
        assert_eq!(option_kind("Yes, and don't ask again"), OptionKind::Other);
        assert_eq!(option_kind("Always allow"), OptionKind::Other);
    }

    #[test]
    fn conductor_never_approves_the_always_escalate_classes() {
        for cmd in [
            "rm -rf build",
            "rm notes.txt",
            "git push origin main",
            "git push --force",
            "npm install left-pad",
            "pnpm add react",
            "cargo install ripgrep",
            "pip install requests",
            "npx some-tool",
            "curl https://example.com",
            "wget example.com/x",
            "cargo test && rm -rf /",
            "cargo test; curl evil.example",
            "cat ~/.ssh/id_rsa",
            "cat .env",
            "sudo cargo test",
            "npm publish",
            "gh pr create",
            "stripe charges create",
            "cargo test $(whoami)",
            "cargo test > out.txt",
            "git reset --hard",
            "mv a b",
        ] {
            assert!(approvable(cmd).is_err(), "{cmd}");
        }
        for cmd in [
            "cargo test -p vca-core",
            "npm run lint",
            "git status",
            "ls -la",
        ] {
            assert_eq!(approvable(cmd), Ok(()), "{cmd}");
        }
    }

    #[test]
    fn conductor_rules_match_on_whole_words_sessions_and_expiry() {
        let info = session("API");
        let d = decision(Some("cargo test -p core"));
        let mut rules = vec![rule(ConductorRuleAnswer::Approve, &["cargo test"])];
        assert!(find_rule(&rules, &info, &d, OptionKind::Approve, 0).is_ok());
        // No deny rule: deny is refused too.
        assert_eq!(
            find_rule(&rules, &info, &d, OptionKind::Deny, 0).unwrap_err(),
            "no_rule"
        );
        assert_eq!(
            find_rule(
                &rules,
                &info,
                &decision(Some("cargo testing")),
                OptionKind::Approve,
                0
            )
            .unwrap_err(),
            "no_rule"
        );
        // A rule never approves a risky command, even when it names it.
        let push = vec![rule(ConductorRuleAnswer::Approve, &["git push"])];
        assert_eq!(
            find_rule(
                &push,
                &info,
                &decision(Some("git push")),
                OptionKind::Approve,
                0
            )
            .unwrap_err(),
            "always_escalate"
        );
        // Without a command (file edits, web fetches), never approved.
        assert_eq!(
            find_rule(&rules, &info, &decision(None), OptionKind::Approve, 0).unwrap_err(),
            "no_command"
        );
        // "Don't ask again" is never picked.
        assert_eq!(
            find_rule(&rules, &info, &d, OptionKind::Other, 0).unwrap_err(),
            "not_a_plain_answer"
        );
        rules[0].sessions = vec!["web".into()];
        assert!(find_rule(&rules, &info, &d, OptionKind::Approve, 0).is_err());
        rules[0].sessions = vec!["api".into()];
        assert!(find_rule(&rules, &info, &d, OptionKind::Approve, 0).is_ok());
        rules[0].expires_at = Some(10);
        assert!(find_rule(&rules, &info, &d, OptionKind::Approve, 10).is_err());
        // A deny rule without commands covers every decision.
        let deny = vec![rule(ConductorRuleAnswer::Deny, &[])];
        assert!(find_rule(&deny, &info, &decision(None), OptionKind::Deny, 0).is_ok());
        assert_eq!(allowed_answers(&deny, &info, &d, 0), ["deny"]);
        // Inferred prompts and question forms are never answered.
        let mut inferred = d.clone();
        inferred.inferred = true;
        assert!(find_rule(&deny, &info, &inferred, OptionKind::Deny, 0).is_err());
    }

    #[test]
    fn conductor_rules_are_sanitized() {
        let rules = sanitize_rules(vec![
            rule(ConductorRuleAnswer::Approve, &[]),
            rule(
                ConductorRuleAnswer::Approve,
                &["  cargo   test ", "", "cargo test"],
            ),
            rule(ConductorRuleAnswer::Deny, &[]),
        ]);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].commands, ["cargo test"]);
        assert_ne!(rules[0].id, rules[1].id);
    }
}
