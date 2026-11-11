//! `vca mail` and `vca security`: mail settings, local verification and
//! recovery on this computer.
//!
//! Gated actions first verify the owner: vcad shows the OS prompt
//! (Windows Hello, Touch ID or the password), or, on a computer without
//! one, asks for the authenticator code here on the terminal. Secrets are
//! only ever read from a hidden prompt or stdin, never from arguments.

use std::path::Path;

use clap::{Subcommand, ValueEnum};
use vca_server::control::{LocalRequest, LocalResponse, REMOTE_SHELL_ENV, SESSION_ENV};
use vca_server::security::mail::{MailForm, MailMode, MailView, SecretKind, TlsMode};
use vca_server::security::{
    FailReason, GatedAction, GrantOp, Presence, SecurityCall, SecurityReply, SecurityStatus,
    Sensitive, StepUpChallenge,
};

use crate::client::Client;
use crate::cloud_cmd::read_secret;
use crate::error::{CliError, CliResult};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ModeArg {
    Smtp,
    Relay,
    /// Turn mail off.
    Off,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TlsArg {
    Starttls,
    Tls,
    /// No encryption: only for a server on this computer.
    None,
}

#[derive(Debug, Subcommand)]
pub enum MailAction {
    /// Show the mail settings (never a secret).
    Show,
    /// Change mail settings; options left out keep their value.
    Set {
        #[arg(long, value_enum)]
        mode: Option<ModeArg>,
        /// SMTP server name.
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long, value_enum)]
        tls: Option<TlsArg>,
        #[arg(long)]
        username: Option<String>,
        /// Sender address (SMTP only; the relay decides the sender).
        #[arg(long)]
        sender: Option<String>,
        #[arg(long)]
        sender_name: Option<String>,
        /// Owner address; repeat for several (replaces the list).
        #[arg(long = "to")]
        to: Vec<String>,
        /// Base URL of the mail relay.
        #[arg(long)]
        relay_url: Option<String>,
        /// Header carrying the relay key, e.g. X-VCA-Key.
        #[arg(long)]
        api_key_header: Option<String>,
        /// Language of the mails: ko, en, ...
        #[arg(long)]
        language: Option<String>,
    },
    /// Send a test mail to the owner addresses.
    Test,
    /// Store the SMTP password (hidden prompt).
    Password {
        /// Delete the stored password.
        #[arg(long)]
        delete: bool,
    },
    /// Store the relay key (hidden prompt).
    RelayKey {
        #[arg(long)]
        delete: bool,
    },
    /// Store the relay's gateway token (hidden prompt).
    GatewayToken {
        #[arg(long)]
        delete: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SecurityAction {
    /// Show what is set up.
    Status,
    /// Check local verification now.
    Verify,
    /// Set the grace window after a verification, in seconds.
    Grace { seconds: u64 },
    /// Turn remote shells on or off.
    RemoteShell {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Lost the authenticator: verify here, then a mailed or backup code.
    Recover {
        /// Use a backup code instead of a mailed code.
        #[arg(long)]
        backup_code: bool,
    },
}

async fn call(client: &mut Client, call: SecurityCall) -> CliResult<SecurityReply> {
    let id = client.next_id();
    let req = LocalRequest::Security {
        id,
        call: Box::new(call),
    };
    match client.local(req).await? {
        LocalResponse::Security { reply, .. } => Ok(*reply),
        other => Err(CliError::failure(format!("unexpected answer: {other:?}"))),
    }
}

fn fail(reply: SecurityReply) -> CliError {
    match reply {
        SecurityReply::Failed { message, .. } => CliError::failure(message),
        other => CliError::failure(format!("unexpected answer: {other:?}")),
    }
}

/// Answers `challenge`: the OS prompt, the authenticator code, or on a
/// computer with nothing to verify against, a person's confirmation.
async fn verify(client: &mut Client, challenge: &StepUpChallenge) -> CliResult<Sensitive> {
    // Remote shells mark their processes. vcad refuses them where it can
    // tell; this answers plainly where it cannot.
    if std::env::var_os(REMOTE_SHELL_ENV).is_some() {
        return Err(CliError::failure(
            "this needs a person at the computer; it cannot be done from a remote shell",
        ));
    }
    let request = |totp, presence| SecurityCall::StepUp {
        nonce: Sensitive(challenge.nonce.clone()),
        window: None,
        totp,
        presence,
        language: None,
    };
    let reply = call(client, request(None, None)).await?;
    let reply = match reply {
        SecurityReply::Failed {
            reason: FailReason::TotpRequired,
            ..
        } => {
            let prompt = if challenge.action == GatedAction::Recovery {
                "Authenticator or backup code (input hidden): "
            } else {
                "Authenticator code (input hidden): "
            };
            let code = read_secret(prompt)?;
            call(client, request(Some(Sensitive::from(code.expose())), None)).await?
        }
        SecurityReply::Failed {
            reason: FailReason::PresenceRequired,
            ..
        } => {
            confirm_presence()?;
            call(client, request(None, Some(Presence::Terminal))).await?
        }
        other => other,
    };
    match reply {
        SecurityReply::StepUp { token, .. } => Ok(token),
        other => Err(fail(other)),
    }
}

/// First enrollment with nothing to verify against: a person says so, at a
/// real terminal, outside of any session VCA runs.
fn confirm_presence() -> CliResult<()> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(CliError::failure(
            "the first authenticator must be set up on an interactive terminal",
        ));
    }
    if std::env::var_os(SESSION_ENV).is_some() {
        return Err(CliError::failure(
            "the first authenticator cannot be set up from a session; \
             use a terminal of your own",
        ));
    }
    println!("This computer cannot verify you yet: it has no system prompt and no authenticator.");
    println!("Continue only if you started this yourself.");
    print!("Type yes to set up the first authenticator: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| CliError::failure(e.to_string()))?;
    if line.trim().eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err(CliError::failure("canceled"))
    }
}

