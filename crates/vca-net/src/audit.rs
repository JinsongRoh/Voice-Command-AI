//! Append-only audit log (SEC-07) as JSON lines with size-based rotation.
//!
//! Secrets never reach the log: pairing secrets, TOTP codes and tokens are
//! not part of any event, and free text (command input) passes through
//! [`redact`].
//!
//! Client IPs follow dnspilot C7/C9: `X-Forwarded-For` is honoured only when
//! the TCP peer is loopback (i.e. a local provider agent forwarded the
//! connection); `X-Real-IP` is always ignored. The forwarded IP is
//! informational, since devices are authenticated by key.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use vca_core::{ClientMsg, UnixMillis};

use crate::clock::SharedClock;
use crate::error::{AuthFailure, CloseReason, Result};
use crate::registry::Scope;

/// Header carrying the visitor IP from a provider agent.
pub const X_FORWARDED_FOR: &str = "x-forwarded-for";
/// DNS Pilot client id header (informational only, C9).
pub const X_DNSPILOT_CLIENT_ID: &str = "x-dnspilot-client-id";

/// Determines the client IP for audit purposes.
///
/// * If the socket peer is a loopback address and an `X-Forwarded-For`
///   header is present, the right-most valid IP of that header is used
///   (the one appended by the nearest proxy) and `forwarded` is true.
/// * Otherwise the socket peer IP is used.
///
/// `X-Real-IP` is never consulted.
pub fn client_ip_from_headers(peer: SocketAddr, x_forwarded_for: Option<&str>) -> (IpAddr, bool) {
    if !peer.ip().is_loopback() {
        return (peer.ip(), false);
    }
    let forwarded = x_forwarded_for.and_then(|v| {
        v.rsplit(',')
            .map(str::trim)
            .find(|s| !s.is_empty())
            .and_then(|s| s.parse::<IpAddr>().ok())
    });
    match forwarded {
        Some(ip) => (ip, true),
        None => (peer.ip(), false),
    }
}

/// How a device authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Pairing,
    Totp,
    BackupCode,
    DeviceKey,
    AccessToken,
    RefreshToken,
}

/// One auditable event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AuditEvent {
    ConnectionAttempt {
        conn_id: u64,
        client_ip: IpAddr,
        forwarded: bool,
        /// Provider-supplied client id (informational, unauthenticated).
        provider_client_id: Option<String>,
    },
    ConnectionRejected {
        conn_id: u64,
        client_ip: IpAddr,
        reason: String,
    },
    AuthSucceeded {
        conn_id: u64,
        device_id: String,
        method: AuthMethod,
        scope: Scope,
        client_ip: IpAddr,
    },
    AuthFailed {
        conn_id: u64,
        device_id: Option<String>,
        key_fingerprint: String,
        failure: AuthFailure,
        /// Precise server-side reason (e.g. "pairing secret expired").
        detail: String,
        client_ip: IpAddr,
    },
    RateLimited {
        key: String,
        locked_until: UnixMillis,
    },
    DevicePaired {
        device_id: String,
        name: String,
        scope: Scope,
        key_fingerprint: String,
        client_ip: IpAddr,
    },
    DeviceRevoked {
        device_id: String,
    },
    TokenReuseDetected {
        device_id: String,
        client_ip: IpAddr,
    },
    Disconnected {
        conn_id: u64,
        device_id: String,
        reason: CloseReason,
    },
    Command {
        device_id: String,
        kind: String,
        /// Redacted, truncated JSON of the message.
        detail: String,
    },
    KillSwitchTripped {
        by: String,
    },
    KillSwitchReset {
        by: String,
    },
}

impl AuditEvent {
    /// Audit record for a command from a device, with secrets redacted.
    pub fn command(device_id: &str, msg: &ClientMsg) -> Self {
        let json = serde_json::to_value(msg).unwrap_or(serde_json::Value::Null);
        let kind = json
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        Self::Command {
            device_id: device_id.to_string(),
            kind,
            detail: redact(&json.to_string()),
        }
    }
}

/// A timestamped event, one JSON line in the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub at: UnixMillis,
    #[serde(flatten)]
    pub event: AuditEvent,
}

const MAX_DETAIL_CHARS: usize = 512;

