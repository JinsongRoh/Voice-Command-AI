//! Security notifications: which events the owner wants by mail, and the
//! digest of failed attempts (at most one mail per 15 minutes).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use vca_core::UnixMillis;
use vca_net::SharedClock;

use super::template::{Notice, RecoveryProof, TotpChange, render};
use super::transport::MailError;
use super::{MailForm, MailService};

/// At most one failure digest per window.
pub const DIGEST_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Failures that arrive close together go into one mail.
pub const DIGEST_GATHER: Duration = Duration::from_secs(60);
const MAX_SAMPLES: usize = 5;

/// Something the owner may want to hear about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityEvent {
    DevicePaired { device: String, ip: Option<String> },
    TotpChanged(TotpChange),
    RecoveryStarted,
    RecoveryCompleted(RecoveryProof),
}

#[derive(Default)]
struct Digest {
    count: u32,
    first: Option<UnixMillis>,
    last: UnixMillis,
    samples: Vec<String>,
    last_sent: Option<UnixMillis>,
    scheduled: bool,
}

impl Digest {
    fn due_at(&self) -> Option<UnixMillis> {
        let first = self.first?;
        let gathered = first + DIGEST_GATHER.as_millis() as u64;
        let allowed = self
            .last_sent
            .map_or(0, |t| t + DIGEST_WINDOW.as_millis() as u64);
        Some(gathered.max(allowed))
    }
}

pub struct Notifier {
    mail: Arc<MailService>,
    server_name: String,
    clock: SharedClock,
    digest: Mutex<Digest>,
}

impl Notifier {
    pub fn new(mail: Arc<MailService>, server_name: String, clock: SharedClock) -> Self {
        Self {
            mail,
            server_name,
            clock,
            digest: Mutex::new(Digest::default()),
        }
    }

    pub fn mail(&self) -> &Arc<MailService> {
        &self.mail
    }

    fn wants(form: &MailForm, event: &SecurityEvent) -> bool {
        let n = form.notifications;
        n.enabled
            && match event {
                SecurityEvent::DevicePaired { .. } => n.device_paired,
                SecurityEvent::TotpChanged(_) => n.totp_changes,
                SecurityEvent::RecoveryStarted | SecurityEvent::RecoveryCompleted(_) => n.recovery,
            }
    }

    /// Renders `notice` for the owner's addresses (or `to`) and sends it
    /// now. Returns the addresses it went to.
    pub async fn send(
        &self,
        notice: Notice,
        to: Option<Vec<String>>,
    ) -> Result<Vec<String>, MailError> {
        let form = self.mail.form();
        if !self.mail.configured() {
            return Err(MailError::NotConfigured);
        }
        let to = to.unwrap_or_else(|| form.recipients.clone());
        if to.is_empty() {
            return Err(MailError::NotConfigured);
        }
        let mail = render(&notice, &form.language, &self.server_name, &to);
        let transport = self.mail.transport()?;
        transport.send(&mail).await?;
        Ok(to)
    }

