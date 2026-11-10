//! Finding requests for the person in an agent's final message, with fixed
//! patterns (English, Korean, Japanese, Spanish). Read-only: nothing found
//! here is ever run.
//!
//! A request is a line that asks the person to do something ("please run",
//! "실행해 주세요", "実行してください", "por favor ejecuta", "as
//! administrator", ...). A fenced code block right after it becomes the
//! task's command; list items after a line ending in a colon become its
//! steps; URLs in it are kept. Closing lines ("let me know if ...",
//! "궁금한 점이 있으면 알려 주세요") and offers ("would you like me to
//! ...?") are not requests.

use vca_core::task::{TASK_MAX_COMMAND_CHARS, TASK_MAX_STEPS_CHARS, TaskShell};

use crate::util::truncate_chars;

/// Most tasks taken from one message.
pub const MAX_PER_MESSAGE: usize = 5;
/// Longest extracted title, in characters.
pub const TITLE_CHARS: usize = 120;

/// A request found in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub title: String,
    pub steps: String,
    pub command: Option<String>,
    pub shell: TaskShell,
    pub needs_admin: bool,
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Text(String),
    Code { lang: String, body: String },
}

/// Splits a message into text lines and fenced code blocks.
fn segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let fence = if trimmed.starts_with("```") {
            "```"
        } else if trimmed.starts_with("~~~") {
            "~~~"
        } else {
            out.push(Segment::Text(line.to_owned()));
            continue;
        };
        let lang = trimmed[fence.len()..].trim().to_ascii_lowercase();
        let mut body = Vec::new();
        for inner in lines.by_ref() {
            if inner.trim_start().starts_with(fence) {
                break;
            }
            body.push(inner);
        }
        out.push(Segment::Code {
            lang,
            body: body.join("\n").trim_matches('\n').to_owned(),
        });
    }
    out
}

/// Phrases that ask the person to do something.
const REQUEST: &[&str] = &[
    // English
    "please ",
    "please,",
    "you need to",
    "you'll need to",
    "you will need to",
    "you must ",
    "you have to",
    "you'll have to",
    "i need you to",
    "could you ",
    "can you ",
    "action required",
    "as administrator",
    "as an administrator",
    "as admin",
    "with administrator",
    "in an elevated",
    "in an administrator",
    "in an admin",
    "must be run as",
    "needs to be run as",
    "uac prompt",
    "manual step",
    // Korean
    "주세요",
    "주십시오",
    "주시겠어요",
    "주실 수",
    "부탁드립니다",
    "부탁드려요",
    "부탁해요",
    "하셔야 합니다",
    "하셔야 해요",
    "관리자 권한으로",
    "관리자 powershell",
    "관리자로 실행",
    // Japanese
    "ください",
    "お願いします",
    "お願いいたします",
    "もらえますか",
    "いただけますか",
    "管理者として",
    "管理者権限で",
    // Spanish
    "por favor",
    "necesitas ",
    "tienes que",
    "debes ",
    "deberás",
    "como administrador",
    "inicia sesión",
];

/// Closing lines, offers and notes: never requests.
const NOT_REQUEST: &[&str] = &[
    "let me know",
    "feel free",
    "any questions",
    "if you'd like",
    "if you would like",
    "if you want",
    "would you like",
    "do you want",
    "shall i",
    "should i",
    "please note",
    "note that",
    "when you're done",
    "once you're done",
    "once done",
    "tell me when",
    "궁금",
    "원하시면",
    "원하시는",
    "필요하시면",
    "필요하면",
    "말씀해",
    "할까요",
    "드릴까요",
    "참고로",
    "참고해",
    "되면 알려",
    "끝나면 알려",
    "완료되면",
    "ご質問",
    "お気軽",
    "必要があれば",
    "しましょうか",
    "終わったら",
    "完了したら",
    "お知らせください",
    "no dudes",
    "si tienes",
    "si quieres",
    "si necesitas",
    "avísame",
    "quieres que",
    "cuando termines",
];

/// The person is asked to run something.
const RUN_VERBS: &[&str] = &[
    "run",
    "execute",
    "paste",
    "type",
    "실행",
    "입력",
    "붙여",
    "実行",
    "入力",
    "貼り付け",
    "ejecuta",
    "ejecute",
    "escribe",
    "pega",
];

/// Administrator rights.
const ADMIN: &[&str] = &[
    "administrator",
    "admin ",
    "admin powershell",
    "elevated",
    " uac",
    "uac ",
    "sudo ",
    "run as admin",
    "runas",
    "관리자",
    "管理者",
    "administrador",
];

/// The agent cannot go on without the person.
const BLOCKING: &[&str] = &[
    "before i can",
    "so i can",
    "so that i can",
    "then i can",
    "to continue",
    "to proceed",
    "i'm blocked",
    "i am blocked",
    "waiting for you",
    "can't continue",
    "cannot continue",
    "계속 진행",
    "진행하려면",
    "진행할 수",
    "계속하려면",
    "기다리겠습니다",
    "기다리고 있",
    "続行",
    "進めるには",
    "待っています",
    "para continuar",
    "para seguir",
    "estoy bloqueado",
];

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Asks the person to do something (and is not a closer or an offer).
pub fn is_request(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    if lower.is_empty() || contains_any(&lower, NOT_REQUEST) {
        return false;
    }
    contains_any(&lower, REQUEST)
}

