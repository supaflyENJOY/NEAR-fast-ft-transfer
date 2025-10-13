use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use dashmap::DashMap;
use log::{error, info, warn};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::{
    Config, HealthResponse, TransferRequest, TransferResponse,
    batcher::{BatchActionType, BatchRequest},
    cache::AccountValidationCache,
    client::ClientPool,
};

#[derive(Clone)]
pub struct AppState {
    pub batcher_tx: mpsc::Sender<BatchRequest>,
    pub client_pool: Arc<ClientPool>,
    pub cache: Arc<AccountValidationCache>,
    pub token_contract: near_primitives::types::AccountId,
    pub config: Config,
    pub account_validation_locks: Arc<DashMap<near_primitives::types::AccountId, Arc<Mutex<()>>>>,
    pub storage_deposit_inflight: Arc<DashMap<near_primitives::types::AccountId, Arc<Mutex<()>>>>,
}

pub fn create_router(
    batcher_tx: mpsc::Sender<BatchRequest>,
    client_pool: Arc<ClientPool>,
    cache: Arc<AccountValidationCache>,
    token_contract: near_primitives::types::AccountId,
    config: Config,
) -> Router {
    let state = AppState {
        batcher_tx,
        client_pool,
        cache,
        token_contract,
        config,
        account_validation_locks: Arc::new(DashMap::new()),
        storage_deposit_inflight: Arc::new(DashMap::new()),
    };

    Router::new()
        .route("/health", get(health_handler))
        .route("/transfer", post(transfer_handler))
        .with_state(state)
}

async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}

async fn transfer_handler(
    State(state): State<AppState>,
    Json(transfer): Json<TransferRequest>,
) -> impl IntoResponse {
    let validation_lock = state
        .account_validation_locks
        .entry(transfer.receiver_id.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();

    let _validation_guard = validation_lock.lock().await;

    let account_exists = match state.cache.get_account_exists(&transfer.receiver_id).await {
        Some(exists) => exists,
        None => match state.client_pool.view_account(&transfer.receiver_id).await {
            Ok(exists) => {
                state
                    .cache
                    .set_account_exists(&transfer.receiver_id, exists)
                    .await;
                exists
            }
            Err(e) => {
                error!("Failed to check account existence: {e:?}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to validate account: {e}")
                    })),
                )
                    .into_response();
            }
        },
    };

    let has_storage_deposit = match state
        .cache
        .get_storage_deposit(&transfer.receiver_id, &state.token_contract)
        .await
    {
        Some(has_deposit) => has_deposit,
        None => {
            match state
                .client_pool
                .check_storage_deposit(&transfer.receiver_id, &state.token_contract)
                .await
            {
                Ok(has_deposit) => {
                    state
                        .cache
                        .set_storage_deposit(
                            &transfer.receiver_id,
                            &state.token_contract,
                            has_deposit,
                        )
                        .await;
                    has_deposit
                }
                Err(e) => {
                    error!("Failed to check storage deposit: {e:?}");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": format!("Failed to validate storage deposit: {e}")
                        })),
                    )
                        .into_response();
                }
            }
        }
    };

    drop(_validation_guard);

    if !account_exists {
        warn!(
            "Transfer rejected: account {} does not exist",
            transfer.receiver_id
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Account {} does not exist", transfer.receiver_id)
            })),
        )
            .into_response();
    }

    if !has_storage_deposit {
        if state.config.auto_storage_deposit {
            let lock = state
                .storage_deposit_inflight
                .entry(transfer.receiver_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone();

            let _guard = lock.lock().await;

            let has_storage_after_wait = state
                .cache
                .get_storage_deposit(&transfer.receiver_id, &state.token_contract)
                .await
                .unwrap_or(false);

            if !has_storage_after_wait {
                info!("Queuing storage deposit for {}", transfer.receiver_id);

                let (storage_tx, storage_rx) = oneshot::channel();
                let storage_request = BatchRequest {
                    action: BatchActionType::StorageDeposit {
                        account_id: transfer.receiver_id.clone(),
                    },
                    response_tx: storage_tx,
                    retry_count: 0,
                };

                if let Err(e) = state.batcher_tx.send(storage_request).await {
                    error!("Failed to queue storage deposit: {e:?}");
                    state.storage_deposit_inflight.remove(&transfer.receiver_id);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": "Failed to queue storage deposit"
                        })),
                    )
                        .into_response();
                }

                match storage_rx.await {
                    Ok(Ok(tx_hash)) => {
                        info!(
                            "Storage deposit completed for {}: {}",
                            transfer.receiver_id, tx_hash
                        );
                        state
                            .cache
                            .set_storage_deposit(&transfer.receiver_id, &state.token_contract, true)
                            .await;
                    }
                    Ok(Err(e)) => {
                        error!("Storage deposit failed for {}: {e:?}", transfer.receiver_id);
                        state.storage_deposit_inflight.remove(&transfer.receiver_id);
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({
                                "error": format!("Storage deposit failed: {e}")
                            })),
                        )
                            .into_response();
                    }
                    Err(_) => {
                        error!("Storage deposit response channel closed");
                        state.storage_deposit_inflight.remove(&transfer.receiver_id);
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({
                                "error": "Failed to receive storage deposit response"
                            })),
                        )
                            .into_response();
                    }
                }
            } else {
                info!(
                    "Storage deposit for {} already completed by concurrent request",
                    transfer.receiver_id
                );
            }

            state.storage_deposit_inflight.remove(&transfer.receiver_id);
        } else {
            warn!(
                "Transfer rejected: account {} does not have storage deposit for token contract {}",
                transfer.receiver_id, state.token_contract
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "Account {} does not have storage deposit for token contract {}. Please register storage first.",
                        transfer.receiver_id, state.token_contract
                    )
                })),
            )
                .into_response();
        }
    }

    let (response_tx, response_rx) = oneshot::channel();
    let batch_request = BatchRequest {
        action: BatchActionType::Transfer {
            receiver_id: transfer.receiver_id,
            amount: transfer.amount,
            memo: transfer.memo,
        },
        response_tx,
        retry_count: 0,
    };

    if let Err(e) = state.batcher_tx.send(batch_request).await {
        error!("Failed to send to batcher: {e:?}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to queue transfer"
            })),
        )
            .into_response();
    }

    match response_rx.await {
        Ok(Ok(tx_hash)) => (
            StatusCode::OK,
            Json(TransferResponse {
                tx_hash: tx_hash.to_string(),
            }),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Transfer failed: {e}")
            })),
        )
            .into_response(),
        Err(_e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to receive transfer response"
            })),
        )
            .into_response(),
    }
}
