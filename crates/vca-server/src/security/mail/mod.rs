//! Mail for security notifications and account recovery.
//!
//! Two transports sit behind [`transport::MailTransport`]:
//!
//! * `smtp`: direct SMTP (STARTTLS or implicit TLS; plain text only to a
//!   loopback host), with the password in the server credential store.
//! * `relay`: `POST <relay_url>/v1/notify/mail` with the JSON
//!   `{to, subject, text, html?, kind}`. It authenticates like the speech
//!   engines ([`vca_speech::auth::EngineAuth`]): the relay key in a custom
//!   header (`api_key_header`), plus an optional gateway token sent as
//!   `Authorization: Bearer`. This computer never holds a mail password,
//!   and the relay decides the sender.
//!
//! Settings are layered: an install-time preset file
//! (`<config dir>/mail-preset.toml`, see [`PRESET_FILE`]) supplies defaults,
//! and the owner's own changes (`<data dir>/security/mail.json`) override
//! them. Every change goes through local verification. Secrets are never in
//! either file: `vca mail password`, `vca mail relay-key` and
//! `vca mail gateway-token` store them in the credential store.

pub mod notify;
pub mod template;
pub mod transport;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vca_net::KeyStore;

pub use transport::{MailError, MailTransport, OutgoingMail};

/// Install-time preset, next to `config.toml`.
pub const PRESET_FILE: &str = "mail-preset.toml";
/// Overrides the preset path.
pub const PRESET_ENV: &str = "VCA_MAIL_PRESET";
/// The owner's settings below the data directory.
pub const SETTINGS_FILE: &str = "mail.json";
/// Most owner addresses.
pub const MAX_RECIPIENTS: usize = 5;

/// Credential-store names.
pub const SECRET_SMTP_PASSWORD: &str = "smtp-password";
pub const SECRET_RELAY_KEY: &str = "relay-key";
pub const SECRET_GATEWAY_TOKEN: &str = "gateway-token";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailMode {
    Smtp,
    Relay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// Plain connection upgraded with STARTTLS (required). Port 587.
    #[default]
    Starttls,
    /// TLS from the first byte. Port 465.
    Tls,
    /// No encryption: only accepted for a loopback host (a local relay).
    None,
}

impl TlsMode {
    pub fn default_port(self) -> u16 {
        match self {
            Self::Starttls => 587,
            Self::Tls => 465,
            Self::None => 25,
        }
    }
}

/// Which secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    SmtpPassword,
    RelayKey,
    GatewayToken,
}

impl SecretKind {
    pub fn store_name(self) -> &'static str {
        match self {
            Self::SmtpPassword => SECRET_SMTP_PASSWORD,
            Self::RelayKey => SECRET_RELAY_KEY,
            Self::GatewayToken => SECRET_GATEWAY_TOKEN,
        }
    }
}

/// Which notifications the owner wants. On by default once mail works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationPrefs {
    pub enabled: bool,
    pub device_paired: bool,
    pub auth_failures: bool,
    pub totp_changes: bool,
    pub recovery: bool,
    pub mail_changes: bool,
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            device_paired: true,
            auth_failures: true,
            totp_changes: true,
            recovery: true,
            mail_changes: true,
        }
    }
}

/// The install-time preset (`mail-preset.toml`). Every key is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MailPreset {
    pub mode: Option<MailMode>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_tls: Option<TlsMode>,
    pub smtp_username: Option<String>,
    pub relay_url: Option<String>,
    pub api_key_header: Option<String>,
    /// Environment variable holding the relay gateway token (else the one
    /// stored with `vca mail gateway-token`).
    pub gateway_token_env: Option<String>,
    pub sender_name: Option<String>,
    pub sender_address: Option<String>,
    /// When set, every owner address must be in one of these domains.
    pub allowed_recipient_domains: Vec<String>,
    pub recipients: Vec<String>,
    pub language: Option<String>,
}

/// The settings form (what the owner edits; also the effective view).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailForm {
    pub mode: Option<MailMode>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_tls: TlsMode,
    pub smtp_username: Option<String>,
    pub relay_url: Option<String>,
    pub api_key_header: Option<String>,
    pub sender_name: Option<String>,
    pub sender_address: Option<String>,
    pub recipients: Vec<String>,
    pub language: String,
    pub notifications: NotificationPrefs,
}