    /// Sends a notification for `event` in the background when the owner
    /// wants it and mail works. Failures are logged (without content).
    pub fn event(self: &Arc<Self>, event: SecurityEvent) {
        let form = self.mail.form();
        if !Self::wants(&form, &event) || !self.mail.configured() {
            return;
        }
        let at = self.clock.now_millis();
        let notice = match event {
            SecurityEvent::DevicePaired { device, ip } => Notice::DevicePaired { device, at, ip },
            SecurityEvent::TotpChanged(change) => Notice::TotpChanged { change, at },
            SecurityEvent::RecoveryStarted => Notice::RecoveryStarted { at },
            SecurityEvent::RecoveryCompleted(proof) => Notice::RecoveryCompleted { at, proof },
        };
        let this = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(err) = this.send(notice, None).await {
                tracing::warn!(%err, "security notification not sent");
            }
        });
    }

    /// The mail settings changed: tell the old addresses and the new ones.
    ///
    /// The old addresses hear it over the old settings, whatever the new
    /// ones are: a new relay or server may be the attacker's and would keep
    /// the warning to itself. A change of where mail goes is always reported
    /// there, also when notifications are turned off.
    pub async fn mail_settings_changed(&self, before: &MailForm) -> Result<(), MailError> {
        let after = self.mail.form();
        let moved = super::delivery_changed(before, &after);
        let wanted = moved
            || after.notifications.enabled && after.notifications.mail_changes
            || before.notifications.enabled && before.notifications.mail_changes;
        if !wanted {
            return Ok(());
        }
        let at = self.clock.now_millis();
        let notice = Notice::MailSettingsChanged {
            at,
            recipients: after.recipients.clone(),
        };
        let mut result = Ok(());
        if before.mode.is_some() && !before.recipients.is_empty() {
            let old = render(
                &notice,
                &before.language,
                &self.server_name,
                &before.recipients,
            );
            result = match self.mail.transport_for(before) {
                Ok(t) => t.send(&old).await,
                Err(e) => Err(e),
            };
        }
        let fresh: Vec<String> = after
            .recipients
            .iter()
            .filter(|r| !before.recipients.contains(r))
            .cloned()
            .collect();
        if !fresh.is_empty() && self.mail.configured() {
            let mail = render(&notice, &after.language, &self.server_name, &fresh);
            let sent = match self.mail.transport() {
                Ok(t) => t.send(&mail).await,
                Err(e) => Err(e),
            };
            if result.is_ok() {
                result = sent;
            }
        }
        result
    }

    /// A sending secret is about to change or go: the owner hears it now,
    /// while the current settings can still deliver. Afterwards they may not.
    pub async fn secret_changing(&self) -> Result<(), MailError> {
        let notice = Notice::MailSettingsChanged {
            at: self.clock.now_millis(),
            recipients: self.mail.form().recipients,
        };
        self.send(notice, None).await.map(|_| ())
    }

    /// Records a failed pairing, TOTP or sign-in attempt. `detail` is a
    /// short line such as "203.0.113.5: wrong code" (no secrets).
    pub fn auth_failure(self: &Arc<Self>, detail: &str) {
        let form = self.mail.form();
        let wanted = form.notifications.enabled && form.notifications.auth_failures;
        if !wanted || !self.mail.configured() {
            return;
        }
        let now = self.clock.now_millis();
        let schedule = {
            let mut d = self.digest.lock().expect("poisoned");
            record(&mut d, now, detail);
            if d.scheduled {
                None
            } else {
                d.scheduled = true;
                d.due_at()
            }
        };
        if let Some(due) = schedule {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                let wait = due.saturating_sub(this.clock.now_millis());
                tokio::time::sleep(Duration::from_millis(wait)).await;
                let notice = {
                    let mut d = this.digest.lock().expect("poisoned");
                    d.scheduled = false;
                    take_due(&mut d, this.clock.now_millis())
                };
                if let Some(notice) = notice {
                    if let Err(err) = this.send(notice, None).await {
                        tracing::warn!(%err, "failure digest not sent");
                    }
                }
            });
        }
    }

    /// Test hook: records a failure without scheduling.
    #[cfg(test)]
    fn record_failure(&self, detail: &str) {
        let now = self.clock.now_millis();
        record(&mut self.digest.lock().unwrap(), now, detail);
    }

    /// Test hook: the digest due now, if any.
    #[cfg(test)]
    fn due_digest(&self) -> Option<Notice> {
        take_due(&mut self.digest.lock().unwrap(), self.clock.now_millis())
    }
}

fn record(d: &mut Digest, now: UnixMillis, detail: &str) {
    d.count = d.count.saturating_add(1);
    d.first.get_or_insert(now);
    d.last = now;
    let detail: String = detail
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    if d.samples.len() < MAX_SAMPLES && !d.samples.contains(&detail) {
        d.samples.push(detail);
    }
}

fn take_due(d: &mut Digest, now: UnixMillis) -> Option<Notice> {
    let due = d.due_at()?;
    if now < due {
        return None;
    }
    let notice = Notice::AuthFailures {
        count: d.count,
        since: d.first.unwrap_or(now),
        until: d.last,
        samples: std::mem::take(&mut d.samples),
    };
    d.count = 0;
    d.first = None;
    d.last_sent = Some(now);
    Some(notice)
}

#[cfg(test)]
mod tests {
    use super::super::transport::{MailKind, MemoryTransport};
    use super::super::{MailMode, MailPreset, SecretKind, TlsMode};
    use super::*;
    use vca_net::{ManualClock, MemoryKeyStore};

