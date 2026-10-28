//! Classify shell command lines as heavy (builds, tests, installs, container
//! starts) or light.
//!
//! The classifier understands enough shell syntax to be useful on the command
//! strings that coding agents run:
//!
//! - quoting (`'...'`, `"..."`, backslash escapes) and comments;
//! - command lists and pipelines (`&&`, `||`, `;`, `|`, `&`, newlines,
//!   `$(...)`, backticks): a line is heavy if **any** part is heavy;
//! - environment prefixes (`RUSTFLAGS=... cargo build`);
//! - wrappers such as `sudo`, `env`, `nice`, `timeout`, `time`, `nohup`,
//!   `xargs`, `npx`, `uv run`, and `sh -c '...'` (classified recursively);
//! - binaries given by path (`./gradlew`, `/usr/bin/make`, `cargo.exe`).
//!
//! Rules are matched by program name plus an optional argument pattern. User
//! rules from the policy file take precedence over the built-in rules, and
//! light rules can override heavy ones (for example to treat `cargo check` as
//! light on a large machine).

use serde::{Deserialize, Serialize};
use vca_core::JobClass;

/// One classification rule as written in `policies.toml`.
///
/// ```toml
/// [[classifier.heavy]]
/// program = "cargo"
/// args = ["build", "test", "nextest run"]
/// ```
///
/// `program` is compared against the binary's file name (lower-cased, with
/// `.exe`/`.cmd`/`.bat` removed). Each entry in `args` is a space-separated
/// word sequence. A pattern that starts with a word that is not a flag must
/// appear at the first positional argument (the second one is also tried when
/// the first may be the value of a preceding flag, as in
/// `npm --prefix web install`). A pattern starting with a flag (`--build`,
/// `-m pytest`) may appear anywhere before a `--` separator. An empty `args`
/// list matches every invocation of the program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Classifier configuration (the `[classifier]` table in `policies.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClassifierConfig {
    /// Include the built-in rule set (default `true`).
    pub builtin_rules: bool,
    /// Extra rules that mark commands heavy.
    pub heavy: Vec<RuleSpec>,
    /// Rules that mark commands light; they override heavy rules.
    pub light: Vec<RuleSpec>,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            builtin_rules: true,
            heavy: Vec::new(),
            light: Vec::new(),
        }
    }
}

/// Result of classifying a command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    pub class: JobClass,
    /// Human-readable description of the rule that decided, e.g. `cargo test`.
    pub matched: Option<String>,
}

#[derive(Debug, Clone)]
struct Rule {
    program: String,
    patterns: Vec<Vec<String>>,
    class: JobClass,
}

impl Rule {
    fn from_spec(spec: &RuleSpec, class: JobClass) -> Self {
        Self {
            program: normalize_program(&spec.program),
            patterns: spec
                .args
                .iter()
                .map(|p| p.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
                .filter(|p| !p.is_empty())
                .collect(),
            class,
        }
    }

    /// Returns the matched description if this rule applies.
    fn matches(&self, program: &str, args: &[String]) -> Option<String> {
        if self.program != program {
            return None;
        }
        if self.patterns.is_empty() {
            return Some(self.program.clone());
        }
        let args: &[String] = match args.iter().position(|a| a == "--") {
            Some(end) => &args[..end],
            None => args,
        };
        for pattern in &self.patterns {
            if pattern_matches(pattern, args) {
                return Some(format!("{} {}", self.program, pattern.join(" ")));
            }
        }
        None
    }
}

fn is_flag(token: &str) -> bool {
    token.starts_with('-') || token.starts_with('+')
}

fn pattern_matches(pattern: &[String], args: &[String]) -> bool {
    let at = |start: usize| {
        args.len() >= start + pattern.len()
            && pattern.iter().zip(&args[start..]).all(|(p, a)| p == a)
    };
    if pattern[0].starts_with('-') {
        return (0..args.len()).any(at);
    }
    let positionals: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| !is_flag(a))
        .map(|(i, _)| i)
        .take(2)
        .collect();
    let Some(&first) = positionals.first() else {
        return false;
    };
    if at(first) {
        return true;
    }
    // The first positional may be the value of a flag (`--prefix web install`).
    if first > 0 && !args[first - 1].contains('=') && args[first - 1].starts_with('-') {
        if let Some(&second) = positionals.get(1) {
            return at(second);
        }
    }
    false
}

