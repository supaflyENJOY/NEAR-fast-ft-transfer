# NEAR FT Transfer API Service

A high-performance REST API service for batching and distributing NEAR Protocol fungible tokens (FT). Designed for token launches and airdrops requiring 100+ transfers per second.

## Features

- **Batching**: Automatically batches multiple `ft_transfer` actions into single transactions
- **Gas-based optimization**: Dynamically calculates batch size based on gas limits (up to 30 transfers per transaction)
- **Smart flushing**: Flushes batches when gas limit is reached OR every 500ms (whichever comes first)
- **RPC Load Balancing**: Round-robin distribution across multiple NEAR RPC endpoints
- **Request Validation**: Pre-flight checks for account existence and storage deposits with TTL-based caching
- **Auto Storage Deposit**: Configurable automatic storage deposit for receivers
- **Retry Logic**: Automatic retry of failed actions with configurable max attempts
- **Proper nonce management**: Single access key with atomic nonce tracking
- **Configurable**: All parameters configurable via environment variables
- **Production-ready**: Built with Rust, Axum, and official NEAR SDK

## Architecture

```
HTTP Request → Validation (Cache) → Batcher Queue → Batched Transaction → NEAR RPC (Round-Robin)
                    ↓                       ↓
          [Account & Storage Check]  [Flush: 500ms OR gas limit OR max size]
```

### Key Mechanisms

#### RPC Load Balancing

- Supports multiple RPC endpoints configured via `RPC_URLS` (comma-separated)
- Round-robin distribution using atomic counter
- All clients share the same nonce counter and block hash for transaction ordering
- Automatic failover to next RPC endpoint

#### Request Validation Pipeline

Each transfer request is validated before batching:

1. **Account Existence Check**: Verify receiver account exists on NEAR
2. **Storage Deposit Check**: Verify receiver has storage deposit for the FT contract

Both checks use TTL-based caching (default: 30 minutes) to minimize RPC calls and improve throughput.

#### Auto Storage Deposit

Configurable via `AUTO_STORAGE_DEPOSIT`:

- **true** (default): Automatically queue `storage_deposit` action before transfer if receiver lacks storage deposit. Both actions batch together in the same transaction. Seamless UX but costs relayer ~0.00125 NEAR per new receiver.
- **false**: Reject transfer with 400 error if receiver lacks storage deposit. Fail-fast approach requiring manual setup.

#### Retry Mechanism

When a batch transaction fails:

- Actions that didn't cause the failure are automatically retried (up to `MAX_RETRY_ATTEMPTS`)
- The action that caused the failure receives an error response immediately
- Broadcast failures retry all actions in the batch
- Prevents cascade failures in high-throughput scenarios

#### Nonce Management

- Single atomic nonce counter (`AtomicU64`) shared across all RPC clients
- Initialized from access key query on startup
- Atomically incremented for each transaction
- Block hash refreshed every 30 seconds in background task

## Quick Start

### Prerequisites

- Rust
- NEAR testnet account with FT tokens
- `hey` for benchmarking (optional)

### Installation

1. Clone the repository:

```bash
git clone https://github.com/supaflyENJOY/NEAR-fast-ft-transfer.git
cd NEAR-fast-ft-transfer
```

2. Configure environment variables:

```bash
cp .env.example .env
# Edit .env with your NEAR credentials
```

3. Build the project:

```bash
cargo build --release
```

4. Run the API server:

```bash
cargo run --release --bin server
```

The server will start on `http://0.0.0.0:3030` by default.

## API Documentation

### POST /transfer

Send an FT transfer request.

**Request:**

```json
{
  "receiver_id": "supafleet.testnet",
  "amount": "1000000000000000000000000",
  "memo": "Optional memo"
}
```

**Response:**

```json
{
  "tx_hash": "8Bc9...x7Kp",
  "batch_size": 1
}
```

**Status Codes:**

- `200 OK` - Transfer successfully queued and sent
- `500 Internal Server Error` - Failed to process transfer

**Example with curl:**

