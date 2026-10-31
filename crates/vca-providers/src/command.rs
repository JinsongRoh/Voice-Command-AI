//! Running provider command-line tools, behind a trait so tests can fake them.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;

/// Output of a finished command.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CmdOutput {
    /// Exit code; `None` when killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs short-lived commands.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Runs `program args...` to completion. `Err(NotFound)` means the
    /// program is not installed.
    async fn run(&self, program: &Path, args: &[String]) -> std::io::Result<CmdOutput>;
}

/// Runs commands with `tokio::process`, with a timeout.
#[derive(Debug, Clone)]
pub struct TokioRunner {
    pub timeout: Duration,
}

impl Default for TokioRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

#[async_trait]
impl CommandRunner for TokioRunner {
    async fn run(&self, program: &Path, args: &[String]) -> std::io::Result<CmdOutput> {
        let child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let out = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "command timed out")
            })??;
        Ok(CmdOutput {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::sync::Mutex;

    /// Returns scripted outputs keyed by the joined argument list.
    #[derive(Default)]
    pub struct FakeRunner {
        pub responses: Mutex<Vec<(String, std::io::Result<CmdOutput>)>>,
        pub calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        pub fn on(self, args: &str, out: CmdOutput) -> Self {
            self.responses
                .lock()
                .unwrap()
                .push((args.to_owned(), Ok(out)));
            self
        }
        pub fn not_installed(self, args: &str) -> Self {
            self.responses.lock().unwrap().push((
                args.to_owned(),
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                )),
            ));
            self
        }
    }

    pub fn ok(stdout: &str) -> CmdOutput {
        CmdOutput {
            code: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    #[async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, _program: &Path, args: &[String]) -> std::io::Result<CmdOutput> {
            let key = args.join(" ");
            self.calls.lock().unwrap().push(key.clone());
            let responses = self.responses.lock().unwrap();
            for (k, v) in responses.iter() {
                if *k == key {
                    return match v {
                        Ok(o) => Ok(o.clone()),
                        Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
                    };
                }
            }
            Ok(CmdOutput {
                code: Some(1),
                stdout: String::new(),
                stderr: format!("unexpected command: {key}"),
            })
        }
    }
}
