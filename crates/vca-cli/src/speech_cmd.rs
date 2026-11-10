//! `vca transcribe`: runs an audio file through the server's speech
//! pipeline and prints the transcript for review. Nothing is sent to a
//! session.
//!
//! `vca speech`: lists the engine and correction LLM profiles, and stores or
//! deletes their API keys on this PC (local channel only). For an engine
//! behind a gateway or reverse proxy that authenticates with its own bearer
//! token, `key-header` moves the API key to a header of its own and
//! `gateway-login` / `gateway-logout` store or delete the gateway token.

use std::path::Path;

use clap::Subcommand;
use vca_core::{AudioEncoding, ClientMsg, ServerMsg, TranscriptIntent};
use vca_server::control::{LocalRequest, LocalResponse};
use vca_server::engines::{EngineEntry, EngineSetup, EngineType};

use crate::client::{Client, unexpected};
use crate::error::{CliError, CliResult};

fn encoding(file: &Path) -> CliResult<AudioEncoding> {
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    Ok(match ext.as_str() {
        "wav" => AudioEncoding::Wav,
        "webm" => AudioEncoding::Webm,
        "ogg" | "opus" => AudioEncoding::Ogg,
        "m4a" | "mp4" => AudioEncoding::M4a,
        _ => {
            return Err(CliError::usage(format!(
                "unknown audio type {:?} (use .wav, .webm, .ogg or .m4a)",
                file.display()
            )));
        }
    })
}

pub async fn transcribe(
    endpoint: &Path,
    file: &Path,
    language: Option<String>,
    session: Option<String>,
    json: bool,
) -> CliResult<i32> {
    let format = encoding(file)?;
    let audio = std::fs::read(file)
        .map_err(|e| CliError::failure(format!("cannot read {}: {e}", file.display())))?;
    let mut client = Client::connect(endpoint).await?;
    let session_id = match session {
        Some(s) => Some(client.resolve(&s).await?),
        None => None,
    };
    let reply = client
        .api(ClientMsg::Transcribe {
            audio,
            format,
            sample_rate: None,
            language,
            session_id,
            context: None,
            // A file is a whole utterance: give it the full pipeline.
            is_final: true,
        })
        .await?;
    let ServerMsg::Transcript {
        text,
        raw_text,
        language,
        corrections,
        intent,
        engine,
        topic,
    } = reply
    else {
        return Err(unexpected(&reply));
    };
    if json {
        let value = serde_json::json!({
            "text": text,
            "raw_text": raw_text,
            "language": language,
            "corrections": corrections,
            "intent": intent,
            "engine": engine,
            "topic": topic,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| CliError::failure(e.to_string()))?
        );
        return Ok(0);
    }
    println!("{text}");
    if raw_text != text {
        println!("  heard:   {raw_text}");
    }
    for c in &corrections {
        println!("  fixed:   {:?} -> {:?} ({})", c.from, c.to, c.layer);
    }
    let intent = match intent {
        TranscriptIntent::Prompt => "prompt for the agent".to_owned(),
        TranscriptIntent::Ambiguous => "ambiguous (command or prompt)".to_owned(),
        TranscriptIntent::Control {
            action, session_id, ..
        } => match session_id {
            Some(id) => format!("command: {action} on {id}"),
            None => format!("command: {action} on the selected session"),
        },
    };
    println!("  intent:  {intent}");
    if let Some(topic) = &topic {
        println!("  topic:   {topic}");
    }
    println!(
        "  engine:  {engine}{}",
        language
            .map(|l| format!(", language {l}"))
            .unwrap_or_default()
    );
    Ok(0)
}

