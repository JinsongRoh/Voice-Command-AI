//! Danger classification of shell command lines (remote shells, rule 7).
//!
//! The server is authoritative: the client only shows the result. The
//! classifier understands enough of bash, PowerShell and cmd syntax to find
//! the risky parts of a line: quoting, command lists and pipelines (`;`,
//! `&&`, `||`, `|`, `&`, newlines, `(...)`, `{...}`, `$(...)`), wrappers
//! (`sudo`, `env`, `nohup`, ...) and nested command strings (`bash -c`,
//! `cmd /c`, `powershell -Command`). Every match carries a stable reason code
//! and the UTF-8 byte spans of the words that triggered it.
//!
//! Reason codes (the level is the highest of all matches):
//!
//! | code | level | examples |
//! |------|-------|----------|
//! | `recursive_delete` | dangerous | `rm -rf`, `Remove-Item -Recurse`, `del /s`, `rd /s`, `find -delete` |
//! | `delete` | caution | `rm file`, `del file`, `Remove-Item file` |
//! | `disk_format` | dangerous | `format`, `diskpart`, `mkfs`, `dd of=`, `Format-Volume` |
//! | `permissions` | dangerous (recursive, ACL writes) / caution | `chmod -R`, `chown -R`, `icacls /grant`, `takeown`, `Set-Acl`, `chmod` |
//! | `registry_write` | dangerous (HKLM) / caution (HKCU) | `reg add`, `reg delete`, `Set-ItemProperty HKLM:` |
//! | `service_or_process_kill` | dangerous / caution | `Stop-Service`, `sc delete`, `taskkill /f`, `kill -9` / `kill`, `taskkill` |
//! | `shutdown` | dangerous | `shutdown`, `reboot`, `Restart-Computer` |
//! | `remote_code` | dangerous | `curl ... \| sh`, `iwr ... \| iex`, `Invoke-Expression` |
//! | `encoded_command` | dangerous | `powershell -EncodedCommand ...` |
//! | `secret_read` | dangerous | `.ssh`, `.aws`, `id_rsa`, `*.p8`, `.env` |
//! | `git_destructive` | dangerous / caution | `git push --force`, `git reset --hard`, `git clean -f` / `git branch -D` |
//! | `elevation` | dangerous | `sudo`, `runas`, `Start-Process -Verb RunAs` (remote shells never run elevated) |

use vca_core::DangerLevel;

/// The result of [`classify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub level: DangerLevel,
    /// Stable reason codes, in order of first appearance, without
    /// duplicates.
    pub reasons: Vec<&'static str>,
    /// Sorted, merged `[start, end)` UTF-8 byte offsets.
    pub spans: Vec<(usize, usize)>,
}

impl Classification {
    /// Spans as the protocol carries them.
    pub fn wire_spans(&self) -> Vec<[u32; 2]> {
        self.spans
            .iter()
            .map(|&(s, e)| [s as u32, e as u32])
            .collect()
    }
}

/// How deep `bash -c '...'` style nesting is followed.
const MAX_DEPTH: usize = 4;

/// Classifies one command line.
pub fn classify(command: &str) -> Classification {
    let mut out = Collector::default();
    classify_range(command, 0, command.len(), 0, false, &mut out);
    out.finish(command)
}

#[derive(Default)]
struct Collector {
    level: Option<DangerLevel>,
    reasons: Vec<&'static str>,
    spans: Vec<(usize, usize)>,
}

impl Collector {
    fn hit(&mut self, level: DangerLevel, reason: &'static str, spans: &[(usize, usize)]) {
        self.level = Some(self.level.map_or(level, |l| l.max(level)));
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
        self.spans
            .extend(spans.iter().copied().filter(|(s, e)| s < e));
    }

    /// Sorts the spans and merges those that overlap or are separated by
    /// whitespace only (`rm -rf` is one span).
    fn finish(mut self, src: &str) -> Classification {
        self.spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (s, e) in self.spans {
            match merged.last_mut() {
                Some(last)
                    if s <= last.1 || src[last.1..s].chars().all(|c| c == ' ' || c == '\t') =>
                {
                    last.1 = last.1.max(e)
                }
                _ => merged.push((s, e)),
            }
        }
        Classification {
            level: self.level.unwrap_or(DangerLevel::Safe),
            reasons: self.reasons,
            spans: merged,
        }
    }
}

/// Longest command that can be approved by voice: a person has to hear it
/// out and understand it.
const VOICE_MAX_CHARS: usize = 120;

/// Read-only programs, whatever their arguments.
const VOICE_PROGRAMS: &[&str] = &[
    "cat", "dir", "echo", "grep", "head", "ls", "pwd", "tail", "uname", "wc", "where", "which",
    "whoami",
];

/// Path pieces of credentials, beyond what [`classify`] calls `secret_read`.
const VOICE_CREDENTIALS: &[&str] = &[
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
    ".config/gh",
    "credentials",
    "secret",
    "token",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "shadow",
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".p8",
];

/// `git log` without patches: `status` and `diff` can run what the
/// repository's own config names (fsmonitor, external diff), and so can
/// `show` and `log -p` (textconv).
const VOICE_GIT_OPTIONS_REFUSED: &[&str] = &[
    "-p",
    "-u",
    "--patch",
    "--output",
    "--ext-diff",
    "--textconv",
];

