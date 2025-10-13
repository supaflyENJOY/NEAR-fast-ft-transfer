mod common;

use anyhow::Result;
use fast_ft_transfer::{TransferRequest, TransferResponse};
use tokio::time::Duration;

#[tokio::test]
async fn test_single_transfer() -> Result<()> {
    let ctx = common::setup_test_environment().await?;
    let config = common::create_test_config(&ctx.worker.rpc_addr(), &ctx.relayer, ctx.ft_contract.id())?;

    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Get initial balance
    let initial_balance = common::get_ft_balance(&ctx.ft_contract, ctx.user1.id()).await?;
    assert_eq!(initial_balance, "0");

    // Send transfer request
    let client = reqwest::Client::new();
    let transfer_request = TransferRequest {
        receiver_id: ctx.user1.id().clone(),
        amount: "1000000000000000000".to_string(), // 1 token
        memo: Some("Test transfer".to_string()),
    };

    let response = client
        .post(format!("{server_url}/transfer"))
        .json(&transfer_request)
        .send()
        .await?;

    assert_eq!(response.status(), 200);
    let transfer_response: TransferResponse = response.json().await?;
    assert!(!transfer_response.tx_hash.is_empty());
    eprintln!(
        "Single transfer response: tx_hash={}",
        transfer_response.tx_hash
    );

    // Wait for balance to update (up to 5 seconds)
    let final_balance = common::wait_for_balance(
        &ctx.ft_contract,
        ctx.user1.id(),
        "1000000000000000000",
        Duration::from_secs(5),
    )
    .await?;

    assert_eq!(final_balance, "1000000000000000000");

    Ok(())
}