#[derive(Debug, Subcommand)]
pub enum SpeechCmd {
    /// List speech engine and correction LLM profiles.
    Options {
        #[arg(long)]
        json: bool,
    },
    /// Store the API key of a profile on this PC (input hidden, or stdin).
    Login {
        /// Profile id from [[speech.engines]] or [[speech.llms]].
        profile: String,
    },
    /// Delete the stored API key of a profile.
    Logout { profile: String },
    /// Send the profile's API key raw in its own header (e.g. X-VCA-Key)
    /// instead of `Authorization: Bearer`. Needed behind a gateway that
    /// authenticates with its own bearer token. Restart vcad afterwards.
    KeyHeader {
        /// Profile id from [[speech.engines]] or [[speech.llms]].
        profile: String,
        /// Header name, e.g. X-VCA-Key.
        #[arg(required_unless_present = "clear")]
        header: Option<String>,
        /// Go back to `Authorization: Bearer <key>`.
        #[arg(long, conflicts_with = "header")]
        clear: bool,
    },
    /// Store or replace the gateway token of a profile on this PC (input
    /// hidden, or stdin). Sent as `Authorization: Bearer <token>`; needs a
    /// key header. Applies at once.
    GatewayLogin { profile: String },
    /// Delete the stored gateway token of a profile.
    GatewayLogout { profile: String },
}

pub async fn run(endpoint: &Path, cmd: SpeechCmd) -> CliResult<i32> {
    match cmd {
        SpeechCmd::Options { json } => options(endpoint, json).await,
        SpeechCmd::Login { profile } => login(endpoint, profile).await,
        SpeechCmd::Logout { profile } => logout(endpoint, profile).await,
        SpeechCmd::KeyHeader {
            profile, header, ..
        } => set_key_header(endpoint, Family::Speech, &profile, header).await,
        SpeechCmd::GatewayLogin { profile } => {
            gateway_login(endpoint, Family::Speech, &profile).await
        }
        SpeechCmd::GatewayLogout { profile } => {
            gateway_logout(endpoint, Family::Speech, &profile).await
        }
    }
}

/// Which engines a name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    /// Speech-to-text engines and correction LLMs (ids are shared).
    Speech,
    Tts,
}

impl Family {
    fn what(self) -> &'static str {
        match self {
            Self::Speech => "speech profile",
            Self::Tts => "TTS engine",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Self::Speech => "speech",
            Self::Tts => "tts",
        }
    }
}

fn setup_of(resp: LocalResponse) -> CliResult<EngineSetup> {
    match resp {
        LocalResponse::Engines { setup, .. } => Ok(*setup),
        other => Err(crate::cloud_cmd::unexpected_local(&other)),
    }
}

/// The profile named `name` in `setup`, and its list.
fn find_profile(
    setup: &EngineSetup,
    family: Family,
    name: &str,
) -> CliResult<(EngineType, EngineEntry)> {
    let lists: &[(EngineType, &Vec<EngineEntry>)] = match family {
        Family::Speech => &[(EngineType::Stt, &setup.stt), (EngineType::Llm, &setup.llm)],
        Family::Tts => &[(EngineType::Tts, &setup.tts)],
    };
    lists
        .iter()
        .find_map(|(kind, list)| {
            list.iter()
                .find(|e| e.profile.id == name)
                .map(|e| (*kind, e.clone()))
        })
        .ok_or_else(|| {
            CliError::usage(format!(
                "no {} {name:?} in {}",
                family.what(),
                setup.config_path
            ))
        })
}

async fn engine_setup(client: &mut Client) -> CliResult<EngineSetup> {
    let id = client.next_id();
    setup_of(client.local(LocalRequest::EnginesGet { id }).await?)
}

/// `vca speech|tts key-header <name> <header>|--clear`.
pub(crate) async fn set_key_header(
    endpoint: &Path,
    family: Family,
    name: &str,
    header: Option<String>,
) -> CliResult<i32> {
    let header = header
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty());
    let mut client = Client::connect(endpoint).await?;
    let setup = engine_setup(&mut client).await?;
    let (kind, entry) = find_profile(&setup, family, name)?;
    if entry.read_only {
        return Err(CliError::usage(format!(
            "{name:?} comes from an older [speech] setting; set api_key_header in {} by hand",
            setup.config_path
        )));
    }
    let mut profile = entry.profile;
    profile.api_key_header = header.clone();
    let id = client.next_id();
    let setup = setup_of(
        client
            .local(LocalRequest::EngineSave {
                id,
                kind,
                original_id: Some(name.to_owned()),
                profile,
                secret: None,
                make_default: entry.default,
                gateway_token: None,
                clear_gateway_token: false,
                step_up: None,
            })
            .await?,
    )?;
    match header {
        Some(h) => println!(
            "{} {name} now sends its API key in the {h} header",
            family.what()
        ),
        None => println!(
            "{} {name} now sends its API key as Authorization: Bearer",
            family.what()
        ),
    }
    if setup.restart_required {
        println!("restart vcad to use the change");
    }
    Ok(0)
}

