//! `vca transcribe`: runs an audio file through the server's speech
//! pipeline and prints the transcript for review. Nothing is sent to a
//! session.

use std::path::Path;

use vca_core::{AudioEncoding, ClientMsg, ServerMsg, TranscriptIntent};

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
        })
        .await?;
    let ServerMsg::Transcript {
        text,
        raw_text,
        language,
        corrections,
        intent,
        engine,
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
    println!(
        "  engine:  {engine}{}",
        language
            .map(|l| format!(", language {l}"))
            .unwrap_or_default()
    );
    Ok(0)
}
