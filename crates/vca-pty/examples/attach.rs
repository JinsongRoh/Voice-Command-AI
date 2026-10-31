//! Attaches the current terminal to an attach endpoint.
//!
//! Usage: `cargo run -p vca-pty --example attach -- <endpoint>`
//! Press Ctrl-] then `d` to detach.

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let Some(endpoint) = std::env::args_os().nth(1) else {
        eprintln!("usage: attach <endpoint>");
        std::process::exit(2);
    };
    let outcome = vca_pty::attach_stdio(std::path::Path::new(&endpoint)).await?;
    println!("\r\noutcome: {outcome:?}\r");
    match outcome {
        vca_pty::AttachOutcome::Exited { code } => std::process::exit(code.unwrap_or(1)),
        _ => Ok(()),
    }
}
