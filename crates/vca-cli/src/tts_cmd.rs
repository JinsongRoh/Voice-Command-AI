//! `vca tts`: lists the PC's TTS engines (`[[tts.engines]]`), synthesizes a
//! test phrase into a file, and stores or deletes engine API keys on this
//! PC (local channel only). Nothing is played.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use vca_core::{ClientMsg, ServerMsg, TtsGender, TtsSpeaker};
use vca_server::control::{LocalRequest, LocalResponse};

use crate::client::{Client, unexpected};
use crate::error::{CliError, CliResult};
use crate::speech_cmd::{self, Family};

#[derive(Debug, Subcommand)]
pub enum TtsCmd {
    /// List the TTS engines configured on this PC.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Synthesize a test phrase and save the audio to a file.
    Say {
        /// Text to speak (at most 300 characters).
        text: String,
        /// Engine name; the default engine when omitted.
        #[arg(long)]
        engine: Option<String>,
        /// Language (BCP-47), which picks the engine's voice for it.
        #[arg(long)]
        language: Option<String>,
        /// Speaker id from `vca tts list` (e.g. F1); the engine's configured
        /// voice when omitted.
        #[arg(long)]
        voice: Option<String>,
        /// Output file; `speech.<format>` when omitted.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Store the API key of an engine on this PC (input hidden, or stdin).
    Login {
        /// Engine name from [[tts.engines]].
        name: String,
    },
    /// Delete the stored API key of an engine.
    Logout { name: String },
    /// Send the engine's API key raw in its own header (e.g. X-VCA-Key)
    /// instead of `Authorization: Bearer`. Needed behind a gateway that
    /// authenticates with its own bearer token. Restart vcad afterwards.
    KeyHeader {
        /// Engine name from [[tts.engines]].
        name: String,
        /// Header name, e.g. X-VCA-Key.
        #[arg(required_unless_present = "clear")]
        header: Option<String>,
        /// Go back to `Authorization: Bearer <key>`.
        #[arg(long, conflicts_with = "header")]
        clear: bool,
    },
    /// Store or replace the gateway token of an engine on this PC (input
    /// hidden, or stdin). Sent as `Authorization: Bearer <token>`; needs a
    /// key header. Applies at once.
    GatewayLogin { name: String },
    /// Delete the stored gateway token of an engine.
    GatewayLogout { name: String },
}

pub async fn run(endpoint: &Path, cmd: TtsCmd) -> CliResult<i32> {
    match cmd {
        TtsCmd::List { json } => list(endpoint, json).await,
        TtsCmd::Say {
            text,
            engine,
            language,
            voice,
            out,
        } => say(endpoint, text, engine, language, voice, out).await,
        TtsCmd::Login { name } => login(endpoint, name).await,
        TtsCmd::Logout { name } => logout(endpoint, name).await,
        TtsCmd::KeyHeader { name, header, .. } => {
            speech_cmd::set_key_header(endpoint, Family::Tts, &name, header).await
        }
        TtsCmd::GatewayLogin { name } => {
            speech_cmd::gateway_login(endpoint, Family::Tts, &name).await
        }
        TtsCmd::GatewayLogout { name } => {
            speech_cmd::gateway_logout(endpoint, Family::Tts, &name).await
        }
    }
}

async fn list(endpoint: &Path, json: bool) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let reply = client.api(ClientMsg::ListTtsEngines).await?;
    let ServerMsg::TtsEngines { engines } = reply else {
        return Err(unexpected(&reply));
    };
    if json {
        let value = serde_json::json!({ "engines": engines });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| CliError::failure(e.to_string()))?
        );
        return Ok(0);
    }
    if engines.is_empty() {
        println!("no TTS engines (add [[tts.engines]] to config.toml)");
        return Ok(0);
    }
    for e in &engines {
        let languages = if e.languages.is_empty() {
            "default voice".to_owned()
        } else {
            e.languages.join(", ")
        };
        let default = if e.default { " (default)" } else { "" };
        println!("  {:<20} {languages}{default}", e.name);
        for line in speaker_lines(&e.speakers) {
            println!("      {line}");
        }
    }
    Ok(0)
}

async fn say(
    endpoint: &Path,
    text: String,
    engine: Option<String>,
    language: Option<String>,
    voice: Option<String>,
    out: Option<PathBuf>,
) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let reply = client
        .api(ClientMsg::Synthesize {
            text,
            language,
            engine,
            voice,
        })
        .await?;
    let ServerMsg::Audio { format, data } = reply else {
        return Err(unexpected(&reply));
    };
    let out = out.unwrap_or_else(|| PathBuf::from(format!("speech.{format}")));
    std::fs::write(&out, &data)
        .map_err(|e| CliError::failure(format!("cannot write {}: {e}", out.display())))?;
    println!(
        "saved {} bytes of {format} to {}",
        data.len(),
        out.display()
    );
    Ok(0)
}