/// `vca speech|tts gateway-login <name>`.
pub(crate) async fn gateway_login(endpoint: &Path, family: Family, name: &str) -> CliResult<i32> {
    let token = crate::cloud_cmd::read_secret(&format!(
        "gateway token for {} {name} (input hidden): ",
        family.what()
    ))?;
    let mut client = Client::connect(endpoint).await?;
    let setup = engine_setup(&mut client).await?;
    let (kind, _) = find_profile(&setup, family, name)?;
    let id = client.next_id();
    let setup = setup_of(
        client
            .local(LocalRequest::EngineGatewayToken {
                id,
                kind,
                profile_id: name.to_owned(),
                token: Some(token),
            })
            .await?,
    )?;
    println!("stored the gateway token for {} {name}", family.what());
    if setup.restart_required {
        println!(
            "restart vcad to use the latest engine settings (vca {} key-header changes)",
            family.command()
        );
    }
    Ok(0)
}

/// `vca speech|tts gateway-logout <name>`.
pub(crate) async fn gateway_logout(endpoint: &Path, family: Family, name: &str) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let setup = engine_setup(&mut client).await?;
    let (kind, _) = find_profile(&setup, family, name)?;
    let id = client.next_id();
    setup_of(
        client
            .local(LocalRequest::EngineGatewayToken {
                id,
                kind,
                profile_id: name.to_owned(),
                token: None,
            })
            .await?,
    )?;
    println!(
        "deleted the stored gateway token for {} {name}",
        family.what()
    );
    Ok(0)
}

async fn options(endpoint: &Path, json: bool) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let reply = client.api(ClientMsg::ListSpeechOptions).await?;
    let ServerMsg::SpeechOptions {
        engines,
        llms,
        current_engine,
        current_llm,
    } = reply
    else {
        return Err(unexpected(&reply));
    };
    if json {
        let value = serde_json::json!({
            "engines": engines,
            "llms": llms,
            "current_engine": current_engine,
            "current_llm": current_llm,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| CliError::failure(e.to_string()))?
        );
        return Ok(0);
    }
    if engines.is_empty() {
        println!("server-side speech recognition is off (configure [speech] in config.toml)");
        return Ok(0);
    }
    let print = |title: &str, list: &[vca_core::SpeechProfileInfo], current: Option<&str>| {
        println!("{title}:");
        for p in list {
            let mut notes = Vec::new();
            if p.default {
                notes.push("default".to_owned());
            }
            if current == Some(p.id.as_str()) {
                notes.push("in use here".to_owned());
            }
            if !p.configured {
                notes.push(format!("no key: vca speech login {}", p.id));
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!(" ({})", notes.join(", "))
            };
            println!("  {:<20} {} [{}]{notes}", p.id, p.label, p.kind);
        }
    };
    print("engines", &engines, current_engine.as_deref());
    if llms.is_empty() {
        println!("correction LLMs: none configured");
    } else {
        print("correction LLMs", &llms, current_llm.as_deref());
    }
    Ok(0)
}

async fn login(endpoint: &Path, profile: String) -> CliResult<i32> {
    let secret = crate::cloud_cmd::read_secret(&format!(
        "API key for speech profile {profile} (input hidden): "
    ))?;
    let mut client = Client::connect(endpoint).await?;
    let id = client.next_id();
    let resp = client
        .local(LocalRequest::SpeechCredentialSet {
            id,
            profile: profile.clone(),
            secret,
        })
        .await?;
    let LocalResponse::Done { .. } = resp else {
        return Err(crate::cloud_cmd::unexpected_local(&resp));
    };
    println!("stored the API key for speech profile {profile}");
    Ok(0)
}

async fn logout(endpoint: &Path, profile: String) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let id = client.next_id();
    let resp = client
        .local(LocalRequest::SpeechCredentialDelete {
            id,
            profile: profile.clone(),
        })
        .await?;
    let LocalResponse::Done { .. } = resp else {
        return Err(crate::cloud_cmd::unexpected_local(&resp));
    };
    println!("deleted the stored API key for speech profile {profile}");
    Ok(0)
}