/// Classifies command lines using an ordered rule list.
#[derive(Debug, Clone)]
pub struct Classifier {
    rules: Vec<Rule>,
}

impl Default for Classifier {
    fn default() -> Self {
        Self::new(&ClassifierConfig::default())
    }
}

/// Maximum nesting depth for `sh -c` and runner recursion.
const MAX_DEPTH: usize = 4;

impl Classifier {
    pub fn new(config: &ClassifierConfig) -> Self {
        let mut rules = Vec::new();
        rules.extend(
            config
                .light
                .iter()
                .map(|s| Rule::from_spec(s, JobClass::Light)),
        );
        rules.extend(
            config
                .heavy
                .iter()
                .map(|s| Rule::from_spec(s, JobClass::Heavy)),
        );
        if config.builtin_rules {
            rules.extend(
                builtin_heavy_rules()
                    .iter()
                    .map(|s| Rule::from_spec(s, JobClass::Heavy)),
            );
        }
        Self { rules }
    }

    /// Classifies a full command line.
    pub fn classify(&self, command: &str) -> Classification {
        self.classify_depth(command, 0).unwrap_or(Classification {
            class: JobClass::Light,
            matched: None,
        })
    }

    /// Shorthand for `classify(command).class`.
    pub fn class_of(&self, command: &str) -> JobClass {
        self.classify(command).class
    }

    fn classify_depth(&self, command: &str, depth: usize) -> Option<Classification> {
        let mut light_match = None;
        for segment in split_commands(command) {
            match self.classify_segment(&segment, depth) {
                Some(c) if c.class == JobClass::Heavy => return Some(c),
                Some(c) => light_match = light_match.or(Some(c)),
                None => {}
            }
        }
        light_match
    }

    fn classify_segment(&self, words: &[String], depth: usize) -> Option<Classification> {
        let words = strip_wrappers(words);
        let (program_raw, args) = words.split_first()?;
        let program = normalize_program(program_raw);

        if depth < MAX_DEPTH {
            // `sh -c 'script'` and friends: classify the script itself.
            if let Some(script) = shell_script_arg(&program, args) {
                return self.classify_depth(&script, depth + 1);
            }
            // Package runners: classify the command they run.
            if let Some(rest) = runner_target(&program, args) {
                if let Some(c) = self.classify_segment(rest, depth + 1) {
                    return Some(c);
                }
            }
        }

        self.rules.iter().find_map(|rule| {
            rule.matches(&program, args).map(|matched| Classification {
                class: rule.class,
                matched: Some(matched),
            })
        })
    }
}

/// Lower-cases a program name, takes its file name, and strips Windows
/// executable suffixes.
fn normalize_program(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let lower = base.to_ascii_lowercase();
    for suffix in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            return stripped.to_owned();
        }
    }
    lower
}

