# phantom-filler
A high-performance, Rust-based intent-based order execution engine inspired by how UniswapX fillers operate. The system monitors mempools and order books across multiple EVM chains, discovers signed swap intents (Dutch auction orders), evaluates optimal fill strategies using on-chain and off-chain liquidity, and executes fills to capture profit while delivering price improvement to swappers.
Core Value Proposition:

Sub-millisecond decision-making for competitive order filling
Multi-chain support (Ethereum, Arbitrum, Base, Polygon, Optimism — extensible)
Pluggable strategy engine for fill optimization
MEV-aware execution with Flashbots/private relay integration
Real-time P&L tracking, risk management, and observability
