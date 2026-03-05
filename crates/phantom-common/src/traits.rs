//! Core traits defining the interfaces between Phantom Filler components.

use alloy::primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{ChainResult, DiscoveryResult, ExecutionResult, PricingResult, StrategyResult};
use crate::types::{ChainId, DutchAuctionOrder, OrderId, SignedOrder, Token};

/// Provides connectivity and interaction with an EVM chain.
#[async_trait]
pub trait ChainProvider: Send + Sync {
    /// Returns the chain ID this provider is connected to.
    fn chain_id(&self) -> ChainId;

    /// Returns the latest block number.
    async fn get_block_number(&self) -> ChainResult<u64>;

    /// Sends a raw signed transaction and returns the transaction hash.
    async fn send_raw_transaction(&self, tx: Bytes) -> ChainResult<[u8; 32]>;

    /// Returns the balance of a token for an address.
    async fn get_token_balance(&self, token: Address, owner: Address) -> ChainResult<U256>;
}

/// Source of swap intents/orders.
#[async_trait]
pub trait OrderSource: Send + Sync {
    /// Returns all currently active orders.
    async fn get_active_orders(&self) -> DiscoveryResult<Vec<SignedOrder>>;

    /// Returns a specific order by ID.
    async fn get_order(&self, id: &OrderId) -> DiscoveryResult<Option<SignedOrder>>;
}

/// Provides token price information.
#[async_trait]
pub trait PriceSource: Send + Sync {
    /// Returns a human-readable name for this price source.
    fn name(&self) -> &str;

    /// Returns the price of `base` in terms of `quote` (how many quote tokens per base token).
    /// The returned value is in the quote token's smallest unit per base token's smallest unit.
    async fn get_price(
        &self,
        base: &Token,
        quote: &Token,
        chain_id: ChainId,
    ) -> PricingResult<U256>;
}

/// Context provided to strategies during order evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationContext {
    /// Chain the order is on.
    pub chain_id: ChainId,
    /// Current block number.
    pub block_number: u64,
    /// Current block timestamp (unix seconds).
    pub block_timestamp: u64,
    /// Current base fee in wei.
    pub base_fee: u128,
    /// Suggested priority fee in wei.
    pub priority_fee: u128,
}

/// Strategy for evaluating whether to fill an order.
#[async_trait]
pub trait FillStrategy: Send + Sync {
    /// Returns a human-readable name for this strategy.
    fn name(&self) -> &str;

    /// Priority for ordering strategies (lower = higher priority).
    fn priority(&self) -> u32;

    /// Evaluates an order and returns an optional fill decision.
    /// Returns `Ok(Some(decision))` if the order should be filled,
    /// `Ok(None)` if the order should be skipped.
    async fn evaluate(
        &self,
        order: &DutchAuctionOrder,
        context: &EvaluationContext,
    ) -> StrategyResult<Option<FillDecision>>;
}

/// Decision to fill an order, including the expected profit.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FillDecision {
    /// The order to fill.
    pub order_id: OrderId,
    /// Strategy that produced this decision.
    pub strategy_name: String,
    /// Estimated profit in USD (or base denomination).
    pub estimated_profit: U256,
    /// Estimated gas cost for the fill transaction.
    pub estimated_gas_cost: U256,
    /// Confidence score from 0-100.
    pub confidence: u8,
}

/// Executes fill transactions on-chain.
#[async_trait]
pub trait Executor: Send + Sync {
    /// Executes a fill for the given order and returns the transaction hash.
    async fn execute_fill(
        &self,
        order: &SignedOrder,
        decision: &FillDecision,
    ) -> ExecutionResult<[u8; 32]>;

    /// Checks the status of a previously submitted transaction.
    async fn get_transaction_status(&self, tx_hash: [u8; 32]) -> ExecutionResult<TxStatus>;
}

/// Status of a submitted transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TxStatus {
    /// Transaction is pending in the mempool.
    Pending,
    /// Transaction has been included in a block.
    Confirmed,
    /// Transaction was reverted.
    Reverted,
    /// Transaction was dropped from the mempool.
    Dropped,
}

/// Provides balance information across chains and wallets.
#[async_trait]
pub trait BalanceProvider: Send + Sync {
    /// Returns the balance of a specific token on a chain for the filler's wallet.
    async fn get_balance(&self, token: &Token) -> ChainResult<U256>;

    /// Returns balances for multiple tokens.
    async fn get_balances(&self, tokens: &[Token]) -> ChainResult<Vec<(Token, U256)>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_decision_serde_roundtrip() {
        let decision = FillDecision {
            order_id: OrderId::new(alloy::primitives::FixedBytes::ZERO),
            strategy_name: "simple_arb".into(),
            estimated_profit: U256::from(1000u64),
            estimated_gas_cost: U256::from(100u64),
            confidence: 85,
        };
        let json = serde_json::to_string(&decision).expect("serialize");
        let deserialized: FillDecision = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.strategy_name, "simple_arb");
        assert_eq!(deserialized.confidence, 85);
    }

    #[test]
    fn tx_status_serde_roundtrip() {
        let statuses = [
            TxStatus::Pending,
            TxStatus::Confirmed,
            TxStatus::Reverted,
            TxStatus::Dropped,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).expect("serialize");
            let deserialized: TxStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*status, deserialized);
        }
    }
}