/// Runs a security request that may need a verification: `build` makes the
/// request for a token. Returns the answer and the token, which a shared
/// class can use for the next request of its flow.
pub(crate) async fn gated_call(
    client: &mut Client,
    build: impl Fn(Option<Sensitive>) -> SecurityCall,
) -> CliResult<(SecurityReply, Option<Sensitive>)> {
    match call(client, build(None)).await? {
        SecurityReply::StepUpRequired { challenge } => {
            let token = verify(client, &challenge).await?;
            let reply = call(client, build(Some(token.clone()))).await?;
            Ok((reply, Some(token)))
        }
        reply => Ok((reply, None)),
    }
}

/// Like [`gated_call`], for a local request. `build` gets the request id.
pub(crate) async fn gated_local(
    client: &mut Client,
    build: impl Fn(u64, Option<Sensitive>) -> LocalRequest,
) -> CliResult<(LocalResponse, Option<Sensitive>)> {
    let id = client.next_id();
    match client.local(build(id, None)).await? {
        LocalResponse::StepUpRequired { challenge, .. } => {
            let token = verify(client, &challenge).await?;
            let id = client.next_id();
            let resp = client.local(build(id, Some(token.clone()))).await?;
            Ok((resp, Some(token)))
        }
        resp => Ok((resp, None)),
    }
}

fn print_mail(view: &MailView) {
    let f = &view.form;
    let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    let mode = match f.mode {
        Some(MailMode::Smtp) => "smtp",
        Some(MailMode::Relay) => "relay",
        None => "off",
    };
    println!("mode:        {mode}");
    println!("configured:  {}", view.configured);
    if !view.missing.is_empty() {
        println!("missing:     {}", view.missing.join(", "));
    }
    match f.mode {
        Some(MailMode::Relay) => {
            println!("relay url:   {}", opt(&f.relay_url));
            println!("key header:  {}", opt(&f.api_key_header));
            println!("relay key:   {}", stored(view.relay_key_set));
            println!("gateway:     {}", stored(view.gateway_token_set));
        }
        _ => {
            println!("host:        {}", opt(&f.smtp_host));
            println!("port:        {:?}", f.smtp_port);
            println!("tls:         {:?}", f.smtp_tls);
            println!("username:    {}", opt(&f.smtp_username));
            println!("sender:      {}", opt(&f.sender_address));
            println!("password:    {}", stored(view.password_set));
        }
    }
    println!("sender name: {}", opt(&f.sender_name));
    println!("owner:       {}", f.recipients.join(", "));
    println!("language:    {}", f.language);
    if !view.preset.is_empty() {
        println!("from preset: {}", view.preset.join(", "));
    }
}

fn stored(set: bool) -> &'static str {
    if set { "stored" } else { "not set" }
}

fn print_status(s: &SecurityStatus) {
    let v = &s.verifier;
    println!(
        "local verification: {:?} ({})",
        v.kind,
        if v.available {
            "available".to_owned()
        } else {
            v.detail.clone().unwrap_or_else(|| "unavailable".into())
        }
    );
    println!(
        "2-step verification: {}",
        if s.totp.enrolled {
            format!(
                "on, {} of {} backup codes left",
                s.totp.backup_codes_remaining, s.totp.backup_codes_total
            )
        } else {
            "not set up (vca totp)".to_owned()
        }
    );
    println!(
        "mail: {}",
        if s.mail.configured {
            s.mail.recipients.join(", ")
        } else {
            "not set up".to_owned()
        }
    );
    println!("recovery: {:?}", s.recovery);
    println!(
        "remote shells: {}",
        if s.remote_shell.enabled { "on" } else { "off" }
    );
    println!("grace window: {} s", s.step_up.grace_secs);
}

