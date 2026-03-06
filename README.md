# Phantom Filler

A high-performance, Rust-based intent execution engine for DeFi. Monitors mempools and order books across multiple EVM chains, discovers signed swap intents (Dutch auction orders), evaluates optimal fill strategies, and executes fills to capture profit while delivering price improvement to swappers.

## Key Features

- **Sub-millisecond decision-making** for competitive order filling
- **Multi-chain support** — Ethereum, Arbitrum, Base, Polygon, Optimism (extensible)
- **Pluggable strategy engine** with simulation and scoring
- **MEV-aware execution** with Flashbots/private relay integration
- **Real-time P&L tracking**, risk management, and observability
- **Dutch auction order filling** with decay curve optimization

## Architecture

```
                    ┌─────────────────────────────────────────┐
                    │            External Systems              │
                    │  RPCs · Mempools · DEXs · CEXs · Relays │
                    └──────────────┬──────────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────────┐
                    │         Chain Connector Layer            │
                    │   Multi-chain WS/RPC · Block Streaming  │
                    └──────────────┬──────────────────────────┘
                                   │
              ┌────────────────────▼────────────────────┐
              │         Intent Discovery Service         │
              │  Reactor Monitoring · Order Decoding     │
              │  In-Memory Order Book · Order Lifecycle  │
              └────────┬───────────────────┬────────────┘
                       │                   │
          ┌────────────▼───────┐ ┌─────────▼──────────┐
          │   Pricing Engine   │ │  Strategy Engine    │
          │  On-chain/Off-chain│ │  Scoring · Routing  │
          │  Gas · Slippage    │ │  Simulation · Rank  │
          └────────┬───────────┘ └─────────┬──────────┘
                   │                       │
              ┌────▼───────────────────────▼────────────┐
              │          Execution Engine                │
              │  Tx Building · Nonce Mgmt · Flashbots   │
              │  Bundle Building · Retry Logic           │
              └────────────────┬────────────────────────┘
                               │
         ┌─────────────────────▼─────────────────────┐
         │        Settlement & Reconciliation         │
         │  Confirmation · Revert Handling · Logging  │
         └─────────────────────┬─────────────────────┘
                               │
    ┌──────────────┬───────────▼──────────┬──────────────┐
    │  Inventory & │    Observability     │  API &       │
    │  Risk Mgmt   │  Metrics · Logging   │  Dashboard   │
    │  P&L · Limits│  Alerts · Health     │  REST · WS   │
    └──────────────┴──────────────────────┴──────────────┘
```

## Workspace Structure

| Crate | Purpose |
|-------|---------|
| `phantom-common` | Shared types, traits, errors, and configuration |
| `phantom-chain` | Multi-chain WS/RPC connections, block/event streaming, mempool monitoring |
| `phantom-discovery` | Reactor monitoring, order decoding, in-memory order book |
| `phantom-pricing` | On-chain + off-chain price aggregation, gas cost estimation |
| `phantom-strategy` | Fill evaluation, pluggable strategies, simulation and scoring |
| `phantom-execution` | Transaction building, nonce management, Flashbots relay integration |
| `phantom-inventory` | Balance tracking, position limits, risk management, P&L |
| `phantom-settlement` | Confirmation tracking, revert handling, accounting |
| `phantom-metrics` | Prometheus metrics, structured logging, health checks, circuit breakers |
| `phantom-api` | REST API server, WebSocket feeds, system status endpoints |
| `phantom-filler` | Main binary — orchestrates all components |

### Smart Contracts (`contracts/`)

Foundry-based Solidity contracts:

