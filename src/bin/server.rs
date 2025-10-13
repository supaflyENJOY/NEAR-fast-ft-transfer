use anyhow::Result;
use dotenv::dotenv;
use log::info;
use std::{sync::Arc, time::Duration};

use fast_ft_transfer::{
    Config, api::create_router, batcher::Batcher, cache::AccountValidationCache, client::ClientPool,
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let config = Config::from_env()?;
    info!("Starting FT Transfer API service");
    info!("Token contract: {}", config.token_contract);
    info!(
        "Max batch size: {} transfers",
        config.max_transfers_per_batch()
    );
    info!("Batch timeout: {}ms", config.batch_timeout_ms);

    let client_pool = Arc::new(ClientPool::new(&config)?);

    let cache = Arc::new(AccountValidationCache::new(Duration::from_secs(
        config.cache_ttl_seconds,
    )));
    info!(
        "Account validation cache TTL: {}s",
        config.cache_ttl_seconds
    );

    let (batcher, batcher_tx) = Batcher::new(
        client_pool.clone(),
        config.token_contract.clone(),
        config.clone(),
    );

    tokio::spawn(async move {
        if let Err(e) = batcher.run().await {
            log::error!("Batcher error: {e:?}");
        }
    });

    let app = create_router(
        batcher_tx,
        client_pool,
        cache,
        config.token_contract.clone(),
        config.clone(),
    );

    let addr = format!("0.0.0.0:{}", config.api_port);
    info!("API server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