/// May this command be approved by voice alone?
///
/// [`classify`] is a list of what it knows to be risky, so anything it does
/// not know is `safe`, which is fine for showing a warning and not for
/// running a command on a word heard through a microphone. This is the
/// opposite: one plain command from a short list of programs that only read.
/// Everything else goes to the screen.
pub fn voice_approvable(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty()
        || command.chars().count() > VOICE_MAX_CHARS
        || classify(command).level != DangerLevel::Safe
    {
        return false;
    }
    // No chaining, redirection, substitution, expansion or quoting: the
    // words below are then exactly what runs.
    const SYNTAX: &[char] = &[
        ';', '&', '|', '<', '>', '`', '$', '(', ')', '{', '}', '[', ']', '*', '?', '~', '!', '#',
        '%', '^', '=', '\\', '\'', '"', '\n', '\r',
    ];
    if command.contains(SYNTAX) {
        return false;
    }
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    if program == "git" {
        return words.next() == Some("log")
            && !words.any(|w| VOICE_GIT_OPTIONS_REFUSED.iter().any(|o| w.starts_with(o)));
    }
    let lower = command.to_lowercase().replace('\\', "/");
    VOICE_PROGRAMS.contains(&program)
        && !VOICE_CREDENTIALS.iter().any(|c| lower.contains(c))
        // A recursive search reads whatever is below.
        && !(program == "grep" && words.any(|w| w.starts_with('-') && w.contains(['r', 'R'])))
}

/// One word of a command, with its raw byte range in the source.
#[derive(Debug, Clone)]
struct Word {
    /// The word without quotes.
    text: String,
    start: usize,
    end: usize,
}

impl Word {
    fn lower(&self) -> String {
        self.text.to_lowercase()
    }

    fn span(&self) -> (usize, usize) {
        (self.start, self.end)
    }
}

#[derive(Debug, Default)]
struct Segment {
    words: Vec<Word>,
    /// Reads the previous segment's output (`a | b`).
    piped: bool,
    /// Index of the pipeline's first segment.
    pipeline: usize,
    /// For `$(...)` / `<(...)`: the segment whose argument this is.
    substituted_into: Option<usize>,
}

/// Splits `src[from..to]` into segments of words.
fn tokenize(src: &str, from: usize, to: usize) -> Vec<Segment> {
    let mut segments: Vec<Segment> = vec![Segment::default()];
    let mut word: Option<Word> = None;
    let mut quote: Option<char> = None;
    let mut chars = src[from..to].char_indices().peekable();

    fn end_word(segments: &mut [Segment], word: &mut Option<Word>) {
        if let Some(w) = word.take() {
            segments.last_mut().expect("one segment").words.push(w);
        }
    }

    fn new_segment(segments: &mut Vec<Segment>, piped: bool, substituted_into: Option<usize>) {
        let index = segments.len();
        let pipeline = if piped {
            segments.last().map_or(index, |s| s.pipeline)
        } else {
            index
        };
        segments.push(Segment {
            words: Vec::new(),
            piped,
            pipeline,
            substituted_into,
        });
    }

    while let Some((i, c)) = chars.next() {
        let at = from + i;
        let next_at = at + c.len_utf8();
        if let Some(q) = quote {
            let w = word.as_mut().expect("a quote is inside a word");
            w.end = next_at;
            if c == q {
                quote = None;
            } else {
                w.text.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                let w = word.get_or_insert_with(|| Word {
                    text: String::new(),
                    start: at,
                    end: at,
                });
                w.end = next_at;
                quote = Some(c);
            }
            ' ' | '\t' | '\r' => end_word(&mut segments, &mut word),
            '\n' | ';' | ')' | '{' | '}' => {
                end_word(&mut segments, &mut word);
                new_segment(&mut segments, false, None);
            }
            '(' => {
                // `$(` and `<(` feed a command's output into the current
                // command; a bare `(` starts a group.
                let substitution = word
                    .as_ref()
                    .is_some_and(|w| w.text.ends_with('$') || w.text.ends_with('<'));
                end_word(&mut segments, &mut word);
                let parent = substitution.then(|| segments.len() - 1);
                new_segment(&mut segments, false, parent);
            }
            '|' => {
                end_word(&mut segments, &mut word);
                if chars.peek().is_some_and(|&(_, n)| n == '|') {
                    chars.next();
                    new_segment(&mut segments, false, None);
                } else {
                    new_segment(&mut segments, true, None);
                }
            }
            '&' => {
                end_word(&mut segments, &mut word);
                if chars.peek().is_some_and(|&(_, n)| n == '&') {
                    chars.next();
                }
                new_segment(&mut segments, false, None);
            }
            _ => {
                let w = word.get_or_insert_with(|| Word {
                    text: String::new(),
                    start: at,
                    end: at,
                });
                w.text.push(c);
                w.end = next_at;
            }
        }
    }
    end_word(&mut segments, &mut word);
    segments
}

/// The program name of a word: file name, lower case, without `.exe` and
/// similar suffixes.
fn program_name(word: &Word) -> String {
    let lower = word.lower();
    let file = lower
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&lower)
        .to_owned();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = file.strip_suffix(ext) {
            return stem.to_owned();
        }
    }
    file
}