| Contract | Purpose |
|----------|---------|
| `PhantomFiller.sol` | On-chain fill execution and reactor interaction |
| `PhantomSettlement.sol` | Settlement verification and fund routing |
| `DeployAll.s.sol` | Full deployment script |

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Language | Rust (2021 edition) |
| Async Runtime | Tokio |
| EVM Interaction | Alloy |
| Database | PostgreSQL 16 (sqlx) |
| Cache | Redis 7 |
| Metrics | Prometheus (metrics crate) |
| Logging | tracing ecosystem |
| Smart Contracts | Solidity 0.8.x (Foundry) |
| Testing | cargo test, proptest, criterion, Foundry |
| Containers | Docker Compose |
| Config | TOML + env var overrides |

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) (stable, latest)
- [Docker](https://docs.docker.com/get-docker/) and Docker Compose
- [Foundry](https://book.getfoundry.sh/getting-started/installation) (for smart contracts)
- RPC endpoints for target chains (e.g., [Alchemy](https://www.alchemy.com/))

### 1. Clone and Build

```bash
git clone https://github.com/0xfandom/phantom-filler.git
cd phantom-filler
cargo build
```

### 2. Start Infrastructure

```bash
docker-compose up -d
```

This starts PostgreSQL 16 and Redis 7 with persistent volumes and health checks.

### 3. Configure

```bash
cp config.example.toml config.toml
cp .env.example .env
```

Edit `config.toml` with your RPC endpoints and preferences. Set your private key in `.env`:

```bash
PHANTOM_PRIVATE_KEY=0x...
```

Environment variables with the `PHANTOM_` prefix override TOML values.

### 4. Run Database Migrations

```bash
sqlx migrate run
```

### 5. Run

```bash
cargo run --release
```

## Configuration

Configuration is loaded from `config.toml` with environment variable overrides:

| Section | Key Settings |
|---------|-------------|
| `[chains.*]` | `chain_id`, `rpc_url`, `ws_url`, `max_concurrent_requests`, `mempool_enabled` |
| `[database]` | `url`, `max_connections`, `connect_timeout_secs` |
| `[redis]` | `url`, `pool_size` |
| `[strategy]` | `min_profit_bps`, `max_gas_price_gwei`, `enabled_strategies` |
| `[execution]` | `flashbots_enabled`, `max_retries`, `retry_delay_ms` |
| `[api]` | `host`, `port`, `websocket_enabled` |
| `[metrics]` | `enabled`, `port`, `log_level`, `json_logs` |

See [`config.example.toml`](config.example.toml) for a complete reference.

## API Endpoints

The REST API runs on the configured port (default: 8080).

**Health (top-level)**

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health/live` | Liveness probe |
| `GET` | `/health/ready` | Readiness probe with component status |
| `GET` | `/health` | Detailed system health report |

**API v1 (`/api/v1`)**

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/v1/status` | System status overview |
| `GET` | `/api/v1/risk` | Current risk parameters and exposure |
| `GET` | `/api/v1/pnl` | Realized P&L summary |
| `GET` | `/api/v1/pnl/daily` | Daily P&L breakdown |
| `GET` | `/api/v1/pnl/tokens` | Per-token P&L breakdown |
| `GET` | `/api/v1/fills` | Recent fill history |
| `GET` | `/api/v1/fills/{id}` | Get specific fill by ID |
| `WS` | `/ws` | WebSocket event feed |

**Prometheus metrics** are served on port 9090 (`/metrics`).

## Testing

### Unit and Integration Tests

```bash
# Run all tests
cargo test --workspace

# Run a specific crate's tests
cargo test -p phantom-discovery

# Run integration tests only
cargo test -p phantom-filler --tests
```

### Property-Based Tests

36 property-based tests using [proptest](https://github.com/proptest-rs/proptest) verify invariants across core types, order book operations, and inventory management:

```bash
cargo test -p phantom-filler --test proptest_types
cargo test -p phantom-filler --test proptest_orderbook
cargo test -p phantom-filler --test proptest_inventory
```

### Benchmarks

Performance benchmarks using [criterion](https://github.com/bheisler/criterion.rs):

```bash
# Run all benchmarks
cargo bench -p phantom-filler

# Run specific benchmark
cargo bench -p phantom-filler --bench orderbook_bench
cargo bench -p phantom-filler --bench risk_bench
cargo bench -p phantom-filler --bench pnl_bench
cargo bench -p phantom-filler --bench execution_bench
```

### Smart Contract Tests

```bash
cd contracts
forge test
```

### Code Quality

```bash
cargo fmt --check        # Formatting
cargo clippy -- -D warnings  # Linting
```

## How It Works

### Order Lifecycle

1. **Discovery** — Reactor contracts emit events when users sign Dutch auction orders. The discovery service decodes these and adds them to the in-memory order book.

2. **Pricing** — The pricing engine fetches real-time prices from on-chain DEXs and off-chain sources, factoring in gas costs and slippage.

3. **Strategy** — Registered strategies evaluate each order against current market conditions. The engine scores opportunities and selects the most profitable fills.

4. **Execution** — The execution engine builds fill transactions, manages nonces, and optionally routes through Flashbots to avoid frontrunning.

5. **Settlement** — After submission, the settlement monitor tracks confirmations, handles reverts, and records outcomes.

6. **Accounting** — Every fill is recorded with P&L, gas costs, and chain metadata. Risk limits are enforced in real-time.

### Dutch Auction Decay

Orders use a Dutch auction mechanism where the output amount decays linearly from `start_amount` to `end_amount` over the decay window. The filler captures the spread between the decayed amount and the actual execution cost.

```
Output Amount
     ^
     |  start_amount ─────╮
     |                     ╲
     |                      ╲  (linear decay)
     |                       ╲
     |           end_amount ──╰─────
     └──────────────────────────────> Time
          decay_start    decay_end
```

## Project Structure

```
phantom-filler/
├── Cargo.toml                 # Workspace root
├── config.example.toml        # Configuration reference
├── docker-compose.yml         # PostgreSQL + Redis
├── .env.example               # Environment variable template
├── migrations/                # SQL database migrations
├── contracts/                 # Foundry smart contracts
│   ├── src/                   # Solidity sources
│   ├── test/                  # Forge tests
│   └── script/                # Deployment scripts
└── crates/
    ├── phantom-common/        # Shared types, traits, config
    ├── phantom-chain/         # Chain connectors
    ├── phantom-discovery/     # Order discovery + order book
    ├── phantom-pricing/       # Price aggregation
    ├── phantom-strategy/      # Fill strategies
    ├── phantom-execution/     # Tx building + MEV
    ├── phantom-inventory/     # Risk + P&L
    ├── phantom-settlement/    # Confirmations
    ├── phantom-metrics/       # Observability
    ├── phantom-api/           # REST + WebSocket API
    └── phantom-filler/        # Main binary + integration tests
```

## License

MIT