    fn configured() -> (Arc<ManualClock>, Arc<Notifier>, Arc<MemoryTransport>) {
        let clock = ManualClock::new(1_793_624_700_000);
        let mail = Arc::new(MailService::in_memory(
            MailPreset::default(),
            Arc::new(MemoryKeyStore::new()),
        ));
        mail.save(&MailForm {
            mode: Some(MailMode::Smtp),
            smtp_host: Some("smtp.example.com".into()),
            smtp_tls: TlsMode::Starttls,
            smtp_username: Some("u".into()),
            sender_address: Some("vca@example.com".into()),
            recipients: vec!["owner@example.com".into()],
            language: "ko".into(),
            ..MailForm::default()
        })
        .unwrap();
        mail.set_secret(SecretKind::SmtpPassword, Some("pw"))
            .unwrap();
        let t = Arc::new(MemoryTransport::default());
        mail.set_transport_override(t.clone());
        let n = Arc::new(Notifier::new(mail, "work-pc".into(), clock.clone()));
        (clock, n, t)
    }

    #[test]
    fn failures_are_digested_at_most_every_15_minutes() {
        let (clock, n, _) = configured();
        assert!(n.due_digest().is_none());
        n.record_failure("203.0.113.5: wrong code");
        n.record_failure("203.0.113.5: wrong code");
        n.record_failure("198.51.100.7: unknown device");
        assert!(n.due_digest().is_none(), "gathering");
        clock.advance_secs(60);
        match n.due_digest() {
            Some(Notice::AuthFailures { count, samples, .. }) => {
                assert_eq!(count, 3);
                assert_eq!(samples.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        // More failures right after: held until 15 minutes after the last mail.
        n.record_failure("x");
        clock.advance_secs(120);
        assert!(n.due_digest().is_none());
        clock.advance_secs(13 * 60 - 1);
        assert!(n.due_digest().is_none());
        clock.advance_secs(1);
        assert!(matches!(
            n.due_digest(),
            Some(Notice::AuthFailures { count: 1, .. })
        ));
        assert!(n.due_digest().is_none());
    }

    #[tokio::test]
    async fn events_respect_preferences() {
        let (_, n, t) = configured();
        n.event(SecurityEvent::DevicePaired {
            device: "Phone".into(),
            ip: None,
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let sent = t.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].kind, MailKind::DevicePaired);
        assert!(sent[0].subject.contains("새 기기 연결"));

        let mut f = n.mail().form();
        f.notifications.device_paired = false;
        n.mail().save(&f).unwrap();
        n.event(SecurityEvent::DevicePaired {
            device: "Phone".into(),
            ip: None,
        });
        n.event(SecurityEvent::TotpChanged(TotpChange::Setup));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let sent = t.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].kind, MailKind::TotpChanged);
    }

    #[tokio::test]
    async fn mail_change_reaches_old_and_new_addresses() {
        let (_, n, t) = configured();
        let before = n.mail().form();
        let mut f = before.clone();
        f.recipients = vec!["new@example.com".into()];
        n.mail().save(&f).unwrap();
        n.mail_settings_changed(&before).await.unwrap();
        let sent = t.sent();
        assert_eq!(sent.len(), 2);
        // The old address first, on its own: it goes over the old settings.
        assert_eq!(sent[0].to, ["owner@example.com"]);
        assert_eq!(sent[1].to, ["new@example.com"]);
        assert_eq!(sent[0].kind, MailKind::MailSettingsChanged);
        assert_eq!(sent[1].kind, MailKind::MailSettingsChanged);
    }

    #[tokio::test]
    async fn nothing_is_sent_without_mail() {
        let clock = ManualClock::new(0);
        let mail = Arc::new(MailService::in_memory(
            MailPreset::default(),
            Arc::new(MemoryKeyStore::new()),
        ));
        let n = Arc::new(Notifier::new(mail, "pc".into(), clock));
        assert_eq!(
            n.send(Notice::Test, None).await,
            Err(MailError::NotConfigured)
        );
        n.event(SecurityEvent::RecoveryCompleted(RecoveryProof::BackupCode));
        n.auth_failure("x");
    }
}