const ELEVATION: &[&str] = &["sudo", "doas", "runas", "gsudo", "su", "pkexec"];
const WRAPPERS: &[&str] = &[
    "env", "nohup", "time", "nice", "ionice", "timeout", "xargs", "command", "exec", "builtin",
    "stdbuf", "call",
];
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish", "busybox"];
const POWERSHELLS: &[&str] = &["powershell", "pwsh"];
const INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "python",
    "python3",
    "perl",
    "ruby",
    "node",
    "php",
    "iex",
    "invoke-expression",
    "powershell",
    "pwsh",
    "cmd",
    "source",
    ".",
];
const DOWNLOADERS: &[&str] = &[
    "curl",
    "wget",
    "iwr",
    "invoke-webrequest",
    "irm",
    "invoke-restmethod",
    "fetch",
    "certutil",
];
const DELETERS: &[&str] = &[
    "rm",
    "rmdir",
    "rd",
    "del",
    "erase",
    "remove-item",
    "ri",
    "unlink",
];
const REGISTRY_CMDLETS: &[&str] = &[
    "set-itemproperty",
    "new-itemproperty",
    "remove-itemproperty",
    "rename-itemproperty",
    "clear-itemproperty",
    "copy-itemproperty",
    "move-itemproperty",
    "set-item",
    "new-item",
    "remove-item",
    "ri",
    "sp",
    "rp",
    "ni",
    "si",
];

/// A short flag cluster such as `-rf` (bash style).
fn short_flags(lower_or_raw: &str) -> Option<&str> {
    let rest = lower_or_raw.strip_prefix('-')?;
    if rest.is_empty()
        || rest.starts_with('-')
        || rest.len() > 6
        || !rest.bytes().all(|b| b.is_ascii_alphabetic())
    {
        return None;
    }
    Some(rest)
}

/// `-Force`, `-fo`: a PowerShell parameter given by any prefix of at least
/// `min` characters.
fn ps_param(lower: &str, name: &str, min: usize) -> bool {
    lower
        .strip_prefix('-')
        .is_some_and(|p| p.len() >= min && name.starts_with(p))
}

fn classify_range(
    src: &str,
    from: usize,
    to: usize,
    depth: usize,
    in_interpreter: bool,
    out: &mut Collector,
) {
    let segments = tokenize(src, from, to);
    // Pipelines that start with a download.
    let mut downloading: Vec<usize> = Vec::new();
    for (index, seg) in segments.iter().enumerate() {
        let Some(cmd) = command_words(&seg.words, out) else {
            continue;
        };
        let program = program_name(cmd[0]);
        if DOWNLOADERS.contains(&program.as_str()) {
            downloading.push(seg.pipeline);
            let into = seg
                .substituted_into
                .and_then(|p| segments.get(p))
                .and_then(|p| command_words(&p.words, &mut Collector::default()))
                .map(|w| program_name(w[0]));
            let executed = into.as_deref().is_some_and(|p| INTERPRETERS.contains(&p))
                || (in_interpreter && seg.substituted_into.is_some());
            if executed {
                out.hit(DangerLevel::Dangerous, "remote_code", &[cmd[0].span()]);
            }
        }
        if seg.piped
            && INTERPRETERS.contains(&program.as_str())
            && downloading.contains(&seg.pipeline)
        {
            let downloader = segments[seg.pipeline..index]
                .iter()
                .filter_map(|s| s.words.first())
                .map(Word::span)
                .collect::<Vec<_>>();
            out.hit(DangerLevel::Dangerous, "remote_code", &[cmd[0].span()]);
            out.hit(DangerLevel::Dangerous, "remote_code", &downloader);
        }
        classify_command(src, &program, &cmd, depth, out);
    }
}

