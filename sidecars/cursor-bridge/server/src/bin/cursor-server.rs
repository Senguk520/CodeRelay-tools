//! Starts the CodeRelay Cursor bridge executable.
//!
//! The bridge is a sidecar of the CodeRelay desktop app: CodeRelay owns the
//! process lifetime and reads the ready line this binary writes to stdout.
use cursor_server::{App, Config, Result};
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cursor_server=info".into()),
        )
        // Logs go to stderr: stdout carries the ready handshake consumed by
        // CodeRelay's process manager, and mixing the two would make the
        // protocol depend on log formatting.
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    App::new(Config::from_env()?).await?.serve_announcing_ready().await
}