fn is_env_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Skips environment assignments and command wrappers, returning the words of
/// the command that actually runs.
fn strip_wrappers(mut words: &[String]) -> &[String] {
    loop {
        while words.first().is_some_and(|w| is_env_assignment(w)) {
            words = &words[1..];
        }
        let Some(first) = words.first() else {
            return words;
        };
        let program = normalize_program(first);
        // Flags of each wrapper that consume the following word.
        let flags_with_value: &[&str] = match program.as_str() {
            "sudo" | "doas" => &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T"],
            "env" => &["-u", "-C", "-S", "--unset", "--chdir"],
            "nice" => &["-n", "--adjustment"],
            "ionice" => &["-c", "-n", "-p", "-P", "-u"],
            "timeout" => &["-s", "-k", "--signal", "--kill-after"],
            "stdbuf" => &["-i", "-o", "-e"],
            "xargs" => &["-I", "-n", "-P", "-L", "-d", "-E", "-s", "-a"],
            "time" | "nohup" | "exec" | "command" | "builtin" | "caffeinate" => &[],
            _ => return words,
        };
        words = &words[1..];
        while let Some(w) = words.first() {
            if w == "--" {
                words = &words[1..];
                break;
            } else if flags_with_value.contains(&w.as_str()) {
                words = words.get(2..).unwrap_or(&[]);
            } else if w.starts_with('-') || (program == "env" && is_env_assignment(w)) {
                words = &words[1..];
            } else {
                break;
            }
        }
        if program == "timeout" && !words.is_empty() {
            // Skip the duration argument.
            words = &words[1..];
        }
    }
}

/// For `sh -c SCRIPT` style invocations, returns SCRIPT. `cmd /c` and
/// PowerShell take the rest of the line as the command, so it is re-joined.
fn shell_script_arg(program: &str, args: &[String]) -> Option<String> {
    let (flags, rest_of_line): (&[&str], bool) = match program {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" => {
            (&["-c", "-lc", "-ec", "-xc", "-cl"], false)
        }
        "cmd" => (&["/c", "/C", "/k", "/K"], true),
        "powershell" | "pwsh" => (&["-command", "-Command", "-c"], true),
        _ => return None,
    };
    let idx = args.iter().position(|a| flags.contains(&a.as_str()))?;
    let rest = args.get(idx + 1..).filter(|r| !r.is_empty())?;
    if rest_of_line {
        Some(rest.join(" "))
    } else {
        Some(rest[0].clone())
    }
}

/// For package runners (`npx tsc`, `uv run pytest`), returns the command they run.
fn runner_target<'a>(program: &str, args: &'a [String]) -> Option<&'a [String]> {
    let skip_flags = |args: &'a [String]| -> Option<&'a [String]> {
        let i = args.iter().position(|a| !a.starts_with('-'))?;
        Some(&args[i..])
    };
    match program {
        "npx" | "bunx" | "pnpx" | "uvx" => skip_flags(args),
        "pipx" | "uv" | "poetry" | "pdm" | "hatch" | "rye" | "pixi" | "conda" | "mamba" => {
            let rest = skip_flags(args)?;
            (rest.first()? == "run")
                .then(|| skip_flags(&rest[1..]))
                .flatten()
        }
        "pnpm" | "yarn" | "bun" | "npm" => {
            let rest = skip_flags(args)?;
            (rest.first()? == "exec" || (program == "npm" && rest.first()? == "x"))
                .then(|| skip_flags(&rest[1..]))
                .flatten()
        }
        _ => None,
    }
}

/// Splits a command line into simple commands (lists of words), honouring
/// quotes and treating every control operator as a separator.
pub(crate) fn split_commands(input: &str) -> Vec<Vec<String>> {
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = input.chars().peekable();

    fn end_word(words: &mut Vec<String>, word: &mut String, in_word: &mut bool) {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    }
    fn end_segment(segments: &mut Vec<Vec<String>>, words: &mut Vec<String>) {
        if !words.is_empty() {
            segments.push(std::mem::take(words));
        }
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => match chars.peek() {
                            Some(&n @ ('"' | '\\' | '$' | '`')) => {
                                word.push(n);
                                chars.next();
                            }
                            _ => word.push('\\'),
                        },
                        _ => word.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.peek() {
                    // Windows paths (`C:\tools\cargo.exe`): escaping a plain
                    // character is rare in POSIX shells, so keep the backslash.
                    Some(&n) if n.is_alphanumeric() || matches!(n, '.' | '_' | '-') => {
                        word.push('\\');
                    }
                    Some('\n') => {
                        chars.next();
                    }
                    Some(&n) => {
                        word.push(n);
                        chars.next();
                    }
                    None => {}
                }
            }
            '#' if !in_word => {
                // Comment until end of line.
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
                end_segment(&mut segments, &mut words);
            }
            '&' if word.ends_with('>') || word.ends_with('<') => {
                // Redirection such as `2>&1`.
                word.push(c);
            }
            ';' | '\n' | '&' | '|' | '(' | ')' | '`' => {
                end_word(&mut words, &mut word, &mut in_word);
                end_segment(&mut segments, &mut words);
            }
            '$' if chars.peek() == Some(&'(') => {
                end_word(&mut words, &mut word, &mut in_word);
                end_segment(&mut segments, &mut words);
            }
            c if c.is_whitespace() => end_word(&mut words, &mut word, &mut in_word),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut words, &mut word, &mut in_word);
    end_segment(&mut segments, &mut words);
    segments
}