/// The owner's changes over the preset (`mail.json`). `None` inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Overrides {
    /// `Some(None)`: mail turned off although the preset sets a mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<Option<MailMode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    smtp_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    smtp_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    smtp_tls: Option<TlsMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    smtp_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relay_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recipients: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    notifications: NotificationPrefs,
}

/// What the settings screen shows (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailView {
    #[serde(flatten)]
    pub form: MailForm,
    pub allowed_recipient_domains: Vec<String>,
    /// Fields whose value comes from the install preset.
    pub preset: Vec<String>,
    pub password_set: bool,
    pub relay_key_set: bool,
    pub gateway_token_set: bool,
    pub configured: bool,
    /// What is still needed before mail can be sent.
    pub missing: Vec<String>,
}

/// A rejected setting; `field` names it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct InvalidSetting {
    pub field: &'static str,
    pub message: String,
    /// An owner address outside `allowed_recipient_domains`.
    pub not_allowed: bool,
}

fn invalid(field: &'static str, message: impl Into<String>) -> InvalidSetting {
    InvalidSetting {
        field,
        message: message.into(),
        not_allowed: false,
    }
}

fn nonempty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A plausible mail address (parsed like the SMTP library does).
pub fn valid_address(addr: &str) -> bool {
    addr.len() <= 254
        && !addr.chars().any(|c| c.is_whitespace() || c.is_control())
        && lettre::Address::from_str(addr).is_ok()
}

