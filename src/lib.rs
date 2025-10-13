use anyhow::Result;
use near_primitives::types::AccountId;
use serde::{Deserialize, Serialize};
use std::env;

pub mod api;
pub mod batcher;
pub mod cache;
pub mod client;

#[derive(Clone)]
pub struct Config {
    pub rpc_urls: Vec<String>,
    pub account_id: AccountId,
    pub private_key: String,
    pub token_contract: AccountId,
    pub ft_transfer_gas: u64,
    pub storage_deposit_gas: u64,
    pub storage_deposit_amount: u128,
    pub max_transaction_gas: u64,
    pub batch_timeout_ms: u64,
    pub cache_ttl_seconds: u64,
    pub auto_storage_deposit: bool,
    pub max_retry_attempts: u8,
    pub api_port: u16,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let rpc_urls_str = env::var("RPC_URLS")?;
        let rpc_urls: Vec<String> = rpc_urls_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if rpc_urls.is_empty() {
            anyhow::bail!("RPC_URLS must contain at least one RPC endpoint");
        }

        Ok(Self {
            rpc_urls,
            account_id: env::var("ACCOUNT_ID")?.parse()?,
            private_key: env::var("PRIVATE_KEY")?,
            token_contract: env::var("TOKEN_CONTRACT")?.parse()?,
            ft_transfer_gas: env::var("FT_TRANSFER_GAS")
                .unwrap_or_else(|_| "3000000000000".to_string())
                .parse()?,
            storage_deposit_gas: env::var("STORAGE_DEPOSIT_GAS")
                .unwrap_or_else(|_| "5000000000000".to_string())
                .parse()?,
            storage_deposit_amount: env::var("STORAGE_DEPOSIT_AMOUNT")?.parse()?,
            max_transaction_gas: env::var("MAX_TRANSACTION_GAS")
                .unwrap_or_else(|_| "300000000000000".to_string())
                .parse()?,
            batch_timeout_ms: env::var("BATCH_TIMEOUT_MS")
                .unwrap_or_else(|_| "500".to_string())
                .parse()?,
            cache_ttl_seconds: env::var("CACHE_TTL_SECONDS")
                .unwrap_or_else(|_| "1800".to_string())
                .parse()?,
            auto_storage_deposit: env::var("AUTO_STORAGE_DEPOSIT")
                .unwrap_or_else(|_| "true".to_string())
                .parse::<bool>()
                .unwrap_or(true),
            max_retry_attempts: env::var("MAX_RETRY_ATTEMPTS")
                .unwrap_or_else(|_| "3".to_string())
                .parse()?,
            api_port: env::var("API_PORT")
                .unwrap_or_else(|_| "3030".to_string())
                .parse()?,
        })
    }

    pub fn max_transfers_per_batch(&self) -> usize {
        (self.max_transaction_gas / self.ft_transfer_gas) as usize
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TransferRequest {
    pub receiver_id: AccountId,
    pub amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TransferResponse {
    pub tx_hash: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HealthResponse {
    pub status: String,
}
