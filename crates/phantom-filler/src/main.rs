//! Phantom Filler — high-performance intent execution engine for DeFi.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("phantom-filler starting");
    Ok(())
}