/// Applies the `vca mail set` options to the current form.
#[allow(clippy::too_many_arguments)]
fn apply(
    form: &mut MailForm,
    mode: Option<ModeArg>,
    host: Option<String>,
    port: Option<u16>,
    tls: Option<TlsArg>,
    username: Option<String>,
    sender: Option<String>,
    sender_name: Option<String>,
    to: Vec<String>,
    relay_url: Option<String>,
    api_key_header: Option<String>,
    language: Option<String>,
) {
    if let Some(mode) = mode {
        form.mode = match mode {
            ModeArg::Smtp => Some(MailMode::Smtp),
            ModeArg::Relay => Some(MailMode::Relay),
            ModeArg::Off => None,
        };
    }
    if let Some(tls) = tls {
        form.smtp_tls = match tls {
            TlsArg::Starttls => TlsMode::Starttls,
            TlsArg::Tls => TlsMode::Tls,
            TlsArg::None => TlsMode::None,
        };
    }
    form.smtp_host = host.or(form.smtp_host.take());
    form.smtp_port = port.or(form.smtp_port);
    form.smtp_username = username.or(form.smtp_username.take());
    form.sender_address = sender.or(form.sender_address.take());
    form.sender_name = sender_name.or(form.sender_name.take());
    form.relay_url = relay_url.or(form.relay_url.take());
    form.api_key_header = api_key_header.or(form.api_key_header.take());
    if !to.is_empty() {
        form.recipients = to;
    }
    if let Some(language) = language {
        form.language = language;
    }
}

async fn mail_view(client: &mut Client) -> CliResult<MailView> {
    match call(client, SecurityCall::MailGet).await? {
        SecurityReply::Mail { view } => Ok(*view),
        other => Err(fail(other)),
    }
}

async fn set_secret(
    client: &mut Client,
    which: SecretKind,
    label: &str,
    delete: bool,
) -> CliResult<i32> {
    let secret = if delete {
        None
    } else {
        Some(read_secret(&format!("{label} (input hidden): "))?)
    };
    let (reply, token) = gated_call(client, |step_up| SecurityCall::MailSecret {
        which,
        secret: secret.clone(),
        current_code: None,
        step_up,
    })
    .await?;
    // Replacing one that works also needs the authenticator.
    let reply = match reply {
        SecurityReply::Failed {
            reason: FailReason::CurrentCodeRequired,
            ..
        } => {
            let code = read_secret("Code from the authenticator (input hidden): ")?;
            let again = SecurityCall::MailSecret {
                which,
                secret,
                current_code: Some(Sensitive::from(code.expose())),
                step_up: token,
            };
            call(client, again).await?
        }
        other => other,
    };
    match reply {
        SecurityReply::Mail { .. } => {
            println!("{label} {}", if delete { "deleted" } else { "stored" });
            Ok(0)
        }
        other => Err(fail(other)),
    }
}

pub async fn mail(endpoint: &Path, action: MailAction) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    match action {
        MailAction::Show => {
            print_mail(&mail_view(&mut client).await?);
            Ok(0)
        }
        MailAction::Set {
            mode,
            host,
            port,
            tls,
            username,
            sender,
            sender_name,
            to,
            relay_url,
            api_key_header,
            language,
        } => {
            let mut form = mail_view(&mut client).await?.form;
            apply(
                &mut form,
                mode,
                host,
                port,
                tls,
                username,
                sender,
                sender_name,
                to,
                relay_url,
                api_key_header,
                language,
            );
            let (reply, token) = gated_call(&mut client, |step_up| SecurityCall::MailSet {
                settings: Box::new(form.clone()),
                current_code: None,
                step_up,
            })
            .await?;
            // Changing where mail goes also needs the authenticator.
            let reply = match reply {
                SecurityReply::Failed {
                    reason: FailReason::CurrentCodeRequired,
                    ..
                } => {
                    let code = read_secret("Code from the authenticator (input hidden): ")?;
                    let current_code = Some(Sensitive::from(code.expose()));
                    let again = SecurityCall::MailSet {
                        settings: Box::new(form),
                        current_code,
                        step_up: token,
                    };
                    call(&mut client, again).await?
                }
                other => other,
            };
            match reply {
                SecurityReply::Mail { view } => {
                    print_mail(&view);
                    Ok(0)
                }
                other => Err(fail(other)),
            }
        }
        MailAction::Test => match call(&mut client, SecurityCall::MailTest).await? {
            SecurityReply::MailSent { sent_to } => {
                println!("test mail sent to {}", sent_to.join(", "));
                Ok(0)
            }
            other => Err(fail(other)),
        },
        MailAction::Password { delete } => {
            set_secret(
                &mut client,
                SecretKind::SmtpPassword,
                "SMTP password",
                delete,
            )
            .await
        }
        MailAction::RelayKey { delete } => {
            set_secret(&mut client, SecretKind::RelayKey, "Relay key", delete).await
        }
        MailAction::GatewayToken { delete } => {
            set_secret(
                &mut client,
                SecretKind::GatewayToken,
                "Gateway token",
                delete,
            )
            .await
        }
    }
}

