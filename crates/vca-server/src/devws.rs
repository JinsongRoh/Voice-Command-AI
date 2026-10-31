//! Development-only WebSocket on 127.0.0.1 (off by default).
//!
//! Lets the UI talk to a real server before the encrypted remote transport
//! is wired up. Connections must present the random token printed at
//! startup as `?token=...`; messages are JSON [`Envelope`]s. Never enable
//! this on a shared machine: any local process that learns the token gets
//! full control.

use std::io;
use std::net::SocketAddr;

use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use vca_core::{ClientMsg, Envelope};

use crate::api::Api;

/// A running development WebSocket server. Dropping it stops listening.
pub struct DevWs {
    pub addr: SocketAddr,
    pub token: String,
    task: JoinHandle<()>,
}

impl Drop for DevWs {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Compares in time independent of where the inputs differ.
fn tokens_match(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn query_token(req: &Request) -> Option<String> {
    req.uri().query()?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "token").then(|| v.to_owned())
    })
}

/// Starts listening on `127.0.0.1:port` (`0` picks a free port).
pub async fn start(api: Api, port: u16) -> io::Result<DevWs> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let token = random_token();
    let expected = token.clone();
    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(serve(stream, api.clone(), expected.clone()));
                }
                Err(e) => tracing::warn!("dev websocket accept failed: {e}"),
            }
        }
    });
    Ok(DevWs { addr, token, task })
}

async fn serve(stream: TcpStream, api: Api, token: String) {
    #[allow(clippy::result_large_err)] // The signature is fixed by tungstenite.
    let check = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        match query_token(req) {
            Some(t) if tokens_match(&t, &token) => Ok(resp),
            _ => {
                let mut err = ErrorResponse::new(Some("invalid token".into()));
                *err.status_mut() = StatusCode::UNAUTHORIZED;
                Err(err)
            }
        }
    };
    let ws = match tokio_tungstenite::accept_hdr_async(stream, check).await {
        Ok(ws) => ws,
        Err(e) => {
            tracing::debug!("dev websocket handshake failed: {e}");
            return;
        }
    };
    let (mut sink, mut source) = ws.split();
    let (client, mut outbound) = api.connect("dev-ws");
    let writer = tokio::spawn(async move {
        while let Some(env) = outbound.recv().await {
            let Ok(text) = serde_json::to_string(&env) else {
                continue;
            };
            if sink.send(Message::text(text)).await.is_err() {
                return;
            }
        }
    });
    while let Some(Ok(msg)) = source.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        match serde_json::from_str::<Envelope<ClientMsg>>(&text) {
            Ok(env) => api.handle(&client, env).await,
            Err(e) => tracing::debug!("bad dev websocket message: {e}"),
        }
    }
    drop(client);
    writer.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "abcd"));
        assert_eq!(random_token().len(), 64);
    }
}
