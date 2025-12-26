mod common;

use anyhow::Result;
use fast_ft_transfer::{TransferRequest, TransferResponse};
use serde_json::json;
use tokio::time::Duration;

#[tokio::test]
async fn test_transfer_without_storage_deposit() -> Result<()> {
    let ctx = common::setup_test_environment().await?;
    let config =
        common::create_test_config(&ctx.worker.rpc_addr(), &ctx.relayer, ctx.ft_contract.id())?;

    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Create a new user account without storage deposit
    let user3 = ctx.worker.dev_create_account().await?;

    // Verify user3 has no storage deposit initially
    let storage_balance = ctx
        .ft_contract
        .view("storage_balance_of")
        .args_json(json!({"account_id": user3.id()}))
        .await?
        .json::<Option<serde_json::Value>>()?;
    assert!(
        storage_balance.is_none(),
        "User3 should not have storage deposit initially"
    );

    // Send transfer request - should auto-deposit storage then transfer
    let client = reqwest::Client::new();
    let transfer_request = TransferRequest {
        receiver_id: user3.id().clone(),
        amount: "1000000000000000000".to_string(),
        memo: Some("Auto storage deposit test".to_string()),
    };

    let response = client
        .post(format!("{server_url}/transfer"))
        .json(&transfer_request)
        .send()
        .await?;

    assert_eq!(response.status(), 200);
    let transfer_response: TransferResponse = response.json().await?;
    eprintln!(
        "Transfer with auto storage deposit: tx_hash={}",
        transfer_response.tx_hash
    );

    // Wait for balance to update
    let final_balance = common::wait_for_balance(
        &ctx.ft_contract,
        user3.id(),
        "1000000000000000000",
        Duration::from_secs(5),
    )
    .await?;

    assert_eq!(final_balance, "1000000000000000000");

    // Verify storage deposit was created
    let storage_balance_after = ctx
        .ft_contract
        .view("storage_balance_of")
        .args_json(json!({"account_id": user3.id()}))
        .await?
        .json::<Option<serde_json::Value>>()?;
    assert!(
        storage_balance_after.is_some(),
        "User3 should have storage deposit after transfer"
    );

    Ok(())
}

#[tokio::test]
async fn test_transfer_without_storage_deposit_disabled() -> Result<()> {
    let ctx = common::setup_test_environment().await?;

    // Create config with auto_storage_deposit = false
    let config = common::create_test_config_with_auto_storage(
        &ctx.worker.rpc_addr(),
        &ctx.relayer,
        ctx.ft_contract.id(),
        false, // Disable auto storage deposit
    )?;

    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Create a new user account without storage deposit
    let user5 = ctx.worker.dev_create_account().await?;

    // Verify user5 has no storage deposit initially
    let storage_balance = ctx
        .ft_contract
        .view("storage_balance_of")
        .args_json(json!({"account_id": user5.id()}))
        .await?
        .json::<Option<serde_json::Value>>()?;
    assert!(
        storage_balance.is_none(),
        "User5 should not have storage deposit initially"
    );

    // Send transfer request - should be rejected with 400
    let client = reqwest::Client::new();
    let transfer_request = TransferRequest {
        receiver_id: user5.id().clone(),
        amount: "1000000000000000000".to_string(),
        memo: Some("Should fail - no auto storage".to_string()),
    };

    let response = client
        .post(format!("{server_url}/transfer"))
        .json(&transfer_request)
        .send()
        .await?;

    // Should return 400 Bad Request
    assert_eq!(response.status(), 400);

    let error_body: serde_json::Value = response.json().await?;
    let error_msg = error_body["error"].as_str().unwrap();

    // Verify error message mentions missing storage deposit
    assert!(
        error_msg.contains("does not have storage deposit"),
        "Error should mention storage deposit: {}",
        error_msg
    );
    assert!(
        error_msg.contains(user5.id().as_str()),
        "Error should mention the account: {}",
        error_msg
    );

    eprintln!("Successfully rejected transfer when auto_storage_deposit=false");

    // Verify user5 still has no balance (transfer was rejected)
    let balance = ctx
        .ft_contract
        .view("ft_balance_of")
        .args_json(json!({"account_id": user5.id()}))
        .await?
        .json::<String>()?;
    assert_eq!(
        balance, "0",
        "User5 should have zero balance after rejected transfer"
    );

    Ok(())
}

#[tokio::test]
async fn test_mixed_batch_transfer_and_storage() -> Result<()> {
    let ctx = common::setup_test_environment().await?;
    let config =
        common::create_test_config(&ctx.worker.rpc_addr(), &ctx.relayer, ctx.ft_contract.id())?;

    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Create new users without storage deposit
    let user3 = ctx.worker.dev_create_account().await?;
    let user4 = ctx.worker.dev_create_account().await?;

    let client = reqwest::Client::new();
    let mut handles = vec![];

    // Transfer to user1 (has storage) - 1 transfer action
    let client_clone = client.clone();
    let server_url_clone = server_url.clone();
    let user1_id = ctx.user1.id().clone();
    handles.push(tokio::spawn(async move {
        let request = TransferRequest {
            receiver_id: user1_id,
            amount: "100000000000000000".to_string(),
            memo: Some("To user with storage".to_string()),
        };
        client_clone
            .post(format!("{server_url_clone}/transfer"))
            .json(&request)
            .send()
            .await
    }));

    // Transfer to user3 (no storage) - 1 storage deposit + 1 transfer action
    let client_clone = client.clone();
    let server_url_clone = server_url.clone();
    let user3_id = user3.id().clone();
    handles.push(tokio::spawn(async move {
        let request = TransferRequest {
            receiver_id: user3_id,
            amount: "200000000000000000".to_string(),
            memo: Some("To user without storage".to_string()),
        };
        client_clone
            .post(format!("{server_url_clone}/transfer"))
            .json(&request)
            .send()
            .await
    }));

    // Transfer to user4 (no storage) - 1 storage deposit + 1 transfer action
    let client_clone = client.clone();
    let server_url_clone = server_url.clone();
    let user4_id = user4.id().clone();
    handles.push(tokio::spawn(async move {
        let request = TransferRequest {
            receiver_id: user4_id,
            amount: "300000000000000000".to_string(),
            memo: Some("To another user without storage".to_string()),
        };
        client_clone
            .post(format!("{server_url_clone}/transfer"))
            .json(&request)
            .send()
            .await
    }));

    // Wait for all requests
    for handle in handles {
        let response = handle.await??;
        assert_eq!(response.status(), 200);
    }

    // Verify all balances
    let user1_balance = common::wait_for_balance(
        &ctx.ft_contract,
        ctx.user1.id(),
        "100000000000000000",
        Duration::from_secs(5),
    )
    .await?;
    assert_eq!(user1_balance, "100000000000000000");

    let user3_balance = common::wait_for_balance(
        &ctx.ft_contract,
        user3.id(),
        "200000000000000000",
        Duration::from_secs(5),
    )
    .await?;
    assert_eq!(user3_balance, "200000000000000000");

    let user4_balance = common::wait_for_balance(
        &ctx.ft_contract,
        user4.id(),
        "300000000000000000",
        Duration::from_secs(5),
    )
    .await?;
    assert_eq!(user4_balance, "300000000000000000");

    eprintln!("Successfully processed mixed batch with storage deposits and transfers");

    Ok(())
}
