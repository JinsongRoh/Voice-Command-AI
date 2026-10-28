//! State detection from terminal output, for agents without hooks.
//!
//! [`OutputHeuristics::analyze`] looks at the last lines of a session's
//! terminal output and guesses whether the agent is asking a question with
//! fixed answers (numbered menus, `[y/N]`, aider's `(Y)es/(N)o`), working
//! (`esc to interrupt`), or waiting at an input prompt. The rule set is
//! extensible with [`OutputHeuristics::with_rule`].

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use vca_core::{DecisionOption, SessionState};

/// Result of analyzing terminal output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeuristicMatch {
    pub state: SessionState,
    /// Name of the rule that matched.
    pub rule: String,
    /// The question, when one was found.
    pub prompt: Option<String>,
    /// Answers. Each option id is the text to type to choose it.
    pub options: Vec<DecisionOption>,
}

/// A custom rule: when `pattern` matches one of the last `window` lines,
/// the session is in `state`.
#[derive(Debug, Clone)]
pub struct HeuristicRule {
    pub name: String,
    pub pattern: Regex,
    pub state: SessionState,
    pub window: usize,
}

impl HeuristicRule {
    pub fn new(
        name: impl Into<String>,
        pattern: &str,
        state: SessionState,
    ) -> Result<Self, regex::Error> {
        Ok(Self {
            name: name.into(),
            pattern: Regex::new(pattern)?,
            state,
            window: 3,
        })
    }
}

/// An ordered rule set. Custom rules run first, then the built-in ones in
/// the order decision, running, input.
#[derive(Debug, Clone)]
pub struct OutputHeuristics {
    custom: Vec<HeuristicRule>,
    /// Number of trailing non-empty lines considered.
    pub window: usize,
}

impl Default for OutputHeuristics {
    fn default() -> Self {
        Self {
            custom: Vec::new(),
            window: 30,
        }
    }
}

static DEFAULT: LazyLock<OutputHeuristics> = LazyLock::new(OutputHeuristics::default);

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).expect("valid regex"));
    };
}

re!(
    ANSI,
    r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]"
);
re!(
    MENU_ITEM,
    r"^(?:[❯›>▶►●◉*→]\s*)?(\d{1,2})[.)]\s+(\S.*?)\s*$"
);
re!(
    YES_NO,
    r"(?i)[\[(]\s*(y(?:es)?)\s*/\s*(n(?:o)?)\s*[\])]|\b(yes)\s*/\s*(no)\b"
);
re!(LETTER_OPTION, r"\(([A-Za-z])\)([A-Za-z'][A-Za-z' ]*)");
re!(
    RUNNING,
    r"(?i)\b(?:esc|escape|ctrl\+c)\s+to\s+(?:interrupt|cancel|stop)\b|\bworking\s*\(\d|\bthinking…"
);
re!(INPUT_PROMPT, r"^(?:[>❯›]\s*|[\w\-./ ]{0,40}>\s*)$");
re!(
    INPUT_TEXT,
    r"(?i)waiting for (?:your )?input|press enter to continue|type your message|what would you like"
);

impl OutputHeuristics {
    /// The built-in rule set.
    pub fn default_set() -> &'static Self {
        &DEFAULT
    }

    /// Adds a custom rule, checked before the built-in ones.
    pub fn with_rule(mut self, rule: HeuristicRule) -> Self {
        self.custom.push(rule);
        self
    }

    pub fn analyze(&self, output: &str) -> Option<HeuristicMatch> {
        let clean = strip_ansi(output);
        let lines: Vec<String> = clean
            .lines()
            .map(clean_line)
            .filter(|l| !l.is_empty())
            .collect();
        let start = lines.len().saturating_sub(self.window);
        let lines = &lines[start..];
        if lines.is_empty() {
            return None;
        }

        for rule in &self.custom {
            let from = lines.len().saturating_sub(rule.window);
            if let Some(line) = lines[from..]
                .iter()
                .rev()
                .find(|l| rule.pattern.is_match(l))
            {
                return Some(HeuristicMatch {
                    state: rule.state.clone(),
                    rule: rule.name.clone(),
                    prompt: Some(line.clone()),
                    options: Vec::new(),
                });
            }
        }
        numbered_menu(lines)
            .or_else(|| yes_no(lines))
            .or_else(|| letter_options(lines))
            .or_else(|| simple(lines, &RUNNING, 4, SessionState::Running, "running"))
            .or_else(|| input_prompt(lines))
    }
}

