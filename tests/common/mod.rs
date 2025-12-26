use anyhow::Result;
use fast_ft_transfer::{
    Config, api::create_router, batcher::Batcher, cache::AccountValidationCache, client::ClientPool,
};
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{Duration, sleep};

// You can download a pre-compiled FT contract from:
// https://github.com/near-examples/FT/releases or build it yourself
// For this test, we'll use a WASM file path that should be provided
pub const FT_WASM_PATH: &str = "./res/fungible_token.wasm";

/// Helper struct to hold test context
#[allow(dead_code)]
pub struct TestContext {
    pub worker: Worker<near_workspaces::network::Sandbox>,
    pub ft_contract: Contract,
    pub owner: Account,
    pub relayer: Account,
    pub user1: Account,
    pub user2: Account,
}

/// Initialize the sandbox environment and deploy FT contract
pub async fn setup_test_environment() -> Result<TestContext> {
    // Start sandbox
    let worker = near_workspaces::sandbox().await?;

    // Create test accounts
    let owner = worker.dev_create_account().await?;
    let relayer = worker.dev_create_account().await?;
    let user1 = worker.dev_create_account().await?;
    let user2 = worker.dev_create_account().await?;

    // Load FT contract WASM
    let ft_wasm = load_ft_wasm().await?;

    // Deploy FT contract
    let ft_contract = owner.deploy(&ft_wasm).await?.result;

    // Initialize FT contract
    ft_contract
        .call("new_default_meta")
        .args_json(json!({
            "owner_id": owner.id(),
            "total_supply": "1000000000000000000000000" // 1M tokens with 18 decimals
        }))
        .transact()
        .await?
        .into_result()?;

    // Register relayer account for storage
    owner
        .call(ft_contract.id(), "storage_deposit")
        .args_json(json!({
            "account_id": relayer.id()
        }))
        .deposit(near_workspaces::types::NearToken::from_millinear(25)) // 0.025 NEAR
        .transact()
        .await?
        .into_result()?;

    // Register user accounts for storage
    for user in [&user1, &user2] {
        owner
            .call(ft_contract.id(), "storage_deposit")
            .args_json(json!({
                "account_id": user.id()
            }))
            .deposit(near_workspaces::types::NearToken::from_millinear(25))
            .transact()
            .await?
            .into_result()?;
    }

    // Transfer tokens to relayer account
    owner
        .call(ft_contract.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": relayer.id(),
            "amount": "500000000000000000000000", // 500k tokens
            "memo": "Initial funding for relayer"
        }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .transact()
        .await?
        .into_result()?;

    Ok(TestContext {
        worker,
        ft_contract,
        owner,
        relayer,
        user1,
        user2,
    })
}

/// Load the FT contract WASM from file system or download if not present
pub async fn load_ft_wasm() -> Result<Vec<u8>> {
    // First try to load from file system
    if let Ok(wasm) = tokio::fs::read(FT_WASM_PATH).await {
        return Ok(wasm);
    }

    anyhow::bail!(
        "Failed to find FT WASM. Please build it manually:\n\
                      1. Clone https://github.com/near-examples/FT\n\
                      2. Build it according to repo README.md\n\
                      3. Copy to ./res/fungible_token.wasm"
    );
}

/// Create a Config for testing with sandbox RPC URL
pub fn create_test_config(
    sandbox_url: &str,
    relayer_account: &Account,
    ft_contract: &AccountId,
) -> Result<Config> {
    create_test_config_with_auto_storage(sandbox_url, relayer_account, ft_contract, true)
}

/// Create a Config for testing with configurable auto_storage_deposit
pub fn create_test_config_with_auto_storage(
    sandbox_url: &str,
    relayer_account: &Account,
    ft_contract: &AccountId,
    auto_storage_deposit: bool,
) -> Result<Config> {
    create_test_config_with_validation(
        sandbox_url,
        relayer_account,
        ft_contract,
        auto_storage_deposit,
        true,
    )
}

/// Create a Config for testing with configurable validation settings
pub fn create_test_config_with_validation(
    sandbox_url: &str,
    relayer_account: &Account,
    ft_contract: &AccountId,
    auto_storage_deposit: bool,
    ensure_receiver_account_exists: bool,
) -> Result<Config> {
    Ok(Config {
        rpc_urls: vec![sandbox_url.to_string()],
        account_id: relayer_account.id().clone(),
        private_key: relayer_account.secret_key().to_string(),
        token_contract: ft_contract.clone(),
        ft_transfer_gas: 10_000_000_000_000,
        storage_deposit_gas: 5_000_000_000_000,
        storage_deposit_amount: 1_250_000_000_000_000_000_000,
        max_transaction_gas: 300_000_000_000_000,
        batch_timeout_ms: 500,
        cache_ttl_seconds: 1800,
        auto_storage_deposit,
        max_retry_attempts: 3,
        api_port: 0, // Use random available port
        ensure_receiver_account_exists,
    })
}

/// Start the API server with batcher in background
pub async fn start_api_server(config: Config) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let client_pool = Arc::new(ClientPool::new(&config)?);

    // Create validation cache
    let cache = Arc::new(AccountValidationCache::new(Duration::from_secs(
        config.cache_ttl_seconds,
    )));

    let (batcher, batcher_tx) = Batcher::new(
        client_pool.clone(),
        config.token_contract.clone(),
        config.clone(),
    );

    // Spawn batcher task
    let batcher_handle = tokio::spawn(async move {
        if let Err(e) = batcher.run().await {
            eprintln!("Batcher error: {e:?}");
        }
    });

    // Create API router
    let app = create_router(
        batcher_tx,
        client_pool,
        cache,
        config.token_contract.clone(),
        config.clone(),
    );

    // Bind to random port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server_url = format!("http://{addr}");

    // Spawn server task
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // Give server time to start
    sleep(Duration::from_millis(100)).await;

    Ok((server_url, batcher_handle))
}

/// Helper to check FT balance
pub async fn get_ft_balance(contract: &Contract, account_id: &AccountId) -> Result<String> {
    let result = contract
        .view("ft_balance_of")
        .args_json(json!({
            "account_id": account_id
        }))
        .await?;

    Ok(result.json::<String>()?)
}

/// Wait for balance to reach expected value with timeout
pub async fn wait_for_balance(
    contract: &Contract,
    account_id: &AccountId,
    expected_balance: &str,
    timeout: Duration,
) -> Result<String> {
    let start = std::time::Instant::now();

    loop {
        let balance = get_ft_balance(contract, account_id).await?;

        if balance == expected_balance || start.elapsed() >= timeout {
            return Ok(balance);
        }

        sleep(Duration::from_millis(200)).await;
    }
}
