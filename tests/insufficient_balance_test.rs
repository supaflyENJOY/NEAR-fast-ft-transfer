mod common;

use anyhow::Result;
use fast_ft_transfer::{TransferRequest, TransferResponse};
use serde_json::json;
use tokio::time::{Duration, sleep};

#[tokio::test]
async fn test_batch_with_insufficient_balance() -> Result<()> {
    let ctx = common::setup_test_environment().await?;

    // Create a NEW relayer with limited balance instead of using the pre-funded one
    let limited_relayer = ctx.worker.dev_create_account().await?;

    // Register limited relayer for storage
    ctx.owner
        .call(ctx.ft_contract.id(), "storage_deposit")
        .args_json(json!({
            "account_id": limited_relayer.id()
        }))
        .deposit(near_workspaces::types::NearToken::from_millinear(25))
        .transact()
        .await?
        .into_result()?;

    // Transfer only 100 tokens to limited relayer (much less than the original 500k)
    ctx.owner
        .call(ctx.ft_contract.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": limited_relayer.id(),
            "amount": "100000000000000000000", // 100 tokens with 18 decimals
            "memo": "Limited funding for insufficient balance test"
        }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .transact()
        .await?
        .into_result()?;

    // Verify limited relayer balance
    let relayer_balance = common::get_ft_balance(&ctx.ft_contract, limited_relayer.id()).await?;
    eprintln!(
        "Limited relayer initial balance: {} (100 tokens)",
        relayer_balance
    );
    assert_eq!(relayer_balance, "100000000000000000000");

    // Create 3 test users and register their storage
    let test_user1 = ctx.worker.dev_create_account().await?;
    let test_user2 = ctx.worker.dev_create_account().await?;
    let test_user3 = ctx.worker.dev_create_account().await?;

    for user in [&test_user1, &test_user2, &test_user3] {
        ctx.owner
            .call(ctx.ft_contract.id(), "storage_deposit")
            .args_json(json!({
                "account_id": user.id()
            }))
            .deposit(near_workspaces::types::NearToken::from_millinear(25))
            .transact()
            .await?
            .into_result()?;
    }

    // Create config with the LIMITED relayer
    let config = common::create_test_config(
        &ctx.worker.rpc_addr(),
        &limited_relayer,
        ctx.ft_contract.id(),
    )?;
    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Send 3 concurrent transfers that exceed the relayer's balance
    // Each transfer is 40 tokens, total = 120 tokens > 100 available
    let client = reqwest::Client::new();
    let mut handles = vec![];

    let users = vec![
        (test_user1.id().clone(), "40000000000000000000"), // 40 tokens
        (test_user2.id().clone(), "40000000000000000000"), // 40 tokens
        (test_user3.id().clone(), "40000000000000000000"), // 40 tokens
    ];

    eprintln!("\n=== Sending 3 concurrent transfers (40 tokens each = 120 total) ===");
    eprintln!("Relayer only has 100 tokens available");
    eprintln!("Expected: First batch fails at action 2 (third transfer - insufficient balance)");
    eprintln!("Expected: Actions 0 & 1 retry successfully, action 2 gets error\n");

    for (i, (receiver_id, amount)) in users.iter().enumerate() {
        let client = client.clone();
        let server_url = server_url.clone();
        let receiver_id = receiver_id.clone();
        let amount = amount.to_string();

        let handle = tokio::spawn(async move {
            let transfer_request = TransferRequest {
                receiver_id,
                amount,
                memo: Some(format!("Transfer #{} (should fail collectively)", i + 1)),
            };

            if i == 2 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            eprintln!("Sending transfer #{}", i + 1);
            client
                .post(format!("{server_url}/transfer"))
                .json(&transfer_request)
                .send()
                .await
        });

        handles.push(handle);
    }

    // Wait for all requests to complete and capture responses
    let mut tx_hashes = vec![];
    let mut statuses = vec![];

    for (i, handle) in handles.into_iter().enumerate() {
        let response = handle.await??;
        let status = response.status();
        statuses.push(status);

        eprintln!("\n--- Transfer #{} Response ---", i + 1);
        eprintln!("HTTP Status: {}", status);

        if status.is_success() {
            let transfer_response: TransferResponse = response.json().await?;
            eprintln!("TX Hash: {}", transfer_response.tx_hash);
            tx_hashes.push(transfer_response.tx_hash);
        } else {
            let error_body: serde_json::Value = response.json().await?;
            eprintln!("Error: {:?}", error_body);
        }
    }

    eprintln!("\n=== OBSERVATION: API Response Analysis ===");
    let success_count = statuses.iter().filter(|s| s.is_success()).count();
    let error_count = statuses.len() - success_count;
    eprintln!(
        "Number of successful HTTP responses (200): {}",
        success_count
    );
    eprintln!("Number of error HTTP responses (4xx/5xx): {}", error_count);

    if !tx_hashes.is_empty() {
        eprintln!(
            "Transfers 1 & 2 got tx_hash (retried successfully): {}",
            tx_hashes[0]
        );
        eprintln!("Transfer 3 got error (failed action)");
        eprintln!("NOTE: With retry logic, partial batch failures allow other actions to succeed");
    } else {
        eprintln!("✓ No tx_hashes returned - all requests failed");
    }

    // Check actual balances - they should ALL be 0 because the transaction failed
    eprintln!("\n=== OBSERVATION: Actual Balance Check (After Transaction Execution) ===");

    let user1_balance = common::get_ft_balance(&ctx.ft_contract, &users[0].0).await?;
    let user2_balance = common::get_ft_balance(&ctx.ft_contract, &users[1].0).await?;
    let user3_balance = common::get_ft_balance(&ctx.ft_contract, &users[2].0).await?;
    let relayer_balance_after =
        common::get_ft_balance(&ctx.ft_contract, limited_relayer.id()).await?;

    eprintln!("User 1 balance: {} (expected: 40)", user1_balance);
    eprintln!("User 2 balance: {} (expected: 40)", user2_balance);
    eprintln!("User 3 balance: {} (expected: 0)", user3_balance);
    eprintln!(
        "Relayer balance after: {} (expected: 20)",
        relayer_balance_after
    );

    // Verify balances after retry
    assert_eq!(
        user1_balance, "40000000000000000000",
        "User 1 should have 40 tokens after retry"
    );
    assert_eq!(
        user2_balance, "40000000000000000000",
        "User 2 should have 40 tokens after retry"
    );
    assert_eq!(
        user3_balance, "0",
        "User 3 should have 0 tokens - transfer failed"
    );
    assert_eq!(
        relayer_balance_after, "20000000000000000000",
        "Relayer should have 20 tokens left (100 - 40 - 40)"
    );

    eprintln!("\n=== CONCLUSION ===");
    if error_count == 1 {
        eprintln!("✅ Retry logic working: Action 2 failed, Actions 0 & 1 retried successfully");
        eprintln!("✅ Partial batch failure correctly identified (ActionError index: Some(2))");
        eprintln!("✅ Failed action got immediate error, others succeeded after retry");
    } else {
        eprintln!("❌ UNEXPECTED: Error count = {}, expected 1", error_count);
    }
    eprintln!("📊 Final state: Users 1 & 2 have 40 tokens, User 3 has 0 (as expected)");
    eprintln!("💡 Retry logic enables partial success even when batch fails\n");

    Ok(())
}

