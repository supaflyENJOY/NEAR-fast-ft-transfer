use anyhow::Result;
use log::{info, warn};
use near_primitives::{
    action::{Action, FunctionCallAction},
    errors::TxExecutionError,
    hash::CryptoHash,
    types::AccountId,
    views::FinalExecutionStatus,
};
use std::sync::Arc;
use tokio::{
    sync::{mpsc, oneshot},
    time::{Duration, interval},
};

use crate::{Config, client::ClientPool};

const FT_TRANSFER_DEPOSIT: u128 = 1;

#[derive(Debug, Clone)]
pub enum BatchActionType {
    Transfer {
        receiver_id: AccountId,
        amount: String,
        memo: Option<String>,
    },
    StorageDeposit { account_id: AccountId },
}

impl BatchActionType {
    pub fn gas_required(&self, config: &Config) -> u64 {
        match self {
            BatchActionType::Transfer { .. } => config.ft_transfer_gas,
            BatchActionType::StorageDeposit { .. } => config.storage_deposit_gas,
        }
    }

    pub fn to_action(&self, config: &Config) -> Action {
        match self {
            BatchActionType::Transfer {
                receiver_id,
                amount,
                memo,
            } => {
                let payload = serde_json::json!({
                    "receiver_id": receiver_id.to_string(),
                    "amount": amount,
                    "memo": memo.as_deref().unwrap_or(""),
                });

                let args = serde_json::to_vec(&payload).expect("Failed to serialize payload");

                Action::FunctionCall(Box::new(FunctionCallAction {
                    method_name: "ft_transfer".to_string(),
                    args,
                    gas: config.ft_transfer_gas,
                    deposit: FT_TRANSFER_DEPOSIT,
                }))
            }
            BatchActionType::StorageDeposit { account_id } => {
                let payload = serde_json::json!({
                    "account_id": account_id.to_string(),
                });

                let args = serde_json::to_vec(&payload).expect("Failed to serialize payload");

                Action::FunctionCall(Box::new(FunctionCallAction {
                    method_name: "storage_deposit".to_string(),
                    args,
                    gas: config.storage_deposit_gas,
                    deposit: config.storage_deposit_amount,
                }))
            }
        }
    }
}

pub struct BatchRequest {
    pub action: BatchActionType,
    pub response_tx: oneshot::Sender<Result<CryptoHash>>,
    pub retry_count: u8,
}

pub struct Batcher {
    client_pool: Arc<ClientPool>,
    token_contract: AccountId,
    config: Config,
    request_rx: mpsc::Receiver<BatchRequest>,
    request_tx: mpsc::Sender<BatchRequest>,
}

impl Batcher {
    pub fn new(
        client_pool: Arc<ClientPool>,
        token_contract: AccountId,
        config: Config,
    ) -> (Self, mpsc::Sender<BatchRequest>) {
        let (request_tx, request_rx) = mpsc::channel(1000);

        let batcher = Self {
            client_pool,
            token_contract,
            config,
            request_rx,
            request_tx: request_tx.clone(),
        };

        (batcher, request_tx)
    }

    pub async fn run(mut self) -> Result<()> {
        // Initialize nonce and block hash
        self.client_pool.reset_nonce_and_block_hash().await?;

        // Start block hash refresher
        let client_pool = self.client_pool.clone();
        tokio::spawn(async move {
            let mut refresh_interval = interval(Duration::from_secs(30));
            refresh_interval.tick().await;
            loop {
                refresh_interval.tick().await;
                if let Err(e) = client_pool.reset_block_hash().await {
                    warn!("Block hash refresh failed: {e:?}");
                }
            }
        });

        let mut batch: Vec<BatchRequest> = Vec::new();
        let mut accumulated_gas: u64 = 0;
        let max_batch_size = self.config.max_transfers_per_batch();
        let mut flush_interval = interval(Duration::from_millis(self.config.batch_timeout_ms));
        flush_interval.tick().await; // First tick completes immediately

        info!("Batcher started. Max batch size: {max_batch_size} transfers");

        loop {
            tokio::select! {
                biased;

                // Timeout: flush whatever we have
                _ = flush_interval.tick() => {
                    if !batch.is_empty() {
                        // Spawn flush as background task so we can start next batch immediately
                        let client_pool = self.client_pool.clone();
                        let token_contract = self.token_contract.clone();
                        let config = self.config.clone();
                        let request_tx = self.request_tx.clone();
                        let batch_to_flush = std::mem::take(&mut batch);

                        tokio::spawn(async move {
                            Self::flush_batch(client_pool, token_contract, config, request_tx, batch_to_flush).await;
                        });

                        accumulated_gas = 0;
                        flush_interval.reset();
                    }
                }

                // Receive new transfer request
                Some(request) = self.request_rx.recv() => {
                    let action_gas = request.action.gas_required(&self.config);
                    let next_gas = accumulated_gas + action_gas;

                    // Check if adding this action would exceed gas limit
                    if !batch.is_empty() && (
                        next_gas > self.config.max_transaction_gas ||
                        batch.len() >= max_batch_size
                    ) {
                        // Spawn flush as background task so we can start next batch immediately
                        let client_pool = self.client_pool.clone();
                        let token_contract = self.token_contract.clone();
                        let config = self.config.clone();
                        let request_tx = self.request_tx.clone();
                        let batch_to_flush = std::mem::take(&mut batch);

                        tokio::spawn(async move {
                            Self::flush_batch(client_pool, token_contract, config, request_tx, batch_to_flush).await;
                        });

                        accumulated_gas = 0;
                        flush_interval.reset();
                    }

                    batch.push(request);
                    accumulated_gas += action_gas;
                }
            }
        }
    }