/// Removes ANSI escape sequences and carriage-return overwrites.
pub fn strip_ansi(s: &str) -> String {
    let no_esc = ANSI.replace_all(s, "");
    no_esc
        .split('\n')
        .map(|line| line.rsplit('\r').find(|seg| !seg.is_empty()).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Trims whitespace and box-drawing borders.
fn clean_line(line: &str) -> String {
    const BORDER: &[char] = &[
        '│', '┃', '║', '|', '╭', '╮', '╰', '╯', '─', '━', '═', '┌', '┐', '└', '┘',
    ];
    line.trim()
        .trim_matches(|c: char| BORDER.contains(&c) || c.is_whitespace())
        .to_owned()
}

fn option(id: impl Into<String>, label: impl Into<String>) -> DecisionOption {
    DecisionOption {
        id: id.into(),
        label: label.into(),
    }
}

fn decision(rule: &str, prompt: Option<String>, options: Vec<DecisionOption>) -> HeuristicMatch {
    HeuristicMatch {
        state: SessionState::AwaitingDecision,
        rule: rule.into(),
        prompt,
        options,
    }
}

/// A block of `1. ...`, `2. ...` lines near the bottom, preceded by a
/// question. Used by Claude Code, Codex, Gemini CLI and others for
/// permission dialogs.
fn numbered_menu(lines: &[String]) -> Option<HeuristicMatch> {
    // The menu may be followed by a few footer lines ("Esc to cancel").
    let search_from = lines.len().saturating_sub(10);
    let last_item = (search_from..lines.len())
        .rev()
        .find(|&i| MENU_ITEM.is_match(&lines[i]))?;
    let mut items: Vec<(u32, String)> = Vec::new();
    let mut first = last_item;
    for i in (0..=last_item).rev() {
        match MENU_ITEM.captures(&lines[i]) {
            Some(c) => {
                items.push((c[1].parse().ok()?, c[2].to_owned()));
                first = i;
            }
            None => break,
        }
    }
    items.reverse();
    let sequential = items
        .iter()
        .enumerate()
        .all(|(i, (n, _))| *n as usize == i + 1);
    if items.len() < 2 || !sequential {
        return None;
    }
    let question = lines[first.saturating_sub(6)..first]
        .iter()
        .rev()
        .find(|l| l.trim_end().ends_with('?'))?;
    Some(decision(
        "numbered_menu",
        Some(question.clone()),
        items
            .into_iter()
            .map(|(n, label)| option(n.to_string(), label))
            .collect(),
    ))
}

fn yes_no(lines: &[String]) -> Option<HeuristicMatch> {
    let line = lines.iter().rev().take(3).find(|l| YES_NO.is_match(l))?;
    Some(decision(
        "yes_no",
        Some(line.clone()),
        vec![option("y", "Yes"), option("n", "No")],
    ))
}

/// aider style: `Add file to the chat? (Y)es/(N)o/(A)ll/(S)kip all [Yes]:`
fn letter_options(lines: &[String]) -> Option<HeuristicMatch> {
    let line = lines.iter().rev().take(3).find(|l| {
        let n = LETTER_OPTION.captures_iter(l).count();
        n >= 2
    })?;
    let options = LETTER_OPTION
        .captures_iter(line)
        .map(|c| {
            let letter = &c[1];
            option(
                letter.to_lowercase(),
                format!("{letter}{}", c[2].trim_end()),
            )
        })
        .collect();
    Some(decision("letter_options", Some(line.clone()), options))
}

fn simple(
    lines: &[String],
    re: &Regex,
    window: usize,
    state: SessionState,
    rule: &str,
) -> Option<HeuristicMatch> {
    let from = lines.len().saturating_sub(window);
    lines[from..]
        .iter()
        .rev()
        .find(|l| re.is_match(l))
        .map(|l| HeuristicMatch {
            state: state.clone(),
            rule: rule.into(),
            prompt: Some(l.clone()),
            options: Vec::new(),
        })
}

fn input_prompt(lines: &[String]) -> Option<HeuristicMatch> {
    simple(
        lines,
        &INPUT_TEXT,
        4,
        SessionState::AwaitingInput,
        "input_text",
    )
    .or_else(|| {
        // The prompt box is usually followed by one or two status lines.
        simple(
            lines,
            &INPUT_PROMPT,
            4,
            SessionState::AwaitingInput,
            "input_prompt",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(s: &str) -> Option<HeuristicMatch> {
        OutputHeuristics::default_set().analyze(s)
    }

    #[test]
    fn claude_permission_dialog() {
        let screen = "\x1b[1m╭──────────────────────────────╮\x1b[0m\n\
            │ Bash command                 │\n\
            │   npm install left-pad       │\n\
            │ Do you want to proceed?      │\n\
            │ ❯ 1. Yes                     │\n\
            │   2. Yes, and don't ask again for npm install commands │\n\
            │   3. No, and tell Claude what to do differently (esc) │\n\
            ╰──────────────────────────────╯\n";
        let m = analyze(screen).unwrap();
        assert_eq!(m.state, SessionState::AwaitingDecision);
        assert_eq!(m.rule, "numbered_menu");
        assert_eq!(m.prompt.as_deref(), Some("Do you want to proceed?"));
        let ids: Vec<&str> = m.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3"]);
        assert_eq!(m.options[0].label, "Yes");
    }

    #[test]
    fn numbered_list_without_question_is_not_a_menu() {
        let out = "Here is the plan:\n1. Add tests\n2. Fix bug\n";
        assert!(analyze(out).is_none_or(|m| m.state != SessionState::AwaitingDecision));
    }

    #[test]
    fn yes_no_prompt() {
        let m = analyze("Overwrite existing file? [y/N] ").unwrap();
        assert_eq!(m.state, SessionState::AwaitingDecision);
        assert_eq!(m.options.len(), 2);
        assert_eq!(m.options[0].id, "y");
    }

    #[test]
    fn aider_letter_options() {
        let m = analyze(
            "Add src/main.rs to the chat? (Y)es/(N)o/(A)ll/(S)kip all/(D)on't ask again [Yes]: ",
        )
        .unwrap();
        assert_eq!(m.rule, "letter_options");
        let ids: Vec<&str> = m.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["y", "n", "a", "s", "d"]);
        assert_eq!(m.options[4].label, "Don't ask again");
    }

    #[test]
    fn running_beats_input_box() {
        let screen =
            "✻ Compiling… (12s · esc to interrupt)\n╭────╮\n│ >  │\n╰────╯\n  ? for shortcuts\n";
        assert_eq!(analyze(screen).unwrap().state, SessionState::Running);
    }

    #[test]
    fn idle_input_box() {
        let screen =
            "Done. All 42 tests pass.\n╭──────────────╮\n│ >            │\n╰──────────────╯\n";
        let m = analyze(screen).unwrap();
        assert_eq!(m.state, SessionState::AwaitingInput);
        let aider = analyze("Tokens: 2.1k sent\nsrc/main.rs> ").unwrap();
        assert_eq!(aider.state, SessionState::AwaitingInput);
    }

    #[test]
    fn custom_rules_run_first() {
        let h = OutputHeuristics::default().with_rule(
            HeuristicRule::new(
                "rate_limit",
                r"(?i)usage limit reached",
                SessionState::Paused,
            )
            .unwrap(),
        );
        let m = h
            .analyze("Usage limit reached. Resets at 5pm.\n> ")
            .unwrap();
        assert_eq!(
            (m.state, m.rule.as_str()),
            (SessionState::Paused, "rate_limit")
        );
    }

    #[test]
    fn strips_ansi_and_carriage_returns() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("10%\r50%\r100%"), "100%");
        assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
    }

    #[test]
    fn empty_output() {
        assert!(analyze("").is_none());
        assert!(analyze("compiling crate foo\n").is_none());
    }
}
