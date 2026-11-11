//! Localized mail templates (Korean and English; other languages fall back
//! to English). Plain text plus a simple HTML part. Mails never contain a
//! clickable action link: what to do is described in words.

use serde::{Deserialize, Serialize};
use vca_core::UnixMillis;

use super::transport::{MailKind, OutgoingMail};

/// A change of the authenticator (2-step verification).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TotpChange {
    Setup,
    Reset,
    Disabled,
    BackupCodes,
}

/// How recovery was proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryProof {
    MailedCode,
    BackupCode,
}

/// What a mail says.
#[derive(Clone, PartialEq, Eq)]
pub enum Notice {
    Test,
    DevicePaired {
        device: String,
        at: UnixMillis,
        /// Client address as seen by the server (may be a tunnel's).
        ip: Option<String>,
    },
    AuthFailures {
        count: u32,
        since: UnixMillis,
        until: UnixMillis,
        /// Up to a few short lines such as "203.0.113.5: wrong code".
        samples: Vec<String>,
    },
    TotpChanged {
        change: TotpChange,
        at: UnixMillis,
    },
    RecoveryCode {
        code: String,
        minutes: u32,
    },
    RecoveryStarted {
        at: UnixMillis,
    },
    RecoveryCompleted {
        at: UnixMillis,
        proof: RecoveryProof,
    },
    MailSettingsChanged {
        at: UnixMillis,
        recipients: Vec<String>,
    },
}

impl std::fmt::Debug for Notice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Test => "Notice::Test",
            Self::DevicePaired { .. } => "Notice::DevicePaired",
            Self::AuthFailures { .. } => "Notice::AuthFailures",
            Self::TotpChanged { .. } => "Notice::TotpChanged",
            Self::RecoveryCode { .. } => "Notice::RecoveryCode",
            Self::RecoveryStarted { .. } => "Notice::RecoveryStarted",
            Self::RecoveryCompleted { .. } => "Notice::RecoveryCompleted",
            Self::MailSettingsChanged { .. } => "Notice::MailSettingsChanged",
        })
    }
}