fn is_list_item(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

/// The text of a line without Markdown decoration.
fn clean_line(line: &str) -> String {
    let mut t = line.trim();
    t = t.trim_start_matches('#').trim_start();
    if is_list_item(t) {
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        t = t[digits.max(1)..]
            .trim_start_matches(['.', ')'])
            .trim_start();
    }
    let t = t.replace("**", "").replace("__", "");
    let t = t.trim().trim_end_matches([':', '：']).trim();
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `inline code` span of a line.
fn inline_code(line: &str) -> Option<String> {
    let start = line.find('`')?;
    let rest = &line[start + 1..];
    let end = rest.find('`')?;
    let code = rest[..end].trim();
    (!code.is_empty()).then(|| code.to_owned())
}

/// `http(s)://` links in a line.
fn urls(line: &str) -> Vec<String> {
    line.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | '"' | '`'))
        .filter(|w| w.starts_with("https://") || w.starts_with("http://"))
        .map(|w| {
            w.trim_end_matches(['.', ',', ';', ':', '!', '?', ']'])
                .to_owned()
        })
        .filter(|w| w.len() > "https://".len())
        .collect()
}

/// The shell of a fence language, else a guess from the text around it.
pub fn shell_for(lang: &str, context: &str, command: &str) -> TaskShell {
    match lang {
        "powershell" | "ps" | "ps1" | "pwsh" | "posh" => return TaskShell::Powershell,
        "cmd" | "bat" | "batch" | "dos" => return TaskShell::Cmd,
        "bash" | "shell" | "console" | "terminal" => return TaskShell::Bash,
        "zsh" => return TaskShell::Zsh,
        "sh" => return TaskShell::Sh,
        _ => {}
    }
    let ctx = context.to_lowercase();
    if ctx.contains("powershell") {
        TaskShell::Powershell
    } else if ctx.contains("cmd") || ctx.contains("command prompt") || ctx.contains("명령 프롬프트")
    {
        TaskShell::Cmd
    } else if command.trim_start().starts_with("sudo ") || ctx.contains("terminal") {
        TaskShell::Bash
    } else if command.contains("Set-") || command.contains("Get-") || command.contains("$env:") {
        TaskShell::Powershell
    } else {
        TaskShell::Other
    }
}

/// The requests in an agent's message, at most [`MAX_PER_MESSAGE`], without
/// duplicates.
pub fn extract(text: &str) -> Vec<Candidate> {
    let segs = segments(text);
    let mut out: Vec<Candidate> = Vec::new();
    let mut i = 0;
    while i < segs.len() && out.len() < MAX_PER_MESSAGE {
        let Segment::Text(line) = &segs[i] else {
            i += 1;
            continue;
        };
        if !is_request(line) {
            i += 1;
            continue;
        }
        let title = truncate_chars(&clean_line(line), TITLE_CHARS, false);
        if title.is_empty() {
            i += 1;
            continue;
        }
        let mut steps: Vec<String> = Vec::new();
        let mut command: Option<(String, String)> = None;
        let mut context = line.clone();
        let mut j = i + 1;
        while matches!(segs.get(j), Some(Segment::Text(t)) if t.trim().is_empty()) {
            j += 1;
        }
        let ends_with_colon = line.trim_end().ends_with([':', '：']);
        match segs.get(j) {
            Some(Segment::Code { lang, body }) if !body.trim().is_empty() => {
                command = Some((lang.clone(), body.clone()));
                i = j + 1;
            }
            Some(Segment::Text(next)) if ends_with_colon && is_list_item(next) => {
                let mut k = j;
                loop {
                    match segs.get(k) {
                        Some(Segment::Text(t)) if is_list_item(t) => {
                            steps.push(format!("- {}", clean_line(t)));
                            context.push('\n');
                            context.push_str(t);
                            k += 1;
                        }
                        Some(Segment::Text(t)) if t.trim().is_empty() => k += 1,
                        Some(Segment::Code { lang, body })
                            if command.is_none() && !body.trim().is_empty() =>
                        {
                            command = Some((lang.clone(), body.clone()));
                            k += 1;
                        }
                        _ => break,
                    }
                }
                i = k;
            }
            _ => i += 1,
        }
        // "Please run `npm login` ...": the inline command.
        if command.is_none() && contains_any(&line.to_lowercase(), RUN_VERBS) {
            if let Some(code) = inline_code(line) {
                command = Some((String::new(), code));
            }
        }
        for url in urls(&context) {
            let step = format!("- <{url}>");
            if !steps.contains(&step) {
                steps.push(step);
            }
        }
        let lower = context.to_lowercase();
        let next_line = match segs.get(i) {
            Some(Segment::Text(t)) => t.to_lowercase(),
            _ => String::new(),
        };
        let blocking = contains_any(&lower, BLOCKING) || contains_any(&next_line, BLOCKING);
        let (shell, command) = match command {
            Some((lang, body)) => (
                shell_for(&lang, &context, &body),
                Some(truncate_chars(body.trim(), TASK_MAX_COMMAND_CHARS, false)),
            ),
            None => (TaskShell::Other, None),
        };
        let needs_admin = contains_any(&format!("{lower} "), ADMIN)
            || command
                .as_deref()
                .is_some_and(|c| c.trim_start().starts_with("sudo "));
        let candidate = Candidate {
            title,
            steps: truncate_chars(&steps.join("\n"), TASK_MAX_STEPS_CHARS, false),
            command,
            shell,
            needs_admin,
            blocking,
        };
        let key = candidate_key(&candidate);
        if !out.iter().any(|c| candidate_key(c) == key) {
            out.push(candidate);
        }
    }
    out
}

/// What makes two requests the same: the command when there is one, else
/// the title, lowercased, letters and digits only.
pub fn candidate_key(c: &Candidate) -> String {
    normalize(c.command.as_deref().unwrap_or(&c.title))
}

/// Lowercase letters and digits, single spaces.
pub fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
