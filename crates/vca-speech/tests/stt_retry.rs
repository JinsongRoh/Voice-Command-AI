//! One retry of a transcription whose connection dropped before the server
//! answered, and none once it answered.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use vca_speech::retry::RETRY_DELAY;
use vca_speech::stt::{OpenAiSttConfig, OpenAiSttEngine};
use vca_speech::{AudioInput, SpeechError, SttEngine, TranscribeOptions};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn pcm_audio() -> AudioInput {
    AudioInput::from_pcm16(&[0, 100, -100, 200, -200, 0, 50, -50], 16_000)
}

fn engine(base: String) -> OpenAiSttEngine {
    OpenAiSttEngine::new(OpenAiSttConfig {
        base_url: format!("{base}/v1"),
        model: "gpt-4o-transcribe".into(),
        ..OpenAiSttConfig::default()
    })
    .unwrap()
}

/// Reads one HTTP request (headers plus a `content-length` body).
async fn read_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await.unwrap();
        if n == 0 {
            return buf;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
        let len = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if buf.len() >= end + 4 + len {
            return buf;
        }
    }
}

/// A server that drops the first `drops` connections without answering,
/// then answers every request with a transcript. Returns its base URL and
/// the number of connections accepted.
async fn flaky_server(drops: usize) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n < drops {
                drop(stream);
                continue;
            }
            tokio::spawn(async move {
                read_request(&mut stream).await;
                let body = json!({ "text": "hello again" }).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).await.unwrap();
                stream.shutdown().await.ok();
            });
        }
    });
    (base, accepted)
}

#[tokio::test]
async fn dropped_connection_is_retried_once_and_succeeds() {
    let (base, accepted) = flaky_server(1).await;
    let started = Instant::now();
    let t = engine(base)
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap();
    assert_eq!(t.text, "hello again");
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
    assert!(started.elapsed() >= RETRY_DELAY, "the retry waits first");
}

#[tokio::test]
async fn a_second_drop_is_reported_as_unreachable() {
    let (base, accepted) = flaky_server(usize::MAX).await;
    let err = engine(base)
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap_err();
    assert!(err.is_unreachable(), "{err:?}");
    assert_eq!(accepted.load(Ordering::SeqCst), 2, "exactly one retry");
}

#[tokio::test]
async fn refused_connection_is_unreachable() {
    let base = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let err = engine(base)
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, SpeechError::Http(_)), "{err:?}");
    assert!(err.is_unreachable(), "{err:?}");
}

#[tokio::test]
async fn server_errors_are_never_retried() {
    for status in [400u16, 500, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(status).set_body_string("nope"))
            .expect(1)
            .mount(&server)
            .await;
        let err = engine(server.uri())
            .transcribe(&pcm_audio(), &TranscribeOptions::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, SpeechError::Api { status: s, .. } if s == status),
            "{err:?}"
        );
        assert!(!err.is_unreachable());
        server.verify().await;
    }
}

/// A server that takes the whole request in and never answers.
async fn silent_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                read_request(&mut stream).await;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                drop(stream);
            });
        }
    });
    (base, accepted)
}

/// The upload arrived and the server is still working on it: sending it
/// again would bill the audio twice and double the wait.
#[tokio::test]
async fn a_timeout_after_the_upload_is_not_retried() {
    let (base, accepted) = silent_server().await;
    let engine = OpenAiSttEngine::new(OpenAiSttConfig {
        base_url: format!("{base}/v1"),
        model: "gpt-4o-transcribe".into(),
        timeout: std::time::Duration::from_millis(300),
        ..OpenAiSttConfig::default()
    })
    .unwrap();
    let err = engine
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, SpeechError::Http(e) if e.is_timeout()),
        "{err:?}"
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "no second upload");
}

/// A Google API key travels in the URL: no error text may carry it.
#[tokio::test]
async fn errors_never_carry_the_api_key() {
    use vca_speech::stt::{GoogleAuth, GoogleSttConfig, GoogleSttEngine};
    let base = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let mut config = GoogleSttConfig::new(
        "proj",
        GoogleAuth::ApiKey {
            key: "SECRET-KEY-123".into(),
        },
    );
    config.base_url = base;
    let err = GoogleSttEngine::new(config)
        .unwrap()
        .transcribe(&pcm_audio(), &TranscribeOptions::default())
        .await
        .unwrap_err();
    assert!(err.is_unreachable(), "{err:?}");
    let text = format!("{err} {err:?}");
    assert!(!text.contains("SECRET-KEY-123"), "{text}");
    let core: vca_core::Error = err.into();
    assert!(!core.to_string().contains("SECRET-KEY-123"), "{core}");
}