fn rule(program: &str, args: &[&str]) -> RuleSpec {
    RuleSpec {
        program: program.to_owned(),
        args: args.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// The built-in heavy rules: builds, tests, dependency installs and
/// container starts across common ecosystems.
pub fn builtin_heavy_rules() -> Vec<RuleSpec> {
    const JS_PM: &[&str] = &[
        "install",
        "i",
        "ci",
        "add",
        "update",
        "upgrade",
        "rebuild",
        "build",
        "test",
        "t",
        "run build",
        "run test",
        "run compile",
        "run typecheck",
        "run e2e",
        "run-script build",
        "run-script test",
    ];
    const CONTAINER: &[&str] = &[
        "build",
        "buildx",
        "run",
        "start",
        "create",
        "compose up",
        "compose build",
        "compose run",
        "compose start",
        "image build",
        "container run",
        "container start",
    ];
    let mut rules = vec![
        // Rust
        rule(
            "cargo",
            &[
                "build",
                "b",
                "test",
                "t",
                "check",
                "c",
                "clippy",
                "run",
                "r",
                "bench",
                "doc",
                "install",
                "nextest",
                "miri",
                "llvm-cov",
                "tarpaulin",
                "fuzz",
                "rustc",
                "xtask",
            ],
        ),
        rule("rustc", &[]),
        rule("cross", &["build", "test", "check", "run"]),
        rule("maturin", &["build", "develop"]),
        rule("wasm-pack", &["build", "test"]),
        // JavaScript / TypeScript
        rule("npm", JS_PM),
        rule("pnpm", JS_PM),
        rule("yarn", JS_PM),
        rule("bun", JS_PM),
        rule("deno", &["compile", "test", "bench", "install"]),
        rule("tsc", &[]),
        rule("webpack", &[]),
        rule("vite", &["build"]),
        rule("next", &["build"]),
        rule("nuxt", &["build", "generate"]),
        rule("astro", &["build"]),
        rule("rollup", &[]),
        rule("esbuild", &[]),
        rule("jest", &[]),
        rule("vitest", &[]),
        rule("mocha", &[]),
        rule("karma", &["start"]),
        rule("playwright", &["test"]),
        rule("cypress", &["run"]),
        rule("turbo", &["run", "build", "test"]),
        rule("nx", &["build", "test", "run-many", "affected", "run"]),
        rule("lerna", &["run"]),
        // Go
        rule(
            "go",
            &["build", "test", "install", "run", "vet", "generate"],
        ),
        // JVM
        rule("gradle", &[]),
        rule("gradlew", &[]),
        rule("mvn", &[]),
        rule("mvnw", &[]),
        rule("ant", &[]),
        rule("sbt", &[]),
        rule("javac", &[]),
        rule("kotlinc", &[]),
        rule("scalac", &[]),
        // C / C++ and generic build systems
        rule("make", &[]),
        rule("gmake", &[]),
        rule("nmake", &[]),
        rule("cmake", &["--build"]),
        rule("ctest", &[]),
        rule("ninja", &[]),
        rule("meson", &["compile", "test"]),
        rule("bazel", &["build", "test", "run", "coverage"]),
        rule("bazelisk", &["build", "test", "run", "coverage"]),
        rule("buck2", &["build", "test", "run"]),
        rule("scons", &[]),
        rule("msbuild", &[]),
        rule("gcc", &[]),
        rule("g++", &[]),
        rule("clang", &[]),
        rule("clang++", &[]),
        rule("cc", &[]),
        rule("c++", &[]),
        rule("cl", &[]),
        rule("zig", &["build", "test", "build-exe", "cc"]),
        // Containers
        rule("docker", CONTAINER),
        rule("podman", CONTAINER),
        rule("nerdctl", CONTAINER),
        rule("docker-compose", &["up", "build", "run", "start"]),
        rule("buildah", &["bud", "build"]),
        // Python
        rule("pytest", &[]),
        rule("py.test", &[]),
        rule("tox", &[]),
        rule("nox", &[]),
        rule("pip", &["install", "wheel", "download"]),
        rule("pip3", &["install", "wheel", "download"]),
        rule("uv", &["sync", "pip install", "build", "tool install"]),
        rule("poetry", &["install", "build", "update", "lock"]),
        rule("pdm", &["install", "build", "sync"]),
        rule("conda", &["install", "create", "env create", "update"]),
        rule("mamba", &["install", "create", "env create", "update"]),
        // .NET / Apple / mobile
        rule(
            "dotnet",
            &["build", "test", "publish", "restore", "run", "pack"],
        ),
        rule("swift", &["build", "test", "run"]),
        rule("xcodebuild", &[]),
        rule("flutter", &["build", "test", "pub get"]),
        rule("dart", &["compile", "test"]),
        rule("pod", &["install", "update"]),
        // Other ecosystems
        rule("composer", &["install", "update"]),
        rule("phpunit", &[]),
        rule("bundle", &["install", "exec rspec", "exec rake"]),
        rule("rspec", &[]),
        rule("rake", &["test", "spec", "build", "compile"]),
        rule("mix", &["compile", "test", "deps.get"]),
        rule("stack", &["build", "test", "install"]),
        rule("cabal", &["build", "test", "install"]),
        rule("ghc", &[]),
    ];
    for py in ["python", "python3", "py"] {
        rules.push(rule(
            py,
            &[
                "-m pytest",
                "-m tox",
                "-m unittest",
                "-m pip install",
                "-m build",
                "setup.py build",
                "setup.py install",
                "setup.py test",
                "manage.py test",
            ],
        ));
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heavy(cmd: &str) -> bool {
        Classifier::default().class_of(cmd) == JobClass::Heavy
    }

    #[test]
    fn splits_on_operators_and_quotes() {
        let segs = split_commands(
            r#"FOO=1 cargo test -- --nocapture && echo "a && b" | tail -n 5; ls 2>&1"#,
        );
        assert_eq!(
            segs,
            vec![
                vec!["FOO=1", "cargo", "test", "--", "--nocapture"],
                vec!["echo", "a && b"],
                vec!["tail", "-n", "5"],
                vec!["ls", "2>&1"],
            ]
        );
    }

    #[test]
    fn heavy_builtins() {
        for cmd in [
            "cargo build",
            "cargo test --workspace",
            "cargo +nightly clippy --all-targets",
            "cargo -q check",
            "rustc main.rs",
            "npm install",
            "npm ci",
            "npm run build",
            "npm --prefix web install",
            "pnpm test",
            "yarn build",
            "bun install",
            "npx tsc --noEmit",
            "npx --yes vite build",
            "webpack --mode production",
            "go test ./...",
            "./gradlew assemble",
            "mvn -q package",
            "make -j8",
            "cmake --build build --config Release",
            "ninja -C out",
            "bazel test //...",
            "docker build -t x .",
            "docker compose up -d",
            "docker-compose up",
            "podman run --rm alpine",
            "pytest -x tests/",
            "python -m pytest",
            "python3 setup.py build",
            "uv run pytest -q",
            "poetry run pytest",
            "tox -e py312",
            "dotnet test",
            "swift build",
            "xcodebuild -scheme App build",
            "C:\\tools\\cargo.exe build",
            "/usr/bin/make install",
        ] {
            assert!(heavy(cmd), "expected heavy: {cmd}");
        }
    }

    #[test]
    fn light_commands() {
        for cmd in [
            "ls -la",
            "git status",
            "cat Cargo.toml",
            "cargo fmt",
            "cargo add serde",
            "cargo --version",
            "npm view react",
            "npm run lint",
            "go fmt ./...",
            "docker ps",
            "docker compose logs",
            "cmake ..",
            "python script.py",
            "echo 'cargo test'",
            "grep -r 'make' .",
            "",
            "   ",
        ] {
            assert!(!heavy(cmd), "expected light: {cmd}");
        }
    }

    #[test]
    fn compound_commands_are_heavy_if_any_part_is() {
        assert!(heavy("cd crates/x && cargo test"));
        assert!(heavy("git pull; npm install"));
        assert!(heavy("cargo test 2>&1 | tail -20"));
        assert!(heavy("ls || make"));
        assert!(heavy("echo $(cargo build --message-format short)"));
        assert!(!heavy("git status && git diff | head"));
    }

    #[test]
    fn wrappers_and_env_prefixes() {
        assert!(heavy(
            "RUSTFLAGS='-D warnings' CARGO_INCREMENTAL=0 cargo build"
        ));
        assert!(heavy("sudo -u builder make"));
        assert!(heavy("sudo -E docker build ."));
        assert!(heavy("env -i PATH=/bin cargo test"));
        assert!(heavy("nice -n 10 cargo build"));
        assert!(heavy("timeout -s KILL 600 cargo test"));
        assert!(heavy("time go build"));
        assert!(heavy("nohup npm run build"));
        assert!(heavy("bash -c 'cd x && cargo test'"));
        assert!(heavy("sh -lc \"npm ci\""));
        assert!(heavy("cmd /c cargo build --release"));
        assert!(heavy("pwsh -Command \"npm ci\""));
        assert!(heavy("echo \\$HOME; cargo test"));
        assert!(!heavy("sudo ls"));
        assert!(!heavy("bash -c 'echo hi'"));
    }

    #[test]
    fn double_dash_stops_pattern_matching() {
        // `-m pytest` after `--` is an argument to the script, not to python.
        assert!(!heavy("python tool.py -- -m pytest"));
    }

    #[test]
    fn user_rules_extend_and_override() {
        let config = ClassifierConfig {
            builtin_rules: true,
            heavy: vec![rule("terraform", &["apply", "plan"])],
            light: vec![rule("cargo", &["check"])],
        };
        let c = Classifier::new(&config);
        assert_eq!(c.class_of("terraform plan"), JobClass::Heavy);
        assert_eq!(c.class_of("cargo check"), JobClass::Light);
        assert_eq!(c.class_of("cargo build"), JobClass::Heavy);
        let explained = c.classify("cargo check");
        assert_eq!(explained.matched.as_deref(), Some("cargo check"));
    }

    #[test]
    fn builtin_rules_can_be_disabled() {
        let config = ClassifierConfig {
            builtin_rules: false,
            heavy: vec![rule("mytool", &[])],
            light: vec![],
        };
        let c = Classifier::new(&config);
        assert_eq!(c.class_of("cargo build"), JobClass::Light);
        assert_eq!(c.class_of("./bin/mytool --fast"), JobClass::Heavy);
    }

    #[test]
    fn explains_match() {
        let c = Classifier::default();
        let r = c.classify("cd x && npm run build");
        assert_eq!(r.class, JobClass::Heavy);
        assert_eq!(r.matched.as_deref(), Some("npm run build"));
        assert_eq!(c.classify("ls").matched, None);
    }
}