    async fn flush_batch(
        client_pool: Arc<ClientPool>,
        token_contract: AccountId,
        config: Config,
        request_tx: mpsc::Sender<BatchRequest>,
        batch: Vec<BatchRequest>,
    ) {
        let actions: Vec<Action> = batch
            .iter()
            .map(|req| req.action.to_action(&config))
            .collect();

        let result = client_pool
            .send_batch_transaction(&token_contract, actions)
            .await;

        match result {
            Ok((outcome, rpc_url)) => {
                let tx_hash = outcome.transaction.hash;

                match &outcome.status {
                    FinalExecutionStatus::SuccessValue(_) => {
                        info!("Batch executed successfully on {rpc_url}: {tx_hash:?}");
                        for request in batch {
                            let _ = request.response_tx.send(Ok(tx_hash));
                        }
                    }
                    FinalExecutionStatus::Failure(tx_error) => {
                        warn!("Batch execution FAILED on {rpc_url}: {tx_hash:?}");
                        warn!("Failure reason: {tx_error:?}");

                        let failed_index_opt = match tx_error {
                            TxExecutionError::ActionError(action_err) => {
                                warn!(
                                    "ActionError at index {:?}: {:?}",
                                    action_err.index, action_err.kind
                                );
                                action_err.index
                            }
                            TxExecutionError::InvalidTxError(_) => {
                                warn!(
                                    "InvalidTxError - entire transaction invalid, no specific action index"
                                );
                                None
                            }
                        };

                        for (i, receipt_outcome) in outcome.receipts_outcome.iter().enumerate() {
                            warn!(
                                "Receipt {i} outcome: status={:?}, gas_burnt={}",
                                receipt_outcome.outcome.status, receipt_outcome.outcome.gas_burnt
                            );
                            if !receipt_outcome.outcome.logs.is_empty() {
                                warn!("Receipt {i} logs: {:?}", receipt_outcome.outcome.logs);
                            }
                        }

                        let error_msg = format!("Transaction execution failed: {tx_error:?}");

                        if let Some(failed_idx) = failed_index_opt {
                            info!(
                                "Action at index {failed_idx} failed, will retry other {}/{} actions",
                                batch.len() - 1,
                                batch.len()
                            );

                            for (idx, request) in batch.into_iter().enumerate() {
                                if idx == failed_idx as usize {
                                    warn!(
                                        "Sending failure response to action {idx} (caused the failure)"
                                    );
                                    let _ = request
                                        .response_tx
                                        .send(Err(anyhow::anyhow!("{}", error_msg)));
                                } else {
                                    if request.retry_count < config.max_retry_attempts {
                                        let retry_request = BatchRequest {
                                            action: request.action,
                                            response_tx: request.response_tx,
                                            retry_count: request.retry_count + 1,
                                        };

                                        if let Err(e) = request_tx.send(retry_request).await {
                                            warn!("Failed to re-queue retry request: {e:?}");
                                        }
                                    } else {
                                        warn!(
                                            "Action {idx} exceeded max retries ({}), failing permanently",
                                            config.max_retry_attempts
                                        );

                                        let max_retry_error = format!(
                                            "Max retry attempts ({}) exceeded. Original failure: {}",
                                            config.max_retry_attempts, error_msg
                                        );

                                        let _ = request
                                            .response_tx
                                            .send(Err(anyhow::anyhow!("{}", max_retry_error)));
                                    }
                                }
                            }
                        } else {
                            warn!(
                                "System-level failure (no action index), failing all {} actions",
                                batch.len()
                            );
                            for request in batch {
                                let _ = request
                                    .response_tx
                                    .send(Err(anyhow::anyhow!("{}", error_msg)));
                            }
                        }
                    }
                    FinalExecutionStatus::NotStarted | FinalExecutionStatus::Started => {
                        warn!(
                            "Batch execution incomplete on {rpc_url}: {tx_hash:?}, status: {:?}",
                            outcome.status
                        );
                        let error_msg = "Transaction execution did not complete";
                        for request in batch {
                            let _ = request
                                .response_tx
                                .send(Err(anyhow::anyhow!("{}", error_msg)));
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Batch broadcast failed: {e:?}");
                let error_msg = format!("Transaction broadcast failed: {e:?}");

                info!(
                    "Broadcast failed for batch of {} actions, will retry eligible requests",
                    batch.len()
                );

                for (idx, request) in batch.into_iter().enumerate() {
                    if request.retry_count < config.max_retry_attempts {
                        let retry_request = BatchRequest {
                            action: request.action,
                            response_tx: request.response_tx,
                            retry_count: request.retry_count + 1,
                        };

                        if let Err(e) = request_tx.send(retry_request).await {
                            warn!(
                                "Failed to re-queue retry request after broadcast failure: {e:?}"
                            );
                        }
                    } else {
                        warn!(
                            "Action {idx} exceeded max retries ({}) after broadcast failure, failing permanently",
                            config.max_retry_attempts
                        );

                        let max_retry_error = format!(
                            "Max retry attempts ({}) exceeded. Original broadcast failure: {}",
                            config.max_retry_attempts, error_msg
                        );

                        let _ = request
                            .response_tx
                            .send(Err(anyhow::anyhow!("{}", max_retry_error)));
                    }
                }
            }
        }
    }
}