/// Skips environment assignments, wrappers and elevation prefixes (which
/// are reported) and returns the words of the actual command.
fn command_words<'a>(words: &'a [Word], out: &mut Collector) -> Option<Vec<&'a Word>> {
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        let name = program_name(w);
        let is_assignment = w.text.contains('=')
            && !w.text.starts_with('=')
            && w.text
                .split('=')
                .next()
                .is_some_and(|k| k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        if is_assignment || w.text == "$" || w.text == "<" || w.text == "@" {
            i += 1;
            continue;
        }
        if ELEVATION.contains(&name.as_str()) {
            out.hit(DangerLevel::Dangerous, "elevation", &[w.span()]);
            i += 1;
            // Options of the elevation tool (`-u root`, `/user:x`).
            while i < words.len() {
                let t = &words[i].text;
                if t.starts_with('-') || (t.starts_with('/') && t.contains(':')) {
                    if matches!(t.as_str(), "-u" | "-g" | "-c") {
                        i += 1;
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        if WRAPPERS.contains(&name.as_str()) {
            i += 1;
            while i < words.len() && words[i].text.starts_with('-') {
                if matches!(words[i].text.as_str(), "-n" | "-s" | "-k") {
                    i += 1;
                }
                i += 1;
            }
            // `timeout 10 cmd`, `nice 5 cmd`
            if i < words.len()
                && matches!(name.as_str(), "timeout" | "nice")
                && words[i]
                    .text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b == b'.')
            {
                i += 1;
            }
            continue;
        }
        break;
    }
    (i < words.len()).then(|| words[i..].iter().collect())
}

/// Range of the command string after a `-c`-style flag: the rest of the
/// segment, without the quotes when it is a single quoted word.
fn rest_range(src: &str, words: &[&Word]) -> Option<(usize, usize)> {
    let first = words.first()?;
    let last = words.last()?;
    let (mut start, mut end) = (first.start, last.end);
    if words.len() == 1 {
        let raw = &src[start..end];
        let quoted = raw.len() >= 2
            && (raw.starts_with('"') && raw.ends_with('"')
                || raw.starts_with('\'') && raw.ends_with('\''));
        if quoted {
            start += 1;
            end -= 1;
        }
    }
    Some((start, end))
}

fn classify_command(src: &str, program: &str, cmd: &[&Word], depth: usize, out: &mut Collector) {
    use DangerLevel::{Caution, Dangerous};
    let args = &cmd[1..];
    let lower: Vec<String> = args.iter().map(|w| w.lower()).collect();
    let prog_span = cmd[0].span();

    // Nested command strings.
    if depth < MAX_DEPTH {
        let nested = if SHELLS.contains(&program) {
            lower
                .iter()
                .position(|a| short_flags(a).is_some_and(|f| f.contains('c')))
        } else if POWERSHELLS.contains(&program) {
            lower
                .iter()
                .position(|a| ps_param(a, "command", 1) && a != "-")
        } else if program == "cmd" {
            lower.iter().position(|a| a == "/c" || a == "/k")
        } else {
            None
        };
        if let Some(pos) = nested
            && let Some((s, e)) = rest_range(src, &args[pos + 1..])
        {
            classify_range(src, s, e, depth + 1, true, out);
        }
    }

    // Secrets: any argument that names a credential file.
    for w in args {
        if names_secret(&w.lower()) {
            out.hit(Dangerous, "secret_read", &[w.span()]);
        }
    }

    // Deleting.
    if DELETERS.contains(&program) {
        let mut recursive = Vec::new();
        let mut force = Vec::new();
        for (w, l) in args.iter().zip(&lower) {
            if l == "--recursive" || l.starts_with("-rec") && "-recurse".starts_with(l.as_str()) {
                recursive.push(w.span());
            } else if l == "--force" || ps_param(l, "force", 2) {
                force.push(w.span());
            } else if l == "/s" {
                recursive.push(w.span());
            } else if l == "/q" || l == "/f" {
                force.push(w.span());
            } else if let Some(flags) = short_flags(&w.text) {
                if flags.contains(['r', 'R']) {
                    recursive.push(w.span());
                } else if flags.contains('f') {
                    force.push(w.span());
                }
            }
        }
        let mut spans = vec![prog_span];
        if recursive.is_empty() {
            spans.extend(force);
            out.hit(Caution, "delete", &spans);
        } else {
            spans.extend(recursive);
            spans.extend(force);
            out.hit(Dangerous, "recursive_delete", &spans);
        }
    }
    if program == "shred" || program == "srm" {
        out.hit(Dangerous, "recursive_delete", &[prog_span]);
    }
    if program == "find"
        && let Some(i) = lower
            .iter()
            .position(|a| a == "-delete" || a == "-exec" || a == "-execdir")
        && (lower[i] == "-delete"
            || lower
                .get(i + 1)
                .is_some_and(|p| matches!(p.as_str(), "rm" | "shred" | "unlink")))
    {
        out.hit(Dangerous, "recursive_delete", &[prog_span, args[i].span()]);
    }

    // Disks.
    let disk_tools = [
        "format",
        "format-volume",
        "clear-disk",
        "initialize-disk",
        "diskpart",
        "fdisk",
        "sfdisk",
        "gdisk",
        "parted",
        "wipefs",
    ];
    if disk_tools.contains(&program) || program.starts_with("mkfs") {
        out.hit(Dangerous, "disk_format", &[prog_span]);
    }
    if program == "diskutil"
        && lower
            .first()
            .is_some_and(|a| a.starts_with("erase") || a.starts_with("partition"))
    {
        out.hit(Dangerous, "disk_format", &[prog_span, args[0].span()]);
    }
    if program == "dd"
        && let Some(i) = lower.iter().position(|a| a.starts_with("of="))
    {
        out.hit(Dangerous, "disk_format", &[prog_span, args[i].span()]);
    }

    // Permissions and ownership.
    if matches!(program, "chmod" | "chown" | "chgrp") {
        let recursive: Vec<_> = args
            .iter()
            .zip(&lower)
            .filter(|(w, l)| {
                l.as_str() == "--recursive" || short_flags(&w.text).is_some_and(|f| f.contains('R'))
            })
            .map(|(w, _)| w.span())
            .collect();
        if recursive.is_empty() {
            out.hit(Caution, "permissions", &[prog_span]);
        } else {
            let mut spans = vec![prog_span];
            spans.extend(recursive);
            out.hit(Dangerous, "permissions", &spans);
        }
    }
    if program == "icacls" || program == "cacls" {
        let writes = [
            "/grant",
            "/deny",
            "/remove",
            "/reset",
            "/setowner",
            "/inheritance",
            "/restore",
            "/setintegritylevel",
            "/g",
            "/r",
            "/p",
            "/d",
            "/e",
        ];
        let hits: Vec<_> = args
            .iter()
            .zip(&lower)
            .filter(|(_, l)| writes.contains(&l.split(':').next().unwrap_or(l)))
            .map(|(w, _)| w.span())
            .collect();
        if !hits.is_empty() {
            let mut spans = vec![prog_span];
            spans.extend(hits);
            out.hit(Dangerous, "permissions", &spans);
        }
    }
    if program == "takeown" || program == "set-acl" {
        out.hit(Dangerous, "permissions", &[prog_span]);
    }

    // Registry.
    if program == "reg"
        && lower.first().is_some_and(|a| {
            matches!(
                a.as_str(),
                "add" | "delete" | "import" | "restore" | "load" | "unload" | "copy"
            )
        })
    {
        out.hit(Dangerous, "registry_write", &[prog_span, args[0].span()]);
    }
    if program == "regedit" {
        let silent = lower.iter().any(|a| a == "/s");
        out.hit(
            if silent { Dangerous } else { Caution },
            "registry_write",
            &[prog_span],
        );
    }
    if REGISTRY_CMDLETS.contains(&program) {
        for (w, l) in args.iter().zip(&lower) {
            let machine = l.starts_with("hklm:")
                || l.contains("hkey_local_machine")
                || l.starts_with("registry::hklm");
            let user = l.starts_with("hkcu:")
                || l.contains("hkey_current_user")
                || l.starts_with("registry::hkcu");
            if machine {
                out.hit(Dangerous, "registry_write", &[prog_span, w.span()]);
            } else if user {
                out.hit(Caution, "registry_write", &[prog_span, w.span()]);
            }
        }
    }

    // Services and processes.
    let kill_signal = |l: &String| {
        matches!(
            l.as_str(),
            "-9" | "-kill" | "-sigkill" | "--signal=kill" | "--signal=9" | "-s9"
        )
    };
    match program {
        "stop-service" | "remove-service" => {
            out.hit(Dangerous, "service_or_process_kill", &[prog_span]);
        }
        "set-service" => out.hit(Caution, "service_or_process_kill", &[prog_span]),
        "sc" if lower
            .first()
            .is_some_and(|a| matches!(a.as_str(), "stop" | "delete" | "config" | "failure")) =>
        {
            out.hit(
                Dangerous,
                "service_or_process_kill",
                &[prog_span, args[0].span()],
            );
        }
        "net" if lower.first().is_some_and(|a| a == "stop") => {
            out.hit(
                Dangerous,
                "service_or_process_kill",
                &[prog_span, args[0].span()],
            );
        }
        "service" if lower.iter().any(|a| a == "stop") => {
            out.hit(Dangerous, "service_or_process_kill", &[prog_span]);
        }
        "systemctl" | "launchctl" => {
            let stop = [
                "stop", "disable", "mask", "kill", "unload", "bootout", "remove",
            ];
            let power = ["poweroff", "reboot", "halt", "kexec"];
            if let Some(first) = lower.iter().position(|a| !a.starts_with('-')) {
                if stop.contains(&lower[first].as_str()) {
                    out.hit(
                        Dangerous,
                        "service_or_process_kill",
                        &[prog_span, args[first].span()],
                    );
                } else if power.contains(&lower[first].as_str()) {
                    out.hit(Dangerous, "shutdown", &[prog_span, args[first].span()]);
                }
            }
        }
        "taskkill" => {
            let forced: Vec<_> = args
                .iter()
                .zip(&lower)
                .filter(|(_, l)| l.as_str() == "/f")
                .map(|(w, _)| w.span())
                .collect();
            if forced.is_empty() {
                out.hit(Caution, "service_or_process_kill", &[prog_span]);
            } else {
                let mut spans = vec![prog_span];
                spans.extend(forced);
                out.hit(Dangerous, "service_or_process_kill", &spans);
            }
        }
        "kill" | "pkill" | "killall" | "stop-process" | "spps" => {
            let mut strong: Vec<_> = args
                .iter()
                .zip(&lower)
                .filter(|(_, l)| kill_signal(l) || ps_param(l, "force", 2))
                .map(|(w, _)| w.span())
                .collect();
            // `kill -s KILL`
            if let Some(i) = lower.iter().position(|a| a == "-s")
                && lower
                    .get(i + 1)
                    .is_some_and(|s| matches!(s.as_str(), "kill" | "9" | "sigkill"))
            {
                strong.push(args[i].span());
                strong.push(args[i + 1].span());
            }
            if strong.is_empty() {
                out.hit(Caution, "service_or_process_kill", &[prog_span]);
            } else {
                strong.insert(0, prog_span);
                out.hit(Dangerous, "service_or_process_kill", &strong);
            }
        }
        _ => {}
    }

    // Shutdown and reboot.
    if matches!(
        program,
        "shutdown" | "reboot" | "poweroff" | "halt" | "stop-computer" | "restart-computer"
    ) {
        out.hit(Dangerous, "shutdown", &[prog_span]);
    }
    if matches!(program, "init" | "telinit") && lower.first().is_some_and(|a| a == "0" || a == "6")
    {
        out.hit(Dangerous, "shutdown", &[prog_span, args[0].span()]);
    }

    // Running downloaded or computed code.
    if matches!(program, "iex" | "invoke-expression") {
        out.hit(Dangerous, "remote_code", &[prog_span]);
    }
    if POWERSHELLS.contains(&program) {
        for (w, l) in args.iter().zip(&lower) {
            if ps_param(l, "encodedcommand", 1) || l == "-ec" {
                out.hit(Dangerous, "encoded_command", &[prog_span, w.span()]);
            }
        }
    }

    // Git history rewrites.
    if program == "git" {
        classify_git(args, &lower, prog_span, out);
    }

    // Elevation through PowerShell.
    if matches!(program, "start-process" | "saps" | "start")
        && let Some(i) = lower.iter().position(|a| ps_param(a, "verb", 2))
        && lower.get(i + 1).is_some_and(|v| v == "runas")
    {
        out.hit(
            Dangerous,
            "elevation",
            &[prog_span, args[i].span(), args[i + 1].span()],
        );
    }
}

fn classify_git(args: &[&Word], lower: &[String], prog_span: (usize, usize), out: &mut Collector) {
    use DangerLevel::{Caution, Dangerous};
    // Skip global options (`-C dir`, `-c key=value`, `--no-pager`).
    let mut i = 0;
    while i < lower.len() && lower[i].starts_with('-') {
        if lower[i] == "-c" || lower[i] == "-C" || args[i].text == "-C" {
            i += 1;
        }
        i += 1;
    }
    let Some(sub) = lower.get(i) else {
        return;
    };
    let rest = &lower[i + 1..];
    let rest_words = &args[i + 1..];
    let sub_span = args[i].span();
    let find = |pred: &dyn Fn(&str, &str) -> bool| -> Vec<(usize, usize)> {
        rest_words
            .iter()
            .zip(rest)
            .filter(|(w, l)| pred(l, &w.text))
            .map(|(w, _)| w.span())
            .collect()
    };
    let hit = |out: &mut Collector, level, found: Vec<(usize, usize)>| {
        if !found.is_empty() {
            let mut spans = vec![prog_span, sub_span];
            spans.extend(found);
            out.hit(level, "git_destructive", &spans);
        }
    };
    match sub.as_str() {
        "push" => {
            hit(
                out,
                Dangerous,
                find(&|l, raw| {
                    l.starts_with("--force")
                        || short_flags(raw).is_some_and(|f| f.contains('f'))
                        || l.starts_with('+') && l.len() > 1
                }),
            );
            hit(
                out,
                Caution,
                find(&|l, raw| {
                    l == "--delete"
                        || short_flags(raw) == Some("d")
                        || l.starts_with(':') && l.len() > 1
                }),
            );
        }
        "reset" => hit(out, Dangerous, find(&|l, _| l == "--hard")),
        "clean" => hit(
            out,
            Dangerous,
            find(&|l, raw| l == "--force" || short_flags(raw).is_some_and(|f| f.contains('f'))),
        ),
        "branch" => hit(out, Caution, find(&|_, raw| raw == "-D")),
        _ => {}
    }
}

/// True when a (lower-case) argument names a credential or secret file.
fn names_secret(arg: &str) -> bool {
    let path = arg.replace('\\', "/");
    let path = path.trim_end_matches('/');
    let dirs = [".ssh", ".aws", ".gnupg", ".kube", ".docker"];
    for dir in dirs {
        if path == dir
            || path.ends_with(&format!("/{dir}"))
            || path.starts_with(&format!("{dir}/"))
            || path.contains(&format!("/{dir}/"))
        {
            return true;
        }
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    let key_files = ["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"];
    if key_files.iter().any(|k| file.starts_with(k)) {
        return true;
    }
    if [".p8", ".p12", ".pfx"]
        .iter()
        .any(|ext| file.ends_with(ext))
        && file.len() > 3
    {
        return true;
    }
    if [".netrc", "_netrc", ".git-credentials", ".pgpass"].contains(&file) {
        return true;
    }
    if file == ".env" {
        return true;
    }
    if let Some(suffix) = file.strip_prefix(".env.") {
        return !matches!(
            suffix,
            "example" | "sample" | "template" | "dist" | "defaults"
        );
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use DangerLevel::{Caution, Dangerous, Safe};

    fn level(cmd: &str) -> DangerLevel {
        classify(cmd).level
    }

    fn reasons(cmd: &str) -> Vec<&'static str> {
        classify(cmd).reasons
    }

    /// The texts covered by the spans.
    fn spanned(cmd: &str) -> Vec<String> {
        classify(cmd)
            .spans
            .iter()
            .map(|&(s, e)| cmd[s..e].to_owned())
            .collect()
    }

    #[test]
    fn safe_commands() {
        for cmd in [
            "ls -la",
            "git status",
            "cargo build --release",
            "Get-ChildItem -Recurse",
            "dir /s",
            "echo hello world",
            "npm test && npm run lint",
            "reg query HKLM\\Software",
            "icacls C:\\code",
            "sc query wuauserv",
            "git push origin main",
            "cat README.md",
            "type .env.example",
            "",
        ] {
            assert_eq!(level(cmd), Safe, "{cmd}: {:?}", classify(cmd));
        }
    }

    #[test]
    fn recursive_and_forced_deletes() {
        for cmd in [
            "rm -rf build",
            "rm -fr /",
            "rm -R -f x",
            "rm --recursive x",
            "Remove-Item -Recurse -Force C:\\x",
            "remove-item C:\\x -rec",
            "ri x -Recurse",
            "del /s /q C:\\temp",
            "rd /s C:\\temp",
            "rmdir /S /Q dir",
            "find . -name '*.o' -delete",
            "find . -exec rm {} ;",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert_eq!(reasons(cmd), ["recursive_delete"], "{cmd}");
        }
        assert_eq!(spanned("rm -rf build"), ["rm -rf"]);
        assert_eq!(
            spanned("Remove-Item -Recurse -Force C:\\x"),
            ["Remove-Item -Recurse -Force"]
        );
        assert_eq!(spanned("rd /s C:\\temp"), ["rd /s"]);
        // Plain and forced single deletes only need care.
        for cmd in [
            "rm file.txt",
            "rm -f file",
            "del file.txt",
            "Remove-Item x -Force",
        ] {
            assert_eq!(level(cmd), Caution, "{cmd}");
            assert_eq!(reasons(cmd), ["delete"], "{cmd}");
        }
    }

    #[test]
    fn disks() {
        for cmd in [
            "format D: /q",
            "diskpart",
            "mkfs.ext4 /dev/sdb1",
            "dd if=/dev/zero of=/dev/sda bs=1M",
            "Format-Volume -DriveLetter D",
            "Clear-Disk -Number 1",
            "diskutil eraseDisk APFS X disk2",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert_eq!(reasons(cmd), ["disk_format"], "{cmd}");
        }
        assert_eq!(spanned("dd if=a of=/dev/sda"), ["dd", "of=/dev/sda"]);
        assert_eq!(level("dd if=a"), Safe);
    }

    #[test]
    fn permissions() {
        for cmd in [
            "chmod -R 777 /",
            "chown -R me:me /srv",
            "icacls C:\\x /grant Everyone:F",
            "icacls C:\\x /reset /T",
            "takeown /f C:\\Windows",
            "Set-Acl -Path x -AclObject $acl",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert_eq!(reasons(cmd), ["permissions"], "{cmd}");
        }
        assert_eq!(level("chmod +x run.sh"), Caution);
        assert_eq!(spanned("chmod -R 777 /"), ["chmod -R"]);
    }

    #[test]
    fn registry() {
        for cmd in [
            "reg add HKLM\\Software\\X /v Y /d 1",
            "reg delete HKCU\\Software\\X /f",
            "Set-ItemProperty HKLM:\\Software\\X -Name Y -Value 1",
            "New-ItemProperty -Path 'HKLM:\\SOFTWARE\\X' -Name Y",
            "regedit /s tweak.reg",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert_eq!(reasons(cmd), ["registry_write"], "{cmd}");
        }
        assert_eq!(
            level("Set-ItemProperty HKCU:\\Software\\X -Name Y -Value 1"),
            Caution
        );
        assert_eq!(
            spanned("Set-ItemProperty HKLM:\\Software\\X -Name Y"),
            ["Set-ItemProperty HKLM:\\Software\\X"]
        );
    }

    #[test]
    fn services_and_processes() {
        for cmd in [
            "Stop-Service wuauserv",
            "sc delete MyService",
            "sc.exe stop MyService",
            "taskkill /f /im node.exe",
            "kill -9 1234",
            "kill -s KILL 1234",
            "pkill -9 node",
            "Stop-Process -Name node -Force",
            "net stop spooler",
            "sudo systemctl stop nginx",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert!(
                reasons(cmd).contains(&"service_or_process_kill"),
                "{cmd}: {:?}",
                reasons(cmd)
            );
        }
        for cmd in ["kill 1234", "taskkill /im node.exe", "Stop-Process -Id 5"] {
            assert_eq!(level(cmd), Caution, "{cmd}");
        }
        assert_eq!(spanned("taskkill /f /im node.exe"), ["taskkill /f"]);
    }

    #[test]
    fn shutdown_and_reboot() {
        for cmd in [
            "shutdown /s /t 0",
            "shutdown -h now",
            "reboot",
            "Restart-Computer -Force",
            "Stop-Computer",
            "systemctl reboot",
            "init 0",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert_eq!(reasons(cmd), ["shutdown"], "{cmd}");
        }
    }

    #[test]
    fn remote_code() {
        for cmd in [
            "curl -fsSL https://x.sh | sh",
            "curl https://x | sudo bash",
            "wget -qO- https://x | python3 -",
            "iwr https://x/install.ps1 | iex",
            "irm https://x | Invoke-Expression",
            "Invoke-Expression $code",
            "bash <(curl -s https://x)",
            "sh -c \"$(curl -fsSL https://x)\"",
            "powershell -c \"iwr https://x | iex\"",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert!(
                reasons(cmd).contains(&"remote_code"),
                "{cmd}: {:?}",
                reasons(cmd)
            );
        }
        assert_eq!(spanned("curl https://x | sh"), ["curl", "sh"]);
        assert_eq!(level("curl -o out.txt https://x"), Safe);
        assert_eq!(level("cat log | grep error"), Safe);
        assert_eq!(
            reasons("powershell -EncodedCommand ZQBjAGgAbwA="),
            ["encoded_command"]
        );
    }

    #[test]
    fn secrets() {
        for cmd in [
            "cat ~/.ssh/id_rsa",
            "type C:\\Users\\me\\.ssh\\id_ed25519",
            "Get-Content $HOME\\.aws\\credentials",
            "cat .env",
            "cat .env.production",
            "scp AuthKey_ABC.p8 host:",
            "ls ~/.ssh",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert_eq!(reasons(cmd), ["secret_read"], "{cmd}");
        }
        assert_eq!(spanned("cat ~/.ssh/id_rsa"), ["~/.ssh/id_rsa"]);
        assert_eq!(level("cat .env.example"), Safe);
        assert_eq!(level("cat environment.md"), Safe);
    }

    #[test]
    fn git_rewrites() {
        for cmd in [
            "git push --force",
            "git push -f origin main",
            "git push origin +main",
            "git push --force-with-lease",
            "git reset --hard HEAD~3",
            "git clean -fdx",
            "git -C repo reset --hard",
        ] {
            assert_eq!(level(cmd), Dangerous, "{cmd}");
            assert_eq!(reasons(cmd), ["git_destructive"], "{cmd}");
        }
        assert_eq!(spanned("git push -f origin"), ["git push -f"]);
        assert_eq!(level("git branch -D old"), Caution);
        assert_eq!(level("git reset HEAD file"), Safe);
    }

    #[test]
    fn elevation_is_flagged_and_the_rest_still_classified() {
        let c = classify("sudo rm -rf /var/x");
        assert_eq!(c.level, Dangerous);
        assert_eq!(c.reasons, ["elevation", "recursive_delete"]);
        assert_eq!(level("sudo ls"), Dangerous);
        assert_eq!(level("runas /user:Administrator cmd"), Dangerous);
        assert_eq!(
            reasons("Start-Process powershell -Verb RunAs"),
            ["elevation"]
        );
        assert_eq!(level("Start-Process notepad"), Safe);
    }

    #[test]
    fn lists_pipes_and_nesting() {
        // Any part of a list counts.
        assert_eq!(level("cd build && rm -rf *"), Dangerous);
        assert_eq!(level("echo a; shutdown now"), Dangerous);
        assert_eq!(level("ls || reboot"), Dangerous);
        assert_eq!(
            level("Get-ChildItem | ForEach-Object { Remove-Item $_ -Recurse }"),
            Dangerous
        );
        // Nested command strings in every shell.
        assert_eq!(level("bash -c 'rm -rf /tmp/x'"), Dangerous);
        assert_eq!(level("cmd /c rd /s /q C:\\x"), Dangerous);
        assert_eq!(level("cmd.exe /C \"del /s /q x\""), Dangerous);
        assert_eq!(level("pwsh -Command \"Remove-Item x -Recurse\""), Dangerous);
        assert_eq!(spanned("bash -c 'rm -rf x'"), ["rm -rf"]);
        // Quoted text is an argument, not a command.
        assert_eq!(level("echo 'rm -rf /'"), Safe);
        assert_eq!(level("git commit -m \"shutdown handler\""), Safe);
        // Wrappers and assignments are skipped.
        assert_eq!(level("FOO=1 nohup rm -rf x"), Dangerous);
        assert_eq!(level("C:\\Windows\\System32\\shutdown.exe /r"), Dangerous);
    }

    #[test]
    fn spans_are_utf8_byte_offsets() {
        let cmd = "echo \"안녕하세요\" && rm -rf ./빌드 && cat ~/.ssh/설정";
        let c = classify(cmd);
        assert_eq!(c.level, Dangerous);
        let texts: Vec<&str> = c.spans.iter().map(|&(s, e)| &cmd[s..e]).collect();
        assert_eq!(texts, ["rm -rf", "~/.ssh/설정"]);
        let first = c.spans[0];
        assert_eq!(first.0, cmd.find("rm").unwrap());
        assert!(cmd.is_char_boundary(c.spans[1].1));
        assert_eq!(c.wire_spans()[0], [first.0 as u32, first.1 as u32]);
        // Non-ASCII command words and emoji do not break the tokenizer.
        assert_eq!(level("echo 🎉 && Remove-Item 폴더 -Recurse"), Dangerous);
        assert_eq!(
            spanned("echo 🎉 && Remove-Item 폴더 -Recurse"),
            ["Remove-Item", "-Recurse"]
        );
        // Unterminated quotes are tolerated.
        assert_eq!(level("echo \"unterminated && rm -rf x"), Safe);
    }

    #[test]
    fn voice_approval_is_an_allow_list() {
        for ok in ["ls -la", "git log -n 5", "pwd", "grep TODO src/main.rs"] {
            assert!(voice_approvable(ok), "{ok}");
        }
        // `classify` calls all of these safe: it only knows what is risky.
        for unknown in [
            "python tools/release.py",
            "npm publish",
            "cargo run",
            "make deploy",
            "curl https://example.com/x -o y",
            "scp report.txt other.example:",
            "./run.sh",
            "/usr/bin/ls",
            "git push",
            "git commit -m x",
            "git -c core.pager=evil log",
            // They can run what the repository's config names.
            "git status",
            "git diff src/main.rs",
            "git show HEAD",
            "git log -p",
            "git log --patch",
            // They change state, or name a file to write.
            "hostname evil",
            "date -s 12:00",
            // Credentials the classifier does not know.
            "cat aws/credentials",
            "cat .npmrc",
            "tail deploy-token.txt",
            "head server.pem",
            "grep -r password src",
        ] {
            assert!(!voice_approvable(unknown), "{unknown}");
        }
        for tricky in [
            "ls; rm -rf /",
            "ls && make deploy",
            "cat a > b",
            "echo $(make deploy)",
            "echo `make deploy`",
            "ls | sh",
            "git diff --output=/tmp/x",
            "git diff --ext-diff",
            "cat 'a b'",
            "ls\nmake deploy",
            "cat ~/.ssh/id_rsa",
            "cat .env",
            "",
        ] {
            assert!(!voice_approvable(tricky), "{tricky}");
        }
        assert!(!voice_approvable(&format!("ls {}", "a".repeat(200))));
    }
}