#[tokio::test]
async fn test_partial_batch_failure_with_retry() -> Result<()> {
    let ctx = common::setup_test_environment().await?;

    // Create a NEW relayer with limited balance (100 tokens)
    let limited_relayer = ctx.worker.dev_create_account().await?;

    // Register limited relayer for storage
    ctx.owner
        .call(ctx.ft_contract.id(), "storage_deposit")
        .args_json(json!({
            "account_id": limited_relayer.id()
        }))
        .deposit(near_workspaces::types::NearToken::from_millinear(25))
        .transact()
        .await?
        .into_result()?;

    // Transfer 100 tokens to limited relayer
    ctx.owner
        .call(ctx.ft_contract.id(), "ft_transfer")
        .args_json(json!({
            "receiver_id": limited_relayer.id(),
            "amount": "100000000000000000000", // 100 tokens
            "memo": "Limited funding for partial failure test"
        }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .transact()
        .await?
        .into_result()?;

    // Verify relayer balance
    let relayer_balance = common::get_ft_balance(&ctx.ft_contract, limited_relayer.id()).await?;
    eprintln!(
        "Limited relayer initial balance: {} (100 tokens)",
        relayer_balance
    );
    assert_eq!(relayer_balance, "100000000000000000000");

    // Create 3 test users and register their storage
    let test_user1 = ctx.worker.dev_create_account().await?;
    let test_user2 = ctx.worker.dev_create_account().await?;
    let test_user3 = ctx.worker.dev_create_account().await?;

    for user in [&test_user1, &test_user2, &test_user3] {
        ctx.owner
            .call(ctx.ft_contract.id(), "storage_deposit")
            .args_json(json!({
                "account_id": user.id()
            }))
            .deposit(near_workspaces::types::NearToken::from_millinear(25))
            .transact()
            .await?
            .into_result()?;
    }

    // Create config with the LIMITED relayer
    let config = common::create_test_config(
        &ctx.worker.rpc_addr(),
        &limited_relayer,
        ctx.ft_contract.id(),
    )?;
    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Send 3 concurrent transfers: 30, 30, 50 (total 110 > 100 available)
    let client = reqwest::Client::new();
    let mut handles = vec![];

    let transfers = vec![
        (test_user1.id().clone(), "30000000000000000000"), // 30 tokens
        (test_user2.id().clone(), "30000000000000000000"), // 30 tokens
        (test_user3.id().clone(), "50000000000000000000"), // 50 tokens - will fail
    ];

    eprintln!("\n=== Sending 3 concurrent transfers: 30, 30, 50 tokens (total 110 > 100) ===");
    eprintln!("Expected: First batch fails at action 2 (third transfer)");
    eprintln!("Expected: Actions 0 & 1 automatically retry in next batch");
    eprintln!("Expected: Action 2 gets immediate error\n");

    for (i, (receiver_id, amount)) in transfers.iter().enumerate() {
        let client = client.clone();
        let server_url = server_url.clone();
        let receiver_id = receiver_id.clone();
        let amount = amount.to_string();

        let handle = tokio::spawn(async move {
            let transfer_request = TransferRequest {
                receiver_id,
                amount,
                memo: Some(format!("Transfer #{}", i + 1)),
            };

            eprintln!("Sending transfer #{}", i + 1);
            client
                .post(format!("{server_url}/transfer"))
                .json(&transfer_request)
                .send()
                .await
        });

        handles.push(handle);
    }

    // Wait for all requests to complete
    let mut responses = vec![];
    for handle in handles {
        let response = handle.await??;
        responses.push(response);
    }

    eprintln!("\n=== OBSERVATION: API Response Analysis ===");

    // Check statuses
    for (i, response) in responses.iter().enumerate() {
        let status = response.status();
        eprintln!("Transfer #{} status: {}", i + 1, status);
    }

    // Transfer 3 (index 2) should fail immediately
    assert_eq!(
        responses[2].status(),
        500,
        "Transfer 3 should fail with 500"
    );

    // Transfers 1 & 2 should eventually succeed (after retry)
    // Note: They might return success or might still be processing
    let status1 = responses[0].status();
    let status2 = responses[1].status();

    eprintln!(
        "\nTransfer 1 status: {} (should be 200 after retry)",
        status1
    );
    eprintln!("Transfer 2 status: {} (should be 200 after retry)", status2);

    // Wait a bit for retries to complete
    eprintln!("\n=== Waiting 5 seconds for retries to complete ===");
    sleep(Duration::from_secs(5)).await;

    // Check final balances
    eprintln!("\n=== OBSERVATION: Final Balance Check ===");

    let user1_balance = common::get_ft_balance(&ctx.ft_contract, &transfers[0].0).await?;
    let user2_balance = common::get_ft_balance(&ctx.ft_contract, &transfers[1].0).await?;
    let user3_balance = common::get_ft_balance(&ctx.ft_contract, &transfers[2].0).await?;
    let relayer_balance_after =
        common::get_ft_balance(&ctx.ft_contract, limited_relayer.id()).await?;

    eprintln!("User 1 balance: {} (expected: 30)", user1_balance);
    eprintln!("User 2 balance: {} (expected: 30)", user2_balance);
    eprintln!("User 3 balance: {} (expected: 0)", user3_balance);
    eprintln!("Relayer balance: {} (expected: 40)", relayer_balance_after);

    // Verify final state
    assert_eq!(
        user1_balance, "30000000000000000000",
        "User 1 should have 30 tokens after retry"
    );
    assert_eq!(
        user2_balance, "30000000000000000000",
        "User 2 should have 30 tokens after retry"
    );
    assert_eq!(user3_balance, "0", "User 3 should have 0 tokens (failed)");
    assert_eq!(
        relayer_balance_after, "40000000000000000000",
        "Relayer should have 40 tokens left"
    );

    eprintln!("\n=== CONCLUSION ===");
    eprintln!("✅ Partial batch failure correctly identified failed action");
    eprintln!("✅ Actions 0 & 1 successfully retried in next batch");
    eprintln!("✅ Action 2 (failed) got immediate error response");
    eprintln!("✅ Final state: Users 1 & 2 have tokens, User 3 does not\n");

    Ok(())
}