fn domain_of(addr: &str) -> String {
    addr.rsplit_once('@')
        .map(|(_, d)| d.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

fn domain_allowed(addr: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    let domain = domain_of(addr);
    allowed.iter().any(|a| {
        let a = a.trim().trim_start_matches('@').to_ascii_lowercase();
        domain == a || domain.ends_with(&format!(".{a}"))
    })
}

/// `localhost` or a loopback IP literal.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Masks an address for display in replies: `o***@example.com`.
pub fn mask_address(addr: &str) -> String {
    match addr.split_once('@') {
        Some((local, domain)) => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".into(),
    }
}

/// Mail settings, secrets and the transport built from them.
pub struct MailService {
    preset: MailPreset,
    settings_path: Option<PathBuf>,
    overrides: std::sync::Mutex<Overrides>,
    secrets: Arc<dyn KeyStore>,
    /// Test hook: replaces the transport built from the settings.
    transport_override: std::sync::Mutex<Option<Arc<dyn MailTransport>>>,
}

impl MailService {
    /// Loads the preset (`preset_path`, if it exists) and the owner's
    /// settings (`data_dir/security/mail.json`).
    pub fn open(
        preset_path: Option<&Path>,
        data_dir: Option<&Path>,
        secrets: Arc<dyn KeyStore>,
    ) -> anyhow::Result<Self> {
        let preset = match preset_path {
            Some(p) => load_preset(p)?,
            None => MailPreset::default(),
        };
        let settings_path = data_dir.map(|d| d.join("security").join(SETTINGS_FILE));
        let overrides = match &settings_path {
            Some(p) => match std::fs::read(p) {
                Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    tracing::warn!(path = %p.display(), "ignoring unreadable mail settings: {e}");
                    Overrides::default()
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Overrides::default(),
                Err(e) => return Err(e.into()),
            },
            None => Overrides::default(),
        };
        let service = Self {
            preset,
            settings_path,
            overrides: std::sync::Mutex::new(overrides),
            secrets,
            transport_override: std::sync::Mutex::new(None),
        };
        service
            .check_preset()
            .map_err(|e| anyhow::anyhow!("invalid mail preset: {e}"))?;
        Ok(service)
    }

    /// Holds the preset's values to the rules a saved form must meet. A
    /// preset may be partial: only what it sets is checked.
    fn check_preset(&self) -> Result<(), InvalidSetting> {
        let p = &self.preset;
        let base = MailForm {
            recipients: p.recipients.clone(),
            language: p.language.clone().unwrap_or_default(),
            sender_name: p.sender_name.clone(),
            ..MailForm::default()
        };
        self.validate(&base)?;
        if p.relay_url.is_some() {
            self.validate(&MailForm {
                mode: Some(MailMode::Relay),
                relay_url: p.relay_url.clone(),
                api_key_header: p.api_key_header.clone(),
                ..base.clone()
            })?;
        }
        if p.smtp_host.is_some() {
            self.validate(&MailForm {
                mode: Some(MailMode::Smtp),
                smtp_host: p.smtp_host.clone(),
                smtp_port: p.smtp_port,
                smtp_tls: p.smtp_tls.unwrap_or_default(),
                smtp_username: p.smtp_username.clone(),
                sender_address: p
                    .sender_address
                    .clone()
                    .or_else(|| Some("preset@example.com".to_owned())),
                ..base
            })?;
        }
        Ok(())
    }

    /// Volatile service for tests.
    pub fn in_memory(preset: MailPreset, secrets: Arc<dyn KeyStore>) -> Self {
        Self {
            preset,
            settings_path: None,
            overrides: std::sync::Mutex::new(Overrides::default()),
            secrets,
            transport_override: std::sync::Mutex::new(None),
        }
    }

    /// Sends through `transport` instead of SMTP/relay (tests).
    pub fn set_transport_override(&self, transport: Arc<dyn MailTransport>) {
        *self.transport_override.lock().expect("poisoned") = Some(transport);
    }

    pub fn preset(&self) -> &MailPreset {
        &self.preset
    }

    /// The effective settings form.
    pub fn form(&self) -> MailForm {
        let o = self.overrides.lock().expect("poisoned").clone();
        let p = &self.preset;
        let mode = match o.mode {
            Some(m) => m,
            None => p.mode,
        };
        let smtp_tls = o.smtp_tls.or(p.smtp_tls).unwrap_or_default();
        MailForm {
            mode,
            smtp_host: o.smtp_host.or_else(|| p.smtp_host.clone()),
            smtp_port: o.smtp_port.or(p.smtp_port),
            smtp_tls,
            smtp_username: o.smtp_username.or_else(|| p.smtp_username.clone()),
            relay_url: o.relay_url.or_else(|| p.relay_url.clone()),
            api_key_header: o.api_key_header.or_else(|| p.api_key_header.clone()),
            sender_name: o.sender_name.or_else(|| p.sender_name.clone()),
            sender_address: o.sender_address.or_else(|| p.sender_address.clone()),
            recipients: o.recipients.unwrap_or_else(|| p.recipients.clone()),
            language: o
                .language
                .or_else(|| p.language.clone())
                .unwrap_or_else(|| "en".into()),
            notifications: o.notifications,
        }
    }

    /// Is a secret of this kind stored?
    pub fn has_secret(&self, kind: SecretKind) -> bool {
        self.secret(kind).is_some()
    }

    fn secret(&self, kind: SecretKind) -> Option<String> {
        match self.secrets.load(kind.store_name()) {
            Ok(Some(bytes)) => std::str::from_utf8(&bytes)
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(%err, "cannot read a mail credential");
                None
            }
        }
    }

    fn gateway_token(&self) -> Option<String> {
        self.preset
            .gateway_token_env
            .as_deref()
            .and_then(|v| std::env::var(v).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| self.secret(SecretKind::GatewayToken))
    }

    /// Stores (`Some`) or deletes (`None`) a secret.
    pub fn set_secret(&self, kind: SecretKind, value: Option<&str>) -> anyhow::Result<()> {
        match value.map(str::trim) {
            Some("") => anyhow::bail!("empty credential"),
            Some(v) => {
                if v.len() > 4096 || v.chars().any(|c| c.is_control()) {
                    anyhow::bail!("the credential contains invalid characters");
                }
                self.secrets.save(kind.store_name(), v.as_bytes())?;
            }
            None => self.secrets.delete(kind.store_name())?,
        }
        Ok(())
    }

    /// What is still needed to send mail (field names).
    fn missing(&self, form: &MailForm) -> Vec<String> {
        let mut out = Vec::new();
        let mut need = |ok: bool, name: &str| {
            if !ok {
                out.push(name.to_owned());
            }
        };
        match form.mode {
            None => need(false, "mode"),
            Some(MailMode::Smtp) => {
                need(nonempty(&form.smtp_host).is_some(), "smtp_host");
                need(nonempty(&form.sender_address).is_some(), "sender_address");
                if nonempty(&form.smtp_username).is_some() {
                    need(
                        self.secret(SecretKind::SmtpPassword).is_some(),
                        "smtp_password",
                    );
                }
            }
            Some(MailMode::Relay) => {
                need(nonempty(&form.relay_url).is_some(), "relay_url");
                need(self.secret(SecretKind::RelayKey).is_some(), "relay_key");
            }
        }
        need(!form.recipients.is_empty(), "recipients");
        out
    }

    pub fn configured(&self) -> bool {
        self.missing(&self.form()).is_empty()
    }

    /// The settings screen (no secrets).
    pub fn view(&self) -> MailView {
        let form = self.form();
        let o = self.overrides.lock().expect("poisoned").clone();
        let p = &self.preset;
        let mut preset = Vec::new();
        let mut from_preset = |name: &str, preset_has: bool, overridden: bool| {
            if preset_has && !overridden {
                preset.push(name.to_owned());
            }
        };
        from_preset("mode", p.mode.is_some(), o.mode.is_some());
        from_preset("smtp_host", p.smtp_host.is_some(), o.smtp_host.is_some());
        from_preset("smtp_port", p.smtp_port.is_some(), o.smtp_port.is_some());
        from_preset("smtp_tls", p.smtp_tls.is_some(), o.smtp_tls.is_some());
        from_preset(
            "smtp_username",
            p.smtp_username.is_some(),
            o.smtp_username.is_some(),
        );
        from_preset("relay_url", p.relay_url.is_some(), o.relay_url.is_some());
        from_preset(
            "api_key_header",
            p.api_key_header.is_some(),
            o.api_key_header.is_some(),
        );
        from_preset(
            "sender_name",
            p.sender_name.is_some(),
            o.sender_name.is_some(),
        );
        from_preset(
            "sender_address",
            p.sender_address.is_some(),
            o.sender_address.is_some(),
        );
        from_preset(
            "recipients",
            !p.recipients.is_empty(),
            o.recipients.is_some(),
        );
        if !p.allowed_recipient_domains.is_empty() {
            preset.push("allowed_recipient_domains".into());
        }
        let missing = self.missing(&form);
        MailView {
            configured: missing.is_empty(),
            missing,
            allowed_recipient_domains: p.allowed_recipient_domains.clone(),
            preset,
            password_set: self.secret(SecretKind::SmtpPassword).is_some(),
            relay_key_set: self.secret(SecretKind::RelayKey).is_some(),
            gateway_token_set: self.gateway_token().is_some(),
            form,
        }
    }

    /// Checks a form without saving it.
    pub fn validate(&self, form: &MailForm) -> Result<MailForm, InvalidSetting> {
        let mut f = form.clone();
        let trim = |s: &Option<String>| nonempty(s);
        f.smtp_host = trim(&f.smtp_host);
        f.smtp_username = trim(&f.smtp_username);
        f.relay_url = trim(&f.relay_url);
        f.api_key_header = trim(&f.api_key_header);
        f.sender_name = trim(&f.sender_name);
        f.sender_address = trim(&f.sender_address);
        f.recipients = f
            .recipients
            .iter()
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
            .collect();
        f.recipients.dedup();
        let lang = f.language.trim().to_ascii_lowercase();
        f.language = if lang.is_empty() { "en".into() } else { lang };
        if f.language.len() > 16
            || !f
                .language
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(invalid("language", "use a language tag such as ko or en"));
        }

        if let Some(name) = &f.sender_name {
            if name.chars().count() > 80 || name.chars().any(|c| c.is_control()) {
                return Err(invalid("sender_name", "at most 80 printable characters"));
            }
        }
        if f.recipients.len() > MAX_RECIPIENTS {
            return Err(invalid(
                "recipients",
                format!("at most {MAX_RECIPIENTS} addresses"),
            ));
        }
        for r in &f.recipients {
            if !valid_address(r) {
                return Err(invalid(
                    "recipients",
                    format!("{r:?} is not a mail address"),
                ));
            }
            if !domain_allowed(r, &self.preset.allowed_recipient_domains) {
                return Err(InvalidSetting {
                    field: "recipients",
                    message: format!(
                        "{r:?} is not allowed here; use an address in {}",
                        self.preset.allowed_recipient_domains.join(", ")
                    ),
                    not_allowed: true,
                });
            }
        }
        match f.mode {
            None => {}
            Some(MailMode::Smtp) => {
                let host = f
                    .smtp_host
                    .as_deref()
                    .ok_or_else(|| invalid("smtp_host", "enter the mail server name"))?;
                if host.len() > 253
                    || host.contains(['/', ' ', '@', ':']) && !host.starts_with('[')
                    || host.chars().any(|c| c.is_control())
                {
                    return Err(invalid(
                        "smtp_host",
                        "enter only the server name, e.g. smtp.example.com",
                    ));
                }
                if f.smtp_tls == TlsMode::None && !is_loopback_host(host) {
                    return Err(invalid(
                        "smtp_tls",
                        "an unencrypted connection is only allowed to this computer (localhost)",
                    ));
                }
                if f.smtp_port == Some(0) {
                    return Err(invalid("smtp_port", "enter a port between 1 and 65535"));
                }
                let sender = f
                    .sender_address
                    .as_deref()
                    .ok_or_else(|| invalid("sender_address", "enter the sender address"))?;
                if !valid_address(sender) {
                    return Err(invalid("sender_address", "not a mail address"));
                }
                if let Some(u) = &f.smtp_username {
                    if u.len() > 254 || u.chars().any(|c| c.is_control()) {
                        return Err(invalid("smtp_username", "invalid user name"));
                    }
                }
            }
            Some(MailMode::Relay) => {
                let url = f
                    .relay_url
                    .as_deref()
                    .ok_or_else(|| invalid("relay_url", "enter the relay URL"))?;
                let parsed = reqwest::Url::parse(url)
                    .map_err(|_| invalid("relay_url", "not a valid URL"))?;
                let host = parsed.host_str().unwrap_or_default();
                match parsed.scheme() {
                    "https" => {}
                    "http" if is_loopback_host(host) => {}
                    _ => {
                        return Err(invalid(
                            "relay_url",
                            "use https:// (http:// only for this computer)",
                        ));
                    }
                }
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(invalid(
                        "relay_url",
                        "do not put credentials in the URL; use vca mail relay-key",
                    ));
                }
                if let Some(h) = &f.api_key_header {
                    vca_speech::auth::check_key_header(h)
                        .map_err(|e| invalid("api_key_header", e))?;
                }
            }
        }
        Ok(f)
    }

    /// Validates and saves `form` (only differences from the preset are
    /// stored). Returns the previous effective form.
    pub fn save(&self, form: &MailForm) -> Result<MailForm, SaveError> {
        let f = self.validate(form).map_err(SaveError::Invalid)?;
        let before = self.form();
        let p = &self.preset;
        fn diff<T: PartialEq + Clone>(value: &Option<T>, preset: &Option<T>) -> Option<T> {
            if value == preset { None } else { value.clone() }
        }
        let next = Overrides {
            mode: if f.mode == p.mode { None } else { Some(f.mode) },
            smtp_host: diff(&f.smtp_host, &p.smtp_host),
            smtp_port: diff(&f.smtp_port, &p.smtp_port),
            smtp_tls: if f.smtp_tls == p.smtp_tls.unwrap_or_default() {
                None
            } else {
                Some(f.smtp_tls)
            },
            smtp_username: diff(&f.smtp_username, &p.smtp_username),
            relay_url: diff(&f.relay_url, &p.relay_url),
            api_key_header: diff(&f.api_key_header, &p.api_key_header),
            sender_name: diff(&f.sender_name, &p.sender_name),
            sender_address: diff(&f.sender_address, &p.sender_address),
            recipients: if f.recipients == p.recipients {
                None
            } else {
                Some(f.recipients.clone())
            },
            language: diff(&Some(f.language.clone()), &p.language),
            notifications: f.notifications,
        };
        if let Some(path) = &self.settings_path {
            let json =
                serde_json::to_vec_pretty(&next).map_err(|e| SaveError::Io(e.to_string()))?;
            write_atomic(path, &json).map_err(|e| SaveError::Io(e.to_string()))?;
        }
        *self.overrides.lock().expect("poisoned") = next;
        Ok(before)
    }

    /// Builds the transport for the current settings.
    pub fn transport(&self) -> Result<Arc<dyn MailTransport>, MailError> {
        if let Some(t) = self.transport_override.lock().expect("poisoned").clone() {
            return Ok(t);
        }
        self.transport_for(&self.form())
    }

    /// Builds the transport for `form` (used to reach old addresses with
    /// the settings that worked for them).
    pub fn transport_for(&self, form: &MailForm) -> Result<Arc<dyn MailTransport>, MailError> {
        if let Some(t) = self.transport_override.lock().expect("poisoned").clone() {
            return Ok(t);
        }
        match form.mode {
            None => Err(MailError::NotConfigured),
            Some(MailMode::Smtp) => {
                let host = nonempty(&form.smtp_host).ok_or(MailError::NotConfigured)?;
                let sender = nonempty(&form.sender_address).ok_or(MailError::NotConfigured)?;
                let username = nonempty(&form.smtp_username);
                let password = match &username {
                    Some(_) => Some(
                        self.secret(SecretKind::SmtpPassword)
                            .ok_or(MailError::NotConfigured)?,
                    ),
                    None => None,
                };
                Ok(Arc::new(transport::SmtpTransport::new(
                    transport::SmtpConfig {
                        port: form.smtp_port.unwrap_or(form.smtp_tls.default_port()),
                        host,
                        tls: form.smtp_tls,
                        username,
                        password,
                        sender_address: sender,
                        sender_name: nonempty(&form.sender_name),
                    },
                )?))
            }
            Some(MailMode::Relay) => {
                let url = nonempty(&form.relay_url).ok_or(MailError::NotConfigured)?;
                let key = self
                    .secret(SecretKind::RelayKey)
                    .ok_or(MailError::NotConfigured)?;
                Ok(Arc::new(transport::RelayTransport::new(
                    transport::RelayConfig {
                        url,
                        key,
                        auth: vca_speech::auth::EngineAuth {
                            key_header: nonempty(&form.api_key_header),
                            gateway_token: self.gateway_token(),
                        },
                    },
                )?))
            }
        }
    }
}