```bash
curl -X POST http://localhost:3030/transfer \
  -H "Content-Type: application/json" \
  -d '{
    "receiver_id": "supafleet.testnet",
    "amount": "123456",
    "memo": "Test transfer"
  }'
```

### GET /health

Health check endpoint.

**Response:**

```json
{
  "status": "ok"
}
```

## Configuration

All configuration is done via environment variables. See `.env.example` for details.

### Core Configuration

| Variable         | Description                                | Default  |
| ---------------- | ------------------------------------------ | -------- |
| `RPC_URLS`       | Comma-separated list of NEAR RPC endpoints | Required |
| `ACCOUNT_ID`     | Relayer account ID                         | Required |
| `PRIVATE_KEY`    | Account private key (ed25519 format)       | Required |
| `TOKEN_CONTRACT` | FT contract address                        | Required |
| `API_PORT`       | API server port                            | `3030`   |

### Batching Configuration

| Variable                 | Description                                 | Default                  |
| ------------------------ | ------------------------------------------- | ------------------------ |
| `FT_TRANSFER_GAS`        | Gas per ft_transfer call (in yoctoNEAR)     | `3000000000000`          |
| `STORAGE_DEPOSIT_GAS`    | Gas per storage_deposit call (in yoctoNEAR) | `5000000000000`          |
| `STORAGE_DEPOSIT_AMOUNT` | Storage deposit amount (in yoctoNEAR)       | `1250000000000000000000` |
| `MAX_TRANSACTION_GAS`    | Maximum gas per transaction (in yoctoNEAR)  | `300000000000000`        |
| `BATCH_TIMEOUT_MS`       | Batch flush interval in milliseconds        | `500`                    |

### Cache Configuration

| Variable            | Description                                  | Default         |
| ------------------- | -------------------------------------------- | --------------- |
| `CACHE_TTL_SECONDS` | Cache TTL for validation checks (in seconds) | `1800` (30 min) |

### Storage Deposit Behavior

| Variable               | Description                                     | Default |
| ---------------------- | ----------------------------------------------- | ------- |
| `AUTO_STORAGE_DEPOSIT` | Auto-deposit storage for receivers (true/false) | `true`  |

### Retry Configuration

| Variable             | Description                               | Default |
| -------------------- | ----------------------------------------- | ------- |
| `MAX_RETRY_ATTEMPTS` | Maximum retry attempts for failed actions | `3`     |

### Tuning Batch Size

The batch size is calculated as: `MAX_TRANSACTION_GAS / FT_TRANSFER_GAS`

With defaults: `300 TGas / 3 TGas = 100 transfers per batch`

If your FT contract uses less gas, you can lower `FT_TRANSFER_GAS` to increase batch size. Note that `storage_deposit` actions also consume gas, so actual batch sizes may vary when `AUTO_STORAGE_DEPOSIT` is enabled.

## Benchmarking

### Using hey

Install hey: https://github.com/rakyll/hey

```bash
# Run 10-minute test
hey -z 10m -c 1000 -q 1 -t 0 -m POST \
  -H "Content-Type: application/json" \
  -d '{"receiver_id":"supafleet2.testnet","amount":"1","memo":"load test transfer"}' \
  http://localhost:3030/transfer
```

### Performance Test Results

Tested on MB Pro 13" 2020 (M1)

```bash
hey -z 10m -c 1500 -q 5 -t 0 -m POST \
  -H "Content-Type: application/json" \
  -d '{"receiver_id":"supafleet2.testnet","amount":"1","memo":"load test transfer"}' \
  http://localhost:3030/transfer
```

