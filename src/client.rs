//! # NEAR JSON-RPC Client with Automatic Retry Logic
//!
//! This module provides a NEAR blockchain client with built-in exponential backoff
//! retry logic for handling rate limits and transient errors.
//!
//! ## Features
//! - Automatic retry on HTTP 403 (Forbidden) errors
//! - Automatic retry on HTTP 429 (Too Many Requests) errors
//! - Automatic retry on "client has exceeded the rate limit" error messages
//! - Exponential backoff with configurable delays (100ms to 30s)

use anyhow::Result;
use log::{info, warn};
use near_crypto::{InMemorySigner, Signer};
use near_jsonrpc_client::{
    JsonRpcClient,
    methods::{
        broadcast_tx_commit::RpcBroadcastTxCommitRequest,
        query::{RpcQueryRequest, RpcQueryResponse},
    },
};
use near_jsonrpc_primitives::types::query::QueryResponseKind;
use near_primitives::{
    action::Action,
    hash::CryptoHash,
    transaction::{Transaction, TransactionV0},
    types::{AccountId, BlockReference},
    views::{FinalExecutionOutcomeView, QueryRequest},
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;

use crate::Config;

pub struct Client {
    jsonrpc_client: JsonRpcClient,
    rpc_url: String,
    signer: InMemorySigner,
    block_hash: Arc<RwLock<CryptoHash>>,
}

impl Client {
    fn new_with_shared(
        rpc_url: &str,
        signer: InMemorySigner,
        block_hash: Arc<RwLock<CryptoHash>>,
    ) -> Self {
        Self {
            jsonrpc_client: JsonRpcClient::connect(rpc_url),
            rpc_url: rpc_url.to_string(),
            signer,
            block_hash,
        }
    }

    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Make a query RPC call with exponential backoff retry logic
    ///
    /// This method automatically retries failed requests under the following conditions:
    /// - HTTP 403 (Forbidden) status codes
    /// - HTTP 429 (Too Many Requests) status codes
    /// - Error messages containing "rate limit" or "exceeded the rate limit"
    /// - Other transient HTTP errors (5xx status codes, connection issues, timeouts)
    ///
    /// Retry behavior:
    /// - Maximum 50 retry attempts
    /// - Exponential backoff starting at 100ms, capped at 10 seconds
    /// - Example delays: 100ms, 200ms, 400ms, 800ms, 1600ms, ... doubling each time until
    ///   reaching 10s, after which all remaining retries use the 10s cap (up to 15 attempts)
    pub async fn call<M>(
        &self,
        method: M,
    ) -> near_jsonrpc_client::MethodCallResult<M::Response, M::Error>
    where
        M: near_jsonrpc_client::methods::RpcMethod,
    {
        const MAX_RETRIES: u32 = 15;
        const BASE_DELAY_MS: u64 = 100;
        const MAX_DELAY_MS: u64 = 10000; // 10 seconds

        let mut attempt = 0;

        loop {
            match self.jsonrpc_client.call(&method).await {
                Ok(response) => return Ok(response),
                Err(err) if err.handler_error().is_some() => {
                    return Err(err.into());
                }
                Err(err) => {
                    attempt += 1;

                    if attempt >= MAX_RETRIES {
                        return Err(err.into());
                    }

                    let delay_ms =
                        std::cmp::min(BASE_DELAY_MS * 2_u64.pow(attempt - 1), MAX_DELAY_MS);

                    warn!(
                        "RPC call failed (attempt {}/{}), retrying in {}ms",
                        attempt, MAX_RETRIES, delay_ms
                    );

                    sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }

    async fn get_access_key_query(&self) -> Result<RpcQueryResponse> {
        let rpc_request = RpcQueryRequest {
            block_reference: BlockReference::latest(),
            request: QueryRequest::ViewAccessKey {
                account_id: self.signer.account_id.clone(),
                public_key: self.signer.public_key.clone(),
            },
        };

        self.call(rpc_request).await.map_err(Into::into)
    }

    /// Check if an account exists on the blockchain
    pub async fn view_account(&self, account_id: &AccountId) -> Result<bool> {
        let rpc_request = RpcQueryRequest {
            block_reference: BlockReference::latest(),
            request: QueryRequest::ViewAccount {
                account_id: account_id.clone(),
            },
        };

        match self.call(rpc_request).await {
            Ok(_) => Ok(true),
            Err(e) => {
                let error_str = e.to_string();
                if error_str.contains("does not exist") {
                    Ok(false)
                } else {
                    Err(e.into())
                }
            }
        }
    }

    pub async fn check_storage_deposit(
        &self,
        account_id: &AccountId,
        contract_id: &AccountId,
    ) -> Result<bool> {
        let rpc_request = RpcQueryRequest {
            block_reference: BlockReference::latest(),
            request: QueryRequest::CallFunction {
                account_id: contract_id.clone(),
                method_name: "storage_balance_of".to_string(),
                args: serde_json::to_vec(&serde_json::json!({
                    "account_id": account_id.to_string()
                }))?
                .into(),
            },
        };

        match self.call(rpc_request).await {
            Ok(response) => {
                let QueryResponseKind::CallResult(result) = response.kind else {
                    anyhow::bail!("Unexpected query response kind");
                };

                let balance: Option<serde_json::Value> = serde_json::from_slice(&result.result)?;
                Ok(balance.is_some())
            }
            Err(e) => {
                let error_str = e.to_string();
                if error_str.contains("MethodNotFound") || error_str.contains("does not exist") {
                    Ok(false)
                } else {
                    Err(e.into())
                }
            }
        }
    }

    pub async fn reset_block_hash(&self) -> Result<()> {
        let access_key_query_response = self.get_access_key_query().await?;
        *self.block_hash.write().await = access_key_query_response.block_hash;
        Ok(())
    }

    async fn send_batch_transaction(
        &self,
        token: &AccountId,
        actions: Vec<Action>,
        nonce: u64,
    ) -> Result<FinalExecutionOutcomeView> {
        let transaction = Transaction::V0(TransactionV0 {
            signer_id: self.signer.account_id.clone(),
            public_key: self.signer.public_key.clone(),
            nonce,
            receiver_id: token.clone(),
            block_hash: *self.block_hash.read().await,
            actions,
        });

        let request = RpcBroadcastTxCommitRequest {
            signed_transaction: transaction
                .sign(&near_crypto::Signer::InMemory(self.signer.clone())),
        };

        self.call(request).await.map_err(Into::into)
    }
}

pub struct ClientPool {
    clients: Vec<Client>,
    nonce: Arc<AtomicU64>,
    block_hash: Arc<RwLock<CryptoHash>>,
    rpc_index: AtomicUsize,
}

impl ClientPool {
    pub fn new(config: &Config) -> Result<Self> {
        let Signer::InMemory(signer) =
            InMemorySigner::from_secret_key(config.account_id.clone(), config.private_key.parse()?)
        else {
            anyhow::bail!("Unsupported signer type");
        };

        let nonce = Arc::new(AtomicU64::new(0));
        let block_hash = Arc::new(RwLock::new(CryptoHash::default()));

        let clients: Vec<Client> = config
            .rpc_urls
            .iter()
            .map(|url| Client::new_with_shared(url, signer.clone(), block_hash.clone()))
            .collect();

        info!("Created client pool with {} RPC endpoints", clients.len());

        Ok(Self {
            clients,
            nonce,
            block_hash,
            rpc_index: AtomicUsize::new(0),
        })
    }

    async fn get_access_key_query(&self) -> Result<RpcQueryResponse> {
        self.clients[0].get_access_key_query().await
    }

    pub async fn reset_block_hash(&self) -> Result<()> {
        let access_key_query_response = self.get_access_key_query().await?;
        *self.block_hash.write().await = access_key_query_response.block_hash;
        Ok(())
    }

    pub async fn reset_nonce_and_block_hash(&self) -> Result<()> {
        let access_key_query_response = self.get_access_key_query().await?;

        let QueryResponseKind::AccessKey(access_key) = access_key_query_response.kind else {
            anyhow::bail!(
                "Unexpected query response kind: {:?}",
                access_key_query_response.kind
            );
        };

        self.nonce.store(access_key.nonce + 1, Ordering::SeqCst);
        *self.block_hash.write().await = access_key_query_response.block_hash;

        Ok(())
    }

    fn fetch_add_nonce(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::SeqCst)
    }

    fn get_next_client(&self) -> &Client {
        let index = self.rpc_index.fetch_add(1, Ordering::SeqCst);
        &self.clients[index % self.clients.len()]
    }

    pub async fn send_batch_transaction(
        &self,
        token: &AccountId,
        actions: Vec<Action>,
    ) -> Result<(FinalExecutionOutcomeView, String)> {
        let client = self.get_next_client();
        let rpc_url = client.rpc_url().to_string();
        let nonce = self.fetch_add_nonce();
        let outcome = client.send_batch_transaction(token, actions, nonce).await?;
        Ok((outcome, rpc_url))
    }

    pub async fn view_account(&self, account_id: &AccountId) -> Result<bool> {
        let client = self.get_next_client();
        client.view_account(account_id).await
    }

    pub async fn check_storage_deposit(
        &self,
        account_id: &AccountId,
        contract_id: &AccountId,
    ) -> Result<bool> {
        let client = self.get_next_client();
        client.check_storage_deposit(account_id, contract_id).await
    }
}