/// Does `after` send mail somewhere else than `before`? That covers the
/// addresses and whatever delivers to them: mail carries the recovery code.
pub fn delivery_changed(before: &MailForm, after: &MailForm) -> bool {
    let same = |set: &MailForm| {
        (
            set.mode,
            set.smtp_host.clone(),
            set.smtp_port,
            set.smtp_tls,
            set.smtp_username.clone(),
            set.relay_url.clone(),
            set.api_key_header.clone(),
        )
    };
    let mut old = before.recipients.clone();
    let mut new = after.recipients.clone();
    old.sort();
    new.sort();
    old != new || same(before) != same(after)
}

/// Why saving failed.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Invalid(InvalidSetting),
    #[error("cannot save the mail settings: {0}")]
    Io(String),
}

fn load_preset(path: &Path) -> anyhow::Result<MailPreset> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("invalid mail preset {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MailPreset::default()),
        Err(e) => Err(anyhow::anyhow!("cannot read {}: {e}", path.display())),
    }
}

/// The preset path: [`PRESET_ENV`], else `<config dir>/mail-preset.toml`.
pub fn preset_path(config_dir: &Path) -> PathBuf {
    std::env::var_os(PRESET_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir.join(PRESET_FILE))
}

/// Writes `data` to `path` through a temporary file (owner-only directory
/// on Unix).
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vca_net::MemoryKeyStore;

    fn service(preset: MailPreset) -> MailService {
        MailService::in_memory(preset, Arc::new(MemoryKeyStore::new()))
    }

    fn smtp_form() -> MailForm {
        MailForm {
            mode: Some(MailMode::Smtp),
            smtp_host: Some("smtp.example.com".into()),
            smtp_port: None,
            smtp_tls: TlsMode::Starttls,
            smtp_username: Some("owner@example.com".into()),
            sender_address: Some("vca@example.com".into()),
            sender_name: Some("VCA".into()),
            recipients: vec!["owner@example.com".into()],
            language: "ko".into(),
            ..MailForm::default()
        }
    }

    #[test]
    fn addresses_and_hosts() {
        assert!(valid_address("a.b+c@example.com"));
        for bad in ["", "a", "a@", "@b.com", "a b@c.com", "a@b.com\n"] {
            assert!(!valid_address(bad), "{bad:?}");
        }
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("[::1]"));
        assert!(!is_loopback_host("smtp.example.com"));
        assert_eq!(mask_address("owner@example.com"), "o***@example.com");
        assert!(domain_allowed(
            "a@mail.example.com",
            &["example.com".into()]
        ));
        assert!(domain_allowed("a@example.com", &["@example.com".into()]));
        assert!(!domain_allowed("a@badexample.com", &["example.com".into()]));
    }

    #[test]
    fn validation_rules() {
        let s = service(MailPreset::default());
        assert!(s.validate(&smtp_form()).is_ok());
        let mut f = smtp_form();
        f.smtp_tls = TlsMode::None;
        assert_eq!(s.validate(&f).unwrap_err().field, "smtp_tls");
        f.smtp_host = Some("127.0.0.1".into());
        assert!(s.validate(&f).is_ok());
        let mut f = smtp_form();
        f.recipients = vec!["nope".into()];
        assert_eq!(s.validate(&f).unwrap_err().field, "recipients");
        let mut f = smtp_form();
        f.mode = Some(MailMode::Relay);
        f.relay_url = Some("http://relay.example.com/send".into());
        assert_eq!(s.validate(&f).unwrap_err().field, "relay_url");
        f.relay_url = Some("https://user:pw@relay.example.com/send".into());
        assert_eq!(s.validate(&f).unwrap_err().field, "relay_url");
        f.relay_url = Some("https://relay.example.com/send".into());
        f.api_key_header = Some("Host".into());
        assert_eq!(s.validate(&f).unwrap_err().field, "api_key_header");
        f.api_key_header = Some("X-VCA-Key".into());
        assert!(s.validate(&f).is_ok());
    }

    #[test]
    fn preset_layers_under_owner_changes() {
        let preset: MailPreset = toml::from_str(
            r#"
mode = "relay"
relay_url = "https://relay.example.com/v1/mail"
api_key_header = "X-VCA-Key"
sender_name = "Voice Command AI"
allowed_recipient_domains = ["example.com"]
language = "ko"
"#,
        )
        .unwrap();
        let s = service(preset);
        let view = s.view();
        assert_eq!(view.form.mode, Some(MailMode::Relay));
        assert!(view.preset.contains(&"relay_url".to_owned()));
        assert!(
            view.preset
                .contains(&"allowed_recipient_domains".to_owned())
        );
        assert!(!view.configured);
        assert_eq!(view.missing, ["relay_key", "recipients"]);

        let mut f = s.form();
        f.recipients = vec!["someone@other.org".into()];
        match s.save(&f) {
            Err(SaveError::Invalid(e)) => assert!(e.not_allowed),
            other => panic!("{other:?}"),
        }
        f.recipients = vec!["owner@example.com".into()];
        s.save(&f).unwrap();
        s.set_secret(SecretKind::RelayKey, Some("k-123")).unwrap();
        let view = s.view();
        assert!(view.configured, "{:?}", view.missing);
        assert!(view.relay_key_set);
        // Unchanged preset values stay inherited.
        assert!(view.preset.contains(&"relay_url".to_owned()));
        assert!(!view.preset.contains(&"recipients".to_owned()));
        // The view never contains the secret.
        assert!(!serde_json::to_string(&view).unwrap().contains("k-123"));
        s.set_secret(SecretKind::RelayKey, None).unwrap();
        assert!(!s.configured());
        assert!(s.set_secret(SecretKind::RelayKey, Some("  ")).is_err());
    }

    #[test]
    fn settings_persist_and_preset_file_loads() {
        let dir = tempfile::tempdir().unwrap();
        let preset_path = dir.path().join(PRESET_FILE);
        std::fs::write(
            &preset_path,
            "mode = \"smtp\"\nsmtp_host = \"smtp.example.com\"\nsmtp_tls = \"tls\"\nsender_address = \"vca@example.com\"\n",
        )
        .unwrap();
        let secrets: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());
        let s = MailService::open(Some(&preset_path), Some(dir.path()), secrets.clone()).unwrap();
        assert_eq!(s.form().smtp_tls, TlsMode::Tls);
        let mut f = s.form();
        f.recipients = vec!["owner@example.com".into()];
        f.notifications.auth_failures = false;
        s.save(&f).unwrap();
        let again = MailService::open(Some(&preset_path), Some(dir.path()), secrets).unwrap();
        assert_eq!(again.form(), s.form());
        assert!(again.configured());
        let text =
            std::fs::read_to_string(dir.path().join("security").join(SETTINGS_FILE)).unwrap();
        assert!(
            !text.contains("smtp.example.com"),
            "preset values are not copied"
        );

        std::fs::write(&preset_path, "unknown_key = 1\n").unwrap();
        assert!(
            MailService::open(Some(&preset_path), None, Arc::new(MemoryKeyStore::new())).is_err()
        );
    }
}