```
Summary:
  Total:	603.6083 secs
  Slowest:	23.9270 secs
  Fastest:	1.6283 secs
  Average:	5.5487 secs
  Requests/sec:	269.7262 <<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<

  Total data:	9433896 bytes
  Size/request:	57 bytes

Response time histogram:
  1.628 [1]	|
  3.858 [43953]	|■■■■■■■■■■■■■■■■■■■■■■■■■■
  6.088 [68671]	|■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■
  8.318 [31415]	|■■■■■■■■■■■■■■■■■■
  10.548 [10092]	|■■■■■■
  12.778 [4945]	|■■■
  15.008 [1678]	|■
  17.237 [986]	|■
  19.467 [819]	|
  21.697 [149]	|
  23.927 [100]	|


Latency distribution:
  10% in 2.9293 secs
  25% in 3.7691 secs
  50% in 4.8579 secs
  75% in 6.6590 secs
  90% in 8.6556 secs
  95% in 10.7441 secs
  99% in 15.5713 secs

Details (average, fastest, slowest):
  DNS+dialup:	0.0003 secs, 1.6283 secs, 23.9270 secs
  DNS-lookup:	0.0001 secs, 0.0000 secs, 0.0325 secs
  req write:	0.0000 secs, 0.0000 secs, 0.0266 secs
  resp wait:	5.5483 secs, 1.6283 secs, 23.9268 secs
  resp read:	0.0000 secs, 0.0000 secs, 0.0152 secs

Status code distribution:
  [200]	162809 responses
```

Performance depends on:

- NEAR RPC endpoint reliability and latency
- Network conditions
- FT contract implementation and gas usage
- Batch size configuration
- Cache hit rate for validation checks
- Number of new receivers requiring storage deposits

## Deployment

### Docker Deployment

The project includes Docker support for easy deployment:

- **Dockerfile**: Multi-stage build that compiles the Rust binary in a builder container and creates a minimal runtime image
- **docker-compose.yml**: Orchestration file for running the service with all required environment variables

To deploy with Docker:

```bash
# Build the image
docker build -t fast-ft-transfer .

# Run with docker-compose
docker-compose up -d
```

Make sure to configure your environment variables in the docker-compose.yml file or use an .env file.

## How It Works

### Request Processing Flow

1. **Request arrives** via POST /transfer
2. **Validation pipeline**:
   - Check account exists (with cache)
   - Check storage deposit exists (with cache)
   - Auto-deposit storage if enabled and needed
3. **Queued in batcher** with gas tracking
4. **Flushed when**:
   - Gas limit would be exceeded by next action, OR
   - 500ms timeout elapsed
5. **Single transaction** created with multiple actions (`ft_transfer` and/or `storage_deposit`)
6. **Transaction sent** to next RPC endpoint (round-robin)
7. **Result handling**:
   - Success: All requests receive transaction hash
   - Failure: Failed action gets error, others are retried
   - Broadcast failure: All actions are retried
8. **Responses sent** to all waiting HTTP clients

### Batching Strategy

The batcher uses `tokio::select!` with biased ordering:

1. Timeout flush takes priority (prevents waiting too long)
2. New requests accumulate with gas tracking
3. Flush immediately if next action would exceed gas limit
4. Batches are flushed in background tasks for maximum throughput

### Nonce Management

- Single access key with `AtomicU64` counter shared across all RPC clients
- Initialized on startup from RPC
- Incremented atomically for each transaction
- Block hash refreshed every 30 seconds in background task
- Ensures transaction ordering regardless of which RPC endpoint is used

## Development

### Project Structure

```
src/
├── lib.rs          # Config and shared types
├── client.rs       # NEAR client pool with RPC load balancing
├── cache.rs        # TTL-based validation cache
├── batcher.rs      # Batching logic with retry mechanism
├── api.rs          # REST API handlers with validation
└── bin/
    └── server.rs   # API service entry point
```

### Running Tests

The project includes comprehensive integration tests using `near-workspaces`:

```bash
cargo test
```

The test suite:

- Runs a local NEAR sandbox environment for isolated testing
- Automatically downloads the FT contract WASM from near-examples/FT GitHub releases (saved to `res/fungible_token.wasm`)
- Verifies core functionality:
  - Single transfer execution
  - Batch accumulation and flushing
  - Timeout-based batching behavior
  - Concurrent transfer handling
  - Account validation and storage deposit checks
  - Auto storage deposit feature
  - Error handling for nonexistent accounts and missing storage deposits