async fn login(endpoint: &Path, name: String) -> CliResult<i32> {
    let secret =
        crate::cloud_cmd::read_secret(&format!("API key for TTS engine {name} (input hidden): "))?;
    let mut client = Client::connect(endpoint).await?;
    let id = client.next_id();
    let resp = client
        .local(LocalRequest::TtsCredentialSet {
            id,
            engine: name.clone(),
            secret,
        })
        .await?;
    let LocalResponse::Done { .. } = resp else {
        return Err(crate::cloud_cmd::unexpected_local(&resp));
    };
    println!("stored the API key for TTS engine {name}");
    Ok(0)
}

async fn logout(endpoint: &Path, name: String) -> CliResult<i32> {
    let mut client = Client::connect(endpoint).await?;
    let id = client.next_id();
    let resp = client
        .local(LocalRequest::TtsCredentialDelete {
            id,
            engine: name.clone(),
        })
        .await?;
    let LocalResponse::Done { .. } = resp else {
        return Err(crate::cloud_cmd::unexpected_local(&resp));
    };
    println!("deleted the stored API key for TTS engine {name}");
    Ok(0)
}

/// One line per gender: `female: F1 (Female 1), F2 (Female 2)`.
fn speaker_lines(speakers: &[TtsSpeaker]) -> Vec<String> {
    [
        (TtsGender::Female, "female"),
        (TtsGender::Male, "male"),
        (TtsGender::Other, "other"),
    ]
    .into_iter()
    .filter_map(|(gender, name)| {
        let items: Vec<String> = speakers
            .iter()
            .filter(|s| s.gender == gender)
            .map(|s| format!("{} ({})", s.id, s.label))
            .collect();
        (!items.is_empty()).then(|| format!("{name}: {}", items.join(", ")))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: TtsCmd,
    }

    #[test]
    fn say_takes_a_voice() {
        let w =
            Wrap::try_parse_from(["t", "say", "Hello", "--voice", "F1", "--engine", "x"]).unwrap();
        let TtsCmd::Say { voice, engine, .. } = w.cmd else {
            panic!("not say");
        };
        assert_eq!(voice.as_deref(), Some("F1"));
        assert_eq!(engine.as_deref(), Some("x"));
        let w = Wrap::try_parse_from(["t", "say", "Hello"]).unwrap();
        let TtsCmd::Say { voice, .. } = w.cmd else {
            panic!("not say");
        };
        assert_eq!(voice, None);
    }

    #[test]
    fn key_header_and_gateway_commands_parse() {
        let w = Wrap::try_parse_from(["t", "key-header", "voice", "X-VCA-Key"]).unwrap();
        let TtsCmd::KeyHeader {
            name,
            header,
            clear,
        } = w.cmd
        else {
            panic!("not key-header");
        };
        assert_eq!(
            (name.as_str(), header.as_deref(), clear),
            ("voice", Some("X-VCA-Key"), false)
        );
        let w = Wrap::try_parse_from(["t", "key-header", "voice", "--clear"]).unwrap();
        assert!(matches!(
            w.cmd,
            TtsCmd::KeyHeader {
                header: None,
                clear: true,
                ..
            }
        ));
        assert!(Wrap::try_parse_from(["t", "key-header", "voice"]).is_err());
        assert!(Wrap::try_parse_from(["t", "key-header", "voice", "X-K", "--clear"]).is_err());
        assert!(matches!(
            Wrap::try_parse_from(["t", "gateway-login", "voice"])
                .unwrap()
                .cmd,
            TtsCmd::GatewayLogin { .. }
        ));
        assert!(matches!(
            Wrap::try_parse_from(["t", "gateway-logout", "voice"])
                .unwrap()
                .cmd,
            TtsCmd::GatewayLogout { .. }
        ));
    }

    #[test]
    fn speakers_are_listed_by_gender() {
        let sp = |id: &str, label: &str, gender| TtsSpeaker {
            id: id.into(),
            label: label.into(),
            gender,
        };
        let lines = speaker_lines(&[
            sp("M1", "Male 1", TtsGender::Male),
            sp("F1", "Female 1", TtsGender::Female),
            sp("F2", "Female 2", TtsGender::Female),
        ]);
        assert_eq!(
            lines,
            vec!["female: F1 (Female 1), F2 (Female 2)", "male: M1 (Male 1)"]
        );
        assert!(speaker_lines(&[]).is_empty());
    }
}
