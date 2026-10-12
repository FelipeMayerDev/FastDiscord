//! Standalone relay. Terminate the process to end all in-memory sessions.
use anyhow::{Context, Result};
#[tokio::main]
async fn main() -> Result<()> {
    let key = std::env::var("FASTDISCORD_DJ_KEY")
        .context("Set FASTDISCORD_DJ_KEY (16–256 characters)")?;
    let address = std::env::var("FASTDISCORD_DJ_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    eprintln!("FastDiscord DJ listening on {}", listener.local_addr()?);
    fastdiscord::backend::dj_server::serve(listener, key).await
}
