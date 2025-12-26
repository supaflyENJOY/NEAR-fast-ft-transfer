mod common;

use anyhow::Result;
use fast_ft_transfer::{TransferRequest, TransferResponse};
use near_workspaces::AccountId;
use tokio::time::Duration;

#[tokio::test]
async fn test_transfer_to_nonexistent_account() -> Result<()> {
    let ctx = common::setup_test_environment().await?;
    let config =
        common::create_test_config(&ctx.worker.rpc_addr(), &ctx.relayer, ctx.ft_contract.id())?;

    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Try to transfer to a non-existent account
    let client = reqwest::Client::new();
    let nonexistent_account: AccountId = "this-account-does-not-exist.test".parse()?;

    let transfer_request = TransferRequest {
        receiver_id: nonexistent_account.clone(),
        amount: "1000000000000000000".to_string(),
        memo: Some("Should fail".to_string()),
    };

    let response = client
        .post(format!("{server_url}/transfer"))
        .json(&transfer_request)
        .send()
        .await?;

    // Should return 400 Bad Request
    assert_eq!(response.status(), 400);

    let error_body: serde_json::Value = response.json().await?;
    assert!(
        error_body["error"]
            .as_str()
            .unwrap()
            .contains("does not exist")
    );

    eprintln!("Successfully rejected transfer to nonexistent account");

    Ok(())
}

#[tokio::test]
async fn test_cached_validation() -> Result<()> {
    let ctx = common::setup_test_environment().await?;
    let config =
        common::create_test_config(&ctx.worker.rpc_addr(), &ctx.relayer, ctx.ft_contract.id())?;

    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    let client = reqwest::Client::new();

    // First transfer - will populate cache
    let transfer_request = TransferRequest {
        receiver_id: ctx.user1.id().clone(),
        amount: "100000000000000000".to_string(),
        memo: Some("First transfer".to_string()),
    };

    let response1 = client
        .post(format!("{server_url}/transfer"))
        .json(&transfer_request)
        .send()
        .await?;
    assert_eq!(response1.status(), 200);

    // Second transfer to same user - should use cache (no RPC calls for validation)
    let transfer_request2 = TransferRequest {
        receiver_id: ctx.user1.id().clone(),
        amount: "200000000000000000".to_string(),
        memo: Some("Second transfer (cached)".to_string()),
    };

    let response2 = client
        .post(format!("{server_url}/transfer"))
        .json(&transfer_request2)
        .send()
        .await?;
    assert_eq!(response2.status(), 200);

    // Wait for final balance
    let final_balance = common::wait_for_balance(
        &ctx.ft_contract,
        ctx.user1.id(),
        "300000000000000000",
        Duration::from_secs(5),
    )
    .await?;

    assert_eq!(final_balance, "300000000000000000");
    eprintln!("Successfully validated cache reuse for repeated transfers");

    Ok(())
}

#[tokio::test]
async fn test_transfer_to_nonexistent_account_validation_disabled() -> Result<()> {
    let ctx = common::setup_test_environment().await?;

    // Create config with account validation disabled
    let config = common::create_test_config_with_validation(
        &ctx.worker.rpc_addr(),
        &ctx.relayer,
        ctx.ft_contract.id(),
        true,  // auto_storage_deposit
        false, // ensure_receiver_account_exists
    )?;

    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Try to transfer to a non-existent account (should succeed now)
    let client = reqwest::Client::new();
    let nonexistent_account: AccountId = "this-account-does-not-exist.test".parse()?;

    let transfer_request = TransferRequest {
        receiver_id: nonexistent_account.clone(),
        amount: "1000000000000000000".to_string(),
        memo: Some("Should succeed when validation disabled".to_string()),
    };

    let response = client
        .post(format!("{server_url}/transfer"))
        .json(&transfer_request)
        .send()
        .await?;

    // Should succeed (200) when account validation is disabled
    assert_eq!(response.status(), 200);

    let transfer_response: TransferResponse = response.json().await?;
    assert!(!transfer_response.tx_hash.is_empty());

    Ok(())
}
