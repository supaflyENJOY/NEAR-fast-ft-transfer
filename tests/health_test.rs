mod common;

use anyhow::Result;

#[tokio::test]
async fn test_health_endpoint() -> Result<()> {
    let ctx = common::setup_test_environment().await?;
    let config = common::create_test_config(&ctx.worker.rpc_addr(), &ctx.relayer, ctx.ft_contract.id())?;

    let (server_url, _batcher_handle) = common::start_api_server(config).await?;

    // Test health endpoint
    let client = reqwest::Client::new();
    let response = client.get(format!("{server_url}/health")).send().await?;

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["status"], "ok");

    Ok(())
}