fn redaction_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]+",
            // key=value / key: value style secrets.
            r#"(?i)((?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|authorization|bearer)\\?"?\s*[:=]\s*\\?"?)[^\s"'\\,;&]+"#,
            // Well-known credential formats.
            r"sk-[A-Za-z0-9_-]{16,}",
            r"AKIA[0-9A-Z]{16}",
            r"gh[pousr]_[A-Za-z0-9]{20,}",
            r"xox[abprs]-[A-Za-z0-9-]{10,}",
            r"eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}",
            r"vca1\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
            // Long opaque strings (hex/base64) are likely secrets.
            r"[A-Za-z0-9+/_-]{40,}={0,2}",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("valid redaction regex"))
        .collect()
    })
}

/// Masks likely secrets in free text and truncates it.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for (i, re) in redaction_patterns().iter().enumerate() {
        let replacement = if i < 2 {
            "${1}[REDACTED]"
        } else {
            "[REDACTED]"
        };
        out = re.replace_all(&out, replacement).into_owned();
    }
    if out.chars().count() > MAX_DETAIL_CHARS {
        out = out.chars().take(MAX_DETAIL_CHARS).collect::<String>() + "…";
    }
    out
}

enum Sink {
    File {
        path: PathBuf,
        file: Option<File>,
        size: u64,
        max_bytes: u64,
        keep: usize,
    },
    Memory(Vec<AuditRecord>),
}

/// Append-only audit log. Writes are synchronous and small (one line);
/// a failed write is reported through `tracing` and never blocks the
/// caller's security decision.
pub struct AuditLog {
    clock: SharedClock,
    sink: Mutex<Sink>,
}

impl AuditLog {
    /// Log to `path`, rotating to `path.1`, `path.2`, ... (`keep` files)
    /// when the current file exceeds `max_bytes`.
    pub fn open(
        path: impl Into<PathBuf>,
        max_bytes: u64,
        keep: usize,
        clock: SharedClock,
    ) -> Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            crate::util::create_private_dir(dir)?;
        }
        let file = open_append(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            clock,
            sink: Mutex::new(Sink::File {
                path,
                file: Some(file),
                size,
                max_bytes: max_bytes.max(1024),
                keep: keep.max(1),
            }),
        })
    }

    /// In-memory log (tests, or when no data directory is available).
    pub fn memory(clock: SharedClock) -> Self {
        Self {
            clock,
            sink: Mutex::new(Sink::Memory(Vec::new())),
        }
    }

    /// Appends an event.
    pub fn record(&self, event: AuditEvent) {
        let record = AuditRecord {
            at: self.clock.now_millis(),
            event,
        };
        let mut sink = self.sink.lock().expect("poisoned");
        match &mut *sink {
            Sink::Memory(v) => v.push(record),
            Sink::File {
                path,
                file,
                size,
                max_bytes,
                keep,
            } => {
                let mut line = match serde_json::to_vec(&record) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!("audit serialization failed: {e}");
                        return;
                    }
                };
                line.push(b'\n');
                if *size > 0 && *size + line.len() as u64 > *max_bytes {
                    *file = None;
                    if let Err(e) = rotate(path, *keep) {
                        tracing::error!("audit log rotation failed: {e}");
                    }
                    *size = 0;
                }
                if file.is_none() {
                    match open_append(path) {
                        Ok(f) => *file = Some(f),
                        Err(e) => {
                            tracing::error!("cannot open audit log: {e}");
                            return;
                        }
                    }
                }
                if let Some(f) = file.as_mut() {
                    match f.write_all(&line).and_then(|_| f.flush()) {
                        Ok(()) => *size += line.len() as u64,
                        Err(e) => tracing::error!("audit write failed: {e}"),
                    }
                }
            }
        }
    }

    /// The most recent `n` records (current file only for file logs).
    pub fn recent(&self, n: usize) -> Vec<AuditRecord> {
        let sink = self.sink.lock().expect("poisoned");
        match &*sink {
            Sink::Memory(v) => v[v.len().saturating_sub(n)..].to_vec(),
            Sink::File { path, .. } => {
                let Ok(f) = File::open(path) else {
                    return Vec::new();
                };
                let all: Vec<AuditRecord> = BufReader::new(f)
                    .lines()
                    .map_while(std::io::Result::ok)
                    .filter_map(|l| serde_json::from_str(&l).ok())
                    .collect();
                all[all.len().saturating_sub(n)..].to_vec()
            }
        }
    }
}