impl Notice {
    pub fn kind(&self) -> MailKind {
        match self {
            Self::Test => MailKind::Test,
            Self::DevicePaired { .. } => MailKind::DevicePaired,
            Self::AuthFailures { .. } => MailKind::AuthFailures,
            Self::TotpChanged { .. } => MailKind::TotpChanged,
            Self::RecoveryCode { .. } => MailKind::RecoveryCode,
            Self::RecoveryStarted { .. } => MailKind::RecoveryStarted,
            Self::RecoveryCompleted { .. } => MailKind::RecoveryCompleted,
            Self::MailSettingsChanged { .. } => MailKind::MailSettingsChanged,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Ko,
    En,
}

fn lang(tag: &str) -> Lang {
    if tag.trim().to_ascii_lowercase().starts_with("ko") {
        Lang::Ko
    } else {
        Lang::En
    }
}

/// `2026-11-02 13:05 UTC`.
pub fn format_utc(ms: UnixMillis) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Device and computer names are printed, never interpreted: strip
/// control characters and bound the length.
fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Renders `notice` for `to` in `language`. `server` is this computer's
/// name as shown in the app.
pub fn render(notice: &Notice, language: &str, server: &str, to: &[String]) -> OutgoingMail {
    let l = lang(language);
    let server = clean(server, 64);
    let ko = l == Lang::Ko;
    let (subject, lines): (String, Vec<String>) = match notice {
        Notice::Test => (
            if ko {
                "테스트 메일".into()
            } else {
                "Test mail".into()
            },
            vec![if ko {
                format!(
                    "{server}의 Voice Command AI에서 보낸 테스트 메일입니다. 이 메일이 보이면 보안 알림 메일이 잘 설정된 것입니다."
                )
            } else {
                format!(
                    "This is a test mail from Voice Command AI on {server}. If you can read it, security notifications are set up correctly."
                )
            }],
        ),
        Notice::DevicePaired { device, at, ip } => {
            let device = clean(device, 64);
            let when = format_utc(*at);
            let ip = ip.as_deref().map(|i| clean(i, 64));
            let mut lines = if ko {
                vec![
                    format!("{server}에 새 기기가 연결되었습니다."),
                    String::new(),
                    format!("기기 이름: {device}"),
                    format!("시각: {when}"),
                ]
            } else {
                vec![
                    format!("A new device was paired with {server}."),
                    String::new(),
                    format!("Device name: {device}"),
                    format!("Time: {when}"),
                ]
            };
            if let Some(ip) = ip {
                lines.push(if ko {
                    format!("접속 주소: {ip} (터널을 거친 경우 터널 주소일 수 있습니다)")
                } else {
                    format!("Address: {ip} (may be a tunnel's address)")
                });
            }
            lines.push(String::new());
            lines.push(if ko {
                "본인이 아니면: 이 컴퓨터에서 Voice Command AI 앱을 열고 설정 > 연결 > 연결된 기기에서 이 기기를 바로 해제하세요. 급하면 설정 > 연결 > 보안의 '원격 접속 즉시 차단'을 누르세요.".into()
            } else {
                "If this was not you: open the Voice Command AI app on this computer and remove the device under Settings > Connection > Paired devices. If it is urgent, use \"Block remote access now\" under Settings > Connection > Security.".into()
            });
            (
                if ko {
                    format!("새 기기 연결: {device}")
                } else {
                    format!("New device paired: {device}")
                },
                lines,
            )
        }
        Notice::AuthFailures {
            count,
            since,
            until,
            samples,
        } => {
            let mut lines = if ko {
                vec![
                    format!(
                        "{server}에서 연결·인증 실패가 {count}번 있었습니다 ({} ~ {}).",
                        format_utc(*since),
                        format_utc(*until)
                    ),
                    String::new(),
                ]
            } else {
                vec![
                    format!(
                        "{server} saw {count} failed pairing or sign-in attempts ({} to {}).",
                        format_utc(*since),
                        format_utc(*until)
                    ),
                    String::new(),
                ]
            };
            for s in samples.iter().take(5) {
                lines.push(format!("- {}", clean(s, 120)));
            }
            lines.push(String::new());
            lines.push(if ko {
                "본인이 시도한 것이 아니면: 앱의 설정 > 연결 > 연결된 기기를 확인하고, 모르는 기기는 해제하세요. 이 알림은 15분에 한 번까지만 보냅니다.".into()
            } else {
                "If these were not your attempts: check Settings > Connection > Paired devices in the app and remove any device you do not know. This alert is sent at most once every 15 minutes.".into()
            });
            (
                if ko {
                    format!("인증 실패 {count}건")
                } else {
                    format!("{count} failed sign-in attempts")
                },
                lines,
            )
        }
        Notice::TotpChanged { change, at } => {
            let what_ko = match change {
                TotpChange::Setup => "2단계 인증(인증 앱)이 설정되었습니다.",
                TotpChange::Reset => "2단계 인증(인증 앱)이 새 앱으로 바뀌었습니다.",
                TotpChange::Disabled => "2단계 인증(인증 앱)이 꺼졌습니다.",
                TotpChange::BackupCodes => {
                    "백업 코드가 새로 만들어졌습니다. 예전 백업 코드는 더 이상 쓸 수 없습니다."
                }
            };
            let what_en = match change {
                TotpChange::Setup => "2-step verification (authenticator app) was set up.",
                TotpChange::Reset => "2-step verification was moved to a new authenticator app.",
                TotpChange::Disabled => "2-step verification (authenticator app) was turned off.",
                TotpChange::BackupCodes => {
                    "New backup codes were created. The old backup codes no longer work."
                }
            };
            (
                if ko {
                    "2단계 인증 변경".into()
                } else {
                    "2-step verification changed".into()
                },
                vec![
                    if ko {
                        format!("{server}: {what_ko}")
                    } else {
                        format!("{server}: {what_en}")
                    },
                    if ko {
                        format!("시각: {}", format_utc(*at))
                    } else {
                        format!("Time: {}", format_utc(*at))
                    },
                    String::new(),
                    if ko {
                        "본인이 하지 않았다면 이 컴퓨터를 직접 확인하고, 앱의 설정 > 보안에서 상태를 점검하세요.".into()
                    } else {
                        "If you did not do this, check this computer in person and review Settings > Security in the app.".into()
                    },
                ],
            )
        }
        Notice::RecoveryCode { code, minutes } => (
            if ko {
                "복구 확인 코드".into()
            } else {
                "Recovery code".into()
            },
            vec![
                if ko {
                    format!("{server}의 2단계 인증 복구 코드입니다.")
                } else {
                    format!("Your 2-step verification recovery code for {server}:")
                },
                String::new(),
                format!("    {code}"),
                String::new(),
                if ko {
                    format!(
                        "{minutes}분 동안 한 번만 쓸 수 있습니다. 이 코드는 누구에게도 알려주지 마세요. 복구를 시작하지 않았다면 이 메일을 무시하고, 컴퓨터를 직접 확인하세요."
                    )
                } else {
                    format!(
                        "It works once, for {minutes} minutes. Never share it. If you did not start a recovery, ignore this mail and check the computer in person."
                    )
                },
            ],
        ),
        Notice::RecoveryStarted { at } => (
            if ko {
                "2단계 인증 복구 시작".into()
            } else {
                "Recovery started".into()
            },
            vec![
                if ko {
                    format!(
                        "{server}에서 2단계 인증 복구가 시작되었습니다 ({}).",
                        format_utc(*at)
                    )
                } else {
                    format!(
                        "A 2-step verification recovery was started on {server} ({}).",
                        format_utc(*at)
                    )
                },
                if ko {
                    "본인이 아니면 컴퓨터를 직접 확인하세요. 복구에는 이 컴퓨터에서의 본인 확인이 꼭 필요합니다.".into()
                } else {
                    "If this was not you, check the computer in person. Recovery always needs verification at the computer itself.".into()
                },
            ],
        ),
        Notice::RecoveryCompleted { at, proof } => (
            if ko {
                "2단계 인증 복구 완료".into()
            } else {
                "Recovery completed".into()
            },
            vec![
                if ko {
                    format!(
                        "{server}에서 2단계 인증 복구가 끝났습니다 ({}, {}).",
                        format_utc(*at),
                        match proof {
                            RecoveryProof::MailedCode => "메일 코드 사용",
                            RecoveryProof::BackupCode => "백업 코드 사용",
                        }
                    )
                } else {
                    format!(
                        "A 2-step verification recovery was completed on {server} ({}, {}).",
                        format_utc(*at),
                        match proof {
                            RecoveryProof::MailedCode => "with the mailed code",
                            RecoveryProof::BackupCode => "with a backup code",
                        }
                    )
                },
                if ko {
                    "이제 새 인증 앱을 등록할 수 있습니다. 본인이 아니면 컴퓨터를 직접 확인하세요."
                        .into()
                } else {
                    "A new authenticator app can now be set up. If this was not you, check the computer in person.".into()
                },
            ],
        ),
        Notice::MailSettingsChanged { at, recipients } => {
            let list = recipients
                .iter()
                .map(|r| clean(r, 254))
                .collect::<Vec<_>>()
                .join(", ");
            (
                if ko {
                    "알림 메일 설정 변경".into()
                } else {
                    "Mail settings changed".into()
                },
                vec![
                    if ko {
                        format!(
                            "{server}의 보안 알림 메일 설정이 바뀌었습니다 ({}).",
                            format_utc(*at)
                        )
                    } else {
                        format!(
                            "The security mail settings of {server} were changed ({}).",
                            format_utc(*at)
                        )
                    },
                    if ko {
                        format!("이제 받는 주소: {list}")
                    } else {
                        format!("Notifications now go to: {list}")
                    },
                    String::new(),
                    if ko {
                        "본인이 바꾸지 않았다면 컴퓨터를 직접 확인하고 앱의 설정 > 보안 > 메일을 점검하세요.".into()
                    } else {
                        "If you did not change this, check the computer in person and review Settings > Security > Mail in the app.".into()
                    },
                ],
            )
        }
    };
    let subject = format!("[VCA] {subject}");
    let footer = if ko {
        "Voice Command AI 보안 알림 · 이 메일에는 링크가 없습니다. 링크로 무언가를 하라는 메일은 보내지 않습니다."
    } else {
        "Voice Command AI security notification. This mail has no links; we never ask you to act through a link."
    };
    let mut text = lines.join("\n");
    text.push_str("\n\n-- \n");
    text.push_str(footer);
    text.push('\n');
    let mut html = String::from("<div style=\"font-family:sans-serif;line-height:1.5\">");
    for line in &lines {
        if line.is_empty() {
            html.push_str("<br>");
        } else if line.starts_with("    ") {
            html.push_str(&format!(
                "<p style=\"font-size:24px;letter-spacing:4px;font-family:monospace\"><b>{}</b></p>",
                escape_html(line.trim())
            ));
        } else {
            html.push_str(&format!("<p>{}</p>", escape_html(line)));
        }
    }
    html.push_str(&format!(
        "<hr><p style=\"color:#666;font-size:12px\">{}</p></div>",
        escape_html(footer)
    ));
    OutgoingMail {
        to: to.to_vec(),
        subject,
        text,
        html: Some(html),
        kind: notice.kind(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: UnixMillis = 1_793_624_700_000; // 2026-11-02 13:05 UTC

    fn to() -> Vec<String> {
        vec!["owner@example.com".into()]
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_utc(AT), "2026-11-02 13:05 UTC");
        assert_eq!(format_utc(951_782_400_000), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn device_paired_in_korean_and_english() {
        let n = Notice::DevicePaired {
            device: "Galaxy <S25>".into(),
            at: AT,
            ip: Some("203.0.113.5".into()),
        };
        let ko = render(&n, "ko-KR", "work-pc", &to());
        assert_eq!(ko.subject, "[VCA] 새 기기 연결: Galaxy <S25>");
        assert!(ko.text.contains("본인이 아니면"));
        assert!(ko.text.contains("203.0.113.5"));
        assert!(ko.text.contains("2026-11-02 13:05 UTC"));
        let html = ko.html.as_deref().unwrap();
        assert!(html.contains("Galaxy &lt;S25&gt;"));
        assert!(!html.contains("<S25>"));
        assert!(!html.contains("href"), "no links");
        assert_eq!(ko.kind, MailKind::DevicePaired);
        assert_eq!(ko.to, to());

        let en = render(&n, "en", "work-pc", &to());
        assert!(en.subject.starts_with("[VCA] New device paired"));
        assert!(en.text.contains("If this was not you"));
        // Unknown languages fall back to English.
        assert_eq!(render(&n, "ja", "work-pc", &to()).subject, en.subject);
    }

    #[test]
    fn every_notice_renders_in_both_languages() {
        let notices = [
            Notice::Test,
            Notice::AuthFailures {
                count: 7,
                since: AT,
                until: AT + 60_000,
                samples: vec!["203.0.113.5: wrong code".into()],
            },
            Notice::TotpChanged {
                change: TotpChange::Reset,
                at: AT,
            },
            Notice::RecoveryCode {
                code: "042917".into(),
                minutes: 10,
            },
            Notice::RecoveryStarted { at: AT },
            Notice::RecoveryCompleted {
                at: AT,
                proof: RecoveryProof::BackupCode,
            },
            Notice::MailSettingsChanged {
                at: AT,
                recipients: vec!["new@example.com".into()],
            },
        ];
        for n in &notices {
            for lang in ["ko", "en"] {
                let m = render(n, lang, "pc\u{7}name", &to());
                assert!(m.subject.starts_with("[VCA] "), "{n:?}");
                assert!(!m.text.contains('\u{7}'));
                assert!(m.text.contains("pcname"));
                assert!(!m.html.as_deref().unwrap().contains("href"));
            }
        }
        let code = render(&notices[3], "ko", "pc", &to());
        assert!(code.text.contains("042917"));
        assert!(code.text.contains("10분"));
        // Debug output never includes the code.
        assert!(!format!("{:?}", notices[3]).contains("042917"));
        assert!(!format!("{code:?}").contains("042917"));
    }
}
