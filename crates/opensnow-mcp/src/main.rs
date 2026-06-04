use std::net::SocketAddr;

use anyhow::Result;
use opensnow_core::{OpenSnowConfig, OpenSnowEngine};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Load standard OpenSnow config (opensnow.toml) and construct an engine
    // pointing at the configured catalog + warehouse.
    let config = OpenSnowConfig::load();
    let engine =
        OpenSnowEngine::from_config_and_catalog(config.storage.clone(), &config.catalog.path);

    // For now, listen on 0.0.0.0:8090 by default.
    let addr: SocketAddr = ([0, 0, 0, 0], 8090).into();
    opensnow_mcp::serve(engine, addr).await
}