fn open_append(path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

fn rotated(path: &Path, i: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{i}"));
    PathBuf::from(s)
}

fn rotate(path: &Path, keep: usize) -> std::io::Result<()> {
    let _ = std::fs::remove_file(rotated(path, keep));
    for i in (1..keep).rev() {
        let from = rotated(path, i);
        if from.exists() {
            std::fs::rename(&from, rotated(path, i + 1))?;
        }
    }
    std::fs::rename(path, rotated(path, 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    #[test]
    fn xff_only_trusted_from_loopback() {
        let lo: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let lo6: SocketAddr = "[::1]:5000".parse().unwrap();
        let remote: SocketAddr = "198.51.100.7:5000".parse().unwrap();
        assert_eq!(
            client_ip_from_headers(lo, Some("203.0.113.9")),
            ("203.0.113.9".parse().unwrap(), true)
        );
        assert_eq!(
            client_ip_from_headers(lo6, Some("10.0.0.1, 203.0.113.9")),
            ("203.0.113.9".parse().unwrap(), true)
        );
        // Not loopback: header ignored.
        assert_eq!(
            client_ip_from_headers(remote, Some("203.0.113.9")),
            ("198.51.100.7".parse().unwrap(), false)
        );
        // Garbage header: fall back to peer.
        assert_eq!(
            client_ip_from_headers(lo, Some("not-an-ip")),
            (lo.ip(), false)
        );
        assert_eq!(client_ip_from_headers(lo, None), (lo.ip(), false));
    }

    #[test]
    fn redacts_secrets() {
        let cases = [
            (
                "export OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwx",
                "sk-abcdefghijklmnop",
            ),
            ("password=hunter2 now", "hunter2"),
            ("curl -H 'Authorization: Bearer abc.def.ghi'", "abc.def.ghi"),
            ("aws AKIAABCDEFGHIJKLMNOP", "AKIAABCDEFGHIJKLMNOP"),
            (
                "gh token ghp_abcdefghijklmnopqrstuvwxyz0123",
                "ghp_abcdefghijklmnopqrstuvwxyz0123",
            ),
            ("t=vca1.eyJhIjoxfQ.c2lnbmF0dXJl", "c2lnbmF0dXJl"),
            (
                "hex 0123456789abcdef0123456789abcdef01234567",
                "0123456789abcdef0123456789abcdef01234567",
            ),
        ];
        for (input, secret) in cases {
            let r = redact(input);
            assert!(!r.contains(secret), "{input} -> {r}");
            assert!(r.contains("[REDACTED]"), "{input} -> {r}");
        }
        assert_eq!(redact("ls -la /tmp"), "ls -la /tmp");
        assert!(redact(&"word ".repeat(500)).chars().count() <= MAX_DETAIL_CHARS + 1);
    }

    #[test]
    fn command_event_redacts_input_text() {
        let msg = ClientMsg::SendInput {
            session_id: "s1".into(),
            input: vca_core::Input::Text {
                text: "my token: supersecretvalue".into(),
                submit: true,
            },
        };
        let AuditEvent::Command { kind, detail, .. } = AuditEvent::command("dev_1", &msg) else {
            panic!()
        };
        assert_eq!(kind, "send_input");
        assert!(!detail.contains("supersecretvalue"), "{detail}");
        assert!(detail.contains("s1"));
    }

    #[test]
    fn file_log_appends_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit").join("audit.jsonl");
        let log = AuditLog::open(&path, 1024, 2, ManualClock::new(42)).unwrap();
        for i in 0..40 {
            log.record(AuditEvent::DeviceRevoked {
                device_id: format!("dev_{i}"),
            });
        }
        assert!(path.exists());
        assert!(rotated(&path, 1).exists());
        assert!(rotated(&path, 2).exists());
        assert!(!rotated(&path, 3).exists());
        assert!(std::fs::metadata(&path).unwrap().len() <= 1024);
        let recent = log.recent(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(
            recent[1].event,
            AuditEvent::DeviceRevoked {
                device_id: "dev_39".into()
            }
        );
        assert_eq!(recent[1].at, 42);
        // Lines are valid JSON with a flattened tag.
        let text = std::fs::read_to_string(&path).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["event"], "device_revoked");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Reopening appends.
        drop(log);
        let log = AuditLog::open(&path, 1024 * 1024, 2, ManualClock::new(43)).unwrap();
        log.record(AuditEvent::KillSwitchTripped { by: "test".into() });
        assert_eq!(log.recent(1)[0].at, 43);
    }
}