async fn recover(client: &mut Client, backup_code: bool) -> CliResult<i32> {
    let finish = |code: Option<Sensitive>, backup: Option<Sensitive>| {
        move |step_up| SecurityCall::RecoveryFinish {
            code: code.clone(),
            backup_code: backup.clone(),
            purpose: GrantOp::Replace,
            step_up,
        }
    };
    let reply = if backup_code {
        let code = read_secret("Backup code (input hidden): ")?;
        let backup = Sensitive::from(code.expose());
        gated_call(client, finish(None, Some(backup))).await?.0
    } else {
        let (reply, token) =
            gated_call(client, |step_up| SecurityCall::RecoveryStart { step_up }).await?;
        match reply {
            SecurityReply::RecoveryCodeSent { sent_to, .. } => {
                println!("code sent to {}", sent_to.join(", "));
            }
            other => return Err(fail(other)),
        }
        let code = read_secret("Mailed code (input hidden): ")?;
        call(
            client,
            finish(Some(Sensitive::from(code.expose())), None)(token),
        )
        .await?
    };
    match reply {
        SecurityReply::RecoveryGranted { .. } => {
            println!("verified; set up the new authenticator within 10 minutes (vca totp)");
            Ok(0)
        }
        other => Err(fail(other)),
    }
}

pub async fn security(endpoint: &Path, action: SecurityAction) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    match action {
        SecurityAction::Status => {
            match call(&mut client, SecurityCall::Status { step_up: None }).await? {
                SecurityReply::Status { status } => {
                    print_status(&status);
                    Ok(0)
                }
                other => Err(fail(other)),
            }
        }
        SecurityAction::Verify => verify_test(&mut client).await,
        SecurityAction::Grace { seconds } => settings(&mut client, Some(seconds), None).await,
        SecurityAction::RemoteShell { state } => {
            settings(&mut client, None, Some(state == "on")).await
        }
        SecurityAction::Recover { backup_code } => recover(&mut client, backup_code).await,
    }
}

/// Tries the verification; nothing is approved.
async fn verify_test(client: &mut Client) -> CliResult<i32> {
    let request = |totp| SecurityCall::StepUpTest {
        window: None,
        totp,
        language: None,
    };
    let reply = match call(client, request(None)).await? {
        SecurityReply::Failed {
            reason: FailReason::TotpRequired,
            ..
        } => {
            let code = read_secret("Authenticator code (input hidden): ")?;
            call(client, request(Some(Sensitive::from(code.expose())))).await?
        }
        other => other,
    };
    match reply {
        SecurityReply::Done => {
            println!("verified");
            Ok(0)
        }
        SecurityReply::Failed {
            reason: FailReason::VerificationUnavailable,
            ..
        } => Err(CliError::failure(
            "nothing can verify you yet: set up 2-step verification (vca totp)",
        )),
        other => Err(fail(other)),
    }
}

async fn settings(
    client: &mut Client,
    grace_secs: Option<u64>,
    remote_shell: Option<bool>,
) -> CliResult<i32> {
    let (reply, _) = gated_call(client, |step_up| SecurityCall::Settings {
        grace_secs,
        remote_shell,
        step_up,
    })
    .await?;
    match reply {
        SecurityReply::Status { status } => {
            print_status(&status);
            Ok(0)
        }
        other => Err(fail(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_left_out_keep_their_value() {
        let mut form = MailForm {
            mode: Some(MailMode::Smtp),
            smtp_host: Some("smtp.example.com".into()),
            recipients: vec!["owner@example.com".into()],
            ..MailForm::default()
        };
        apply(
            &mut form,
            None,
            None,
            Some(2525),
            Some(TlsArg::Tls),
            None,
            None,
            None,
            Vec::new(),
            None,
            None,
            Some("ko".into()),
        );
        assert_eq!(form.smtp_host.as_deref(), Some("smtp.example.com"));
        assert_eq!(form.smtp_port, Some(2525));
        assert_eq!(form.smtp_tls, TlsMode::Tls);
        assert_eq!(form.recipients, ["owner@example.com"]);
        assert_eq!(form.language, "ko");

        apply(
            &mut form,
            Some(ModeArg::Off),
            None,
            None,
            None,
            None,
            None,
            None,
            vec!["new@example.com".into()],
            None,
            None,
            None,
        );
        assert_eq!(form.mode, None);
        assert_eq!(form.recipients, ["new@example.com"]);
    }
}
