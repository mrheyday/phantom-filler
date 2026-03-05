//! Simple arbitrage strategy comparing order prices against market prices.

use std::sync::Arc;

use alloy::primitives::{Address, U256};
use async_trait::async_trait;
use chrono::Utc;
use phantom_common::error::{StrategyError, StrategyResult};
use phantom_common::traits::{EvaluationContext, FillDecision, FillStrategy};
use phantom_common::types::{ChainId, DutchAuctionOrder};
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

/// Provides market price quotes for token pairs.
///
/// Implementations wrap actual DEX quoters or price aggregators to return
/// the cost of acquiring a given output amount.
#[async_trait]
pub trait MarketQuoter: Send + Sync {
    /// Returns the cost (in input token smallest units) to acquire
    /// `output_amount` of `output_token` by spending `input_token`.
    async fn quote_cost(
        &self,
        input_token: Address,
        output_token: Address,
        output_amount: U256,
        chain_id: ChainId,
    ) -> StrategyResult<U256>;
}

/// Configuration for the simple arbitrage strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimpleArbConfig {
    /// Minimum net profit in wei after gas costs.
    pub min_profit_wei: U256,
    /// Minimum profit as basis points of input amount (e.g., 10 = 0.1%).
    pub min_profit_bps: u64,
    /// Maximum gas cost in wei the strategy is willing to pay.
    pub max_gas_cost_wei: U256,
    /// Default gas limit for a fill transaction.
    pub default_gas_limit: u64,
    /// Strategy priority (lower = higher priority).
    pub priority: u32,
}

impl Default for SimpleArbConfig {
    fn default() -> Self {
        Self {
            min_profit_wei: U256::from(1_000_000_000_000_000u64), // 0.001 ETH
            min_profit_bps: 10,                                   // 0.1%
            max_gas_cost_wei: U256::from(50_000_000_000_000_000u64), // 0.05 ETH
            default_gas_limit: 200_000,
            priority: 10,
        }
    }
}

/// Simple arbitrage strategy that compares Dutch auction order prices
/// against current market prices to identify profitable fill opportunities.
///
/// The strategy:
/// 1. Computes the current required output (decay-adjusted)
/// 2. Queries market cost to acquire that output
/// 3. Calculates net profit after gas costs
/// 4. Applies minimum profit thresholds (absolute and basis points)
pub struct SimpleArbStrategy {
    quoter: Arc<dyn MarketQuoter>,
    config: SimpleArbConfig,
}

impl SimpleArbStrategy {
    /// Creates a new strategy with the given market quoter and configuration.
    pub fn new(quoter: Arc<dyn MarketQuoter>, config: SimpleArbConfig) -> Self {
        Self { quoter, config }
    }

    /// Creates a strategy with default configuration.
    pub fn with_defaults(quoter: Arc<dyn MarketQuoter>) -> Self {
        Self::new(quoter, SimpleArbConfig::default())
    }

    /// Returns a reference to the strategy configuration.
    pub fn config(&self) -> &SimpleArbConfig {
        &self.config
    }
}

#[async_trait]
impl FillStrategy for SimpleArbStrategy {
    fn name(&self) -> &str {
        "simple_arb"
    }

    fn priority(&self) -> u32 {
        self.config.priority
    }

    async fn evaluate(
        &self,
        order: &DutchAuctionOrder,
        context: &EvaluationContext,
    ) -> StrategyResult<Option<FillDecision>> {
        let now = Utc::now();

        // Skip expired orders.
        if order.is_expired(now) {
            trace!(order_id = %order.id, "order expired, skipping");
            return Ok(None);
        }

        // Use the first output for simple arb (single-output orders).
        if order.outputs.is_empty() {
            trace!(order_id = %order.id, "no outputs, skipping");
            return Ok(None);
        }

        let required_output = order.current_output_amount(0, now).ok_or_else(|| {
            StrategyError::SimulationFailed("failed to compute current output amount".into())
        })?;

        let output = &order.outputs[0];

        // Query market cost to acquire the required output amount.
        let market_cost = self
            .quoter
            .quote_cost(
                order.input.token,
                output.token,
                required_output,
                order.chain_id,
            )
            .await?;

        // No profit if market cost exceeds what the swapper is providing.
        if market_cost >= order.input.amount {
            debug!(
                order_id = %order.id,
                input_amount = %order.input.amount,
                market_cost = %market_cost,
                "no arbitrage opportunity"
            );
            return Ok(None);
        }

        let gross_profit = order.input.amount - market_cost;

        // Estimate gas cost: (base_fee + priority_fee) * gas_limit.
        let gas_price = U256::from(context.base_fee.saturating_add(context.priority_fee));
        let gas_cost_wei = gas_price * U256::from(self.config.default_gas_limit);

        // Skip if gas cost exceeds our maximum.
        if gas_cost_wei > self.config.max_gas_cost_wei {
            debug!(
                order_id = %order.id,
                gas_cost = %gas_cost_wei,
                max_gas = %self.config.max_gas_cost_wei,
                "gas cost exceeds maximum"
            );
            return Ok(None);
        }

        // Net profit after gas.
        let net_profit = gross_profit.saturating_sub(gas_cost_wei);

        // Check absolute minimum profit.
        if net_profit < self.config.min_profit_wei {
            debug!(
                order_id = %order.id,
                net_profit = %net_profit,
                min_profit = %self.config.min_profit_wei,
                "profit below minimum threshold"
            );
            return Ok(None);
        }

        // Check basis points threshold (net_profit / input_amount * 10000).
        if !order.input.amount.is_zero() {
            let profit_bps = net_profit * U256::from(10_000u64) / order.input.amount;
            if profit_bps < U256::from(self.config.min_profit_bps) {
                debug!(
                    order_id = %order.id,
                    profit_bps = %profit_bps,
                    min_bps = self.config.min_profit_bps,
                    "profit below basis points threshold"
                );
                return Ok(None);
            }
        }

        // Compute confidence based on profit-to-gas ratio.
        let confidence = compute_confidence(net_profit, gas_cost_wei);

        debug!(
            order_id = %order.id,
            net_profit = %net_profit,
            gas_cost = %gas_cost_wei,
            confidence,
            "profitable fill opportunity found"
        );

        Ok(Some(FillDecision {
            order_id: order.id,
            strategy_name: "simple_arb".into(),
            estimated_profit: net_profit,
            estimated_gas_cost: gas_cost_wei,
            confidence,
        }))
    }
}

/// Computes a confidence score (0-95) based on the profit-to-gas ratio.
///
/// Higher ratios yield higher confidence. The score is capped at 95 to
/// reflect inherent uncertainty in on-chain execution.
fn compute_confidence(net_profit: U256, gas_cost: U256) -> u8 {
    if gas_cost.is_zero() {
        return 95;
    }

    // ratio = net_profit / gas_cost (integer division)
    let ratio = net_profit / gas_cost;

    // Map ratio to confidence:
    // ratio 1 -> 50, ratio 2 -> 60, ratio 5 -> 75, ratio 10+ -> 90, ratio 20+ -> 95
    if ratio >= U256::from(20u64) {
        95u8
    } else if ratio >= U256::from(10u64) {
        90
    } else if ratio >= U256::from(5u64) {
        75
    } else if ratio >= U256::from(2u64) {
        60
    } else if ratio >= U256::from(1u64) {
        50
    } else {
        40
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, fixed_bytes};
    use chrono::Duration;
    use phantom_common::types::{OrderId, OrderInput, OrderOutput};

    /// Mock quoter that returns a fixed cost.
    struct FixedQuoter {
        cost: U256,
    }

    impl FixedQuoter {
        fn new(cost: u64) -> Self {
            Self {
                cost: U256::from(cost),
            }
        }
    }

    #[async_trait]
    impl MarketQuoter for FixedQuoter {
        async fn quote_cost(
            &self,
            _input_token: Address,
            _output_token: Address,
            _output_amount: U256,
            _chain_id: ChainId,
        ) -> StrategyResult<U256> {
            Ok(self.cost)
        }
    }

    /// Mock quoter that always errors.
    struct ErrorQuoter;

    #[async_trait]
    impl MarketQuoter for ErrorQuoter {
        async fn quote_cost(
            &self,
            _input_token: Address,
            _output_token: Address,
            _output_amount: U256,
            _chain_id: ChainId,
        ) -> StrategyResult<U256> {
            Err(StrategyError::SimulationFailed("quote unavailable".into()))
        }
    }

    fn sample_order(input_amount: u64) -> DutchAuctionOrder {
        let now = Utc::now();
        DutchAuctionOrder {
            id: OrderId::new(fixed_bytes!(
                "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
            )),
            chain_id: ChainId::Ethereum,
            reactor: address!("0x6000da47483062A0D734Ba3dc7576Ce6A0B645C4"),
            signer: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            nonce: U256::from(1u64),
            decay_start_time: now - Duration::minutes(1),
            decay_end_time: now + Duration::minutes(10),
            deadline: now + Duration::minutes(30),
            input: OrderInput {
                token: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                amount: U256::from(input_amount),
            },
            outputs: vec![OrderOutput {
                token: address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                start_amount: U256::from(500_000_000_000_000_000u64),
                end_amount: U256::from(450_000_000_000_000_000u64),
                recipient: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            }],
        }
    }

    fn expired_order() -> DutchAuctionOrder {
        let past = Utc::now() - Duration::hours(1);
        DutchAuctionOrder {
            id: OrderId::new(fixed_bytes!(
                "1111111111111111111111111111111111111111111111111111111111111111"
            )),
            chain_id: ChainId::Ethereum,
            reactor: address!("0x6000da47483062A0D734Ba3dc7576Ce6A0B645C4"),
            signer: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            nonce: U256::from(1u64),
            decay_start_time: past - Duration::minutes(30),
            decay_end_time: past - Duration::minutes(10),
            deadline: past,
            input: OrderInput {
                token: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                amount: U256::from(1_000_000_000u64),
            },
            outputs: vec![OrderOutput {
                token: address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                start_amount: U256::from(500_000_000_000_000_000u64),
                end_amount: U256::from(450_000_000_000_000_000u64),
                recipient: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            }],
        }
    }

    fn sample_context() -> EvaluationContext {
        EvaluationContext {
            chain_id: ChainId::Ethereum,
            block_number: 19_000_000,
            block_timestamp: 1_700_000_000,
            base_fee: 30_000_000_000,    // 30 gwei
            priority_fee: 2_000_000_000, // 2 gwei
        }
    }

    fn low_gas_config() -> SimpleArbConfig {
        SimpleArbConfig {
            min_profit_wei: U256::from(1_000u64),
            min_profit_bps: 1,
            max_gas_cost_wei: U256::from(100_000_000_000_000_000u64), // 0.1 ETH
            default_gas_limit: 200_000,
            priority: 10,
        }
    }

    #[tokio::test]
    async fn profitable_order_fills() {
        // Input: 1_000_000_000 (1B units), market cost: 500_000_000 (500M units)
        // Gross profit: 500_000_000
        // Gas cost: (30 + 2) gwei * 200k = 6_400_000_000_000 (6.4T wei)
        // For this test, use a large input so profit > gas
        let order = sample_order(10_000_000_000_000_000_000); // 10 ETH worth
        let quoter = Arc::new(FixedQuoter::new(5_000_000_000_000_000_000)); // 5 ETH cost

        let strategy = SimpleArbStrategy::new(quoter, low_gas_config());
        let result = strategy
            .evaluate(&order, &sample_context())
            .await
            .expect("should succeed");

        let decision = result.expect("should recommend fill");
        assert_eq!(decision.strategy_name, "simple_arb");
        assert!(decision.estimated_profit > U256::ZERO);
        assert!(decision.confidence >= 50);
    }

    #[tokio::test]
    async fn unprofitable_order_skipped() {
        // Market cost > input amount = no profit
        let order = sample_order(1_000_000_000);
        let quoter = Arc::new(FixedQuoter::new(2_000_000_000)); // costs more than input

        let strategy = SimpleArbStrategy::new(quoter, low_gas_config());
        let result = strategy
            .evaluate(&order, &sample_context())
            .await
            .expect("should succeed");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn expired_order_skipped() {
        let order = expired_order();
        let quoter = Arc::new(FixedQuoter::new(100));

        let strategy = SimpleArbStrategy::with_defaults(quoter);
        let result = strategy
            .evaluate(&order, &sample_context())
            .await
            .expect("should succeed");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn profit_below_min_wei_skipped() {
        // Small profit that's below the minimum threshold
        let order = sample_order(1_000_000);
        let quoter = Arc::new(FixedQuoter::new(999_000)); // 1000 profit, tiny

        let config = SimpleArbConfig {
            min_profit_wei: U256::from(1_000_000_000_000_000u64), // 0.001 ETH
            min_profit_bps: 0,
            max_gas_cost_wei: U256::from(100_000_000_000_000_000u64),
            default_gas_limit: 200_000,
            priority: 10,
        };
        let strategy = SimpleArbStrategy::new(quoter, config);
        let result = strategy
            .evaluate(&order, &sample_context())
            .await
            .expect("should succeed");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn gas_cost_exceeds_max_skipped() {
        let order = sample_order(10_000_000_000_000_000_000);
        let quoter = Arc::new(FixedQuoter::new(5_000_000_000_000_000_000));

        let config = SimpleArbConfig {
            min_profit_wei: U256::from(1u64),
            min_profit_bps: 0,
            max_gas_cost_wei: U256::from(1u64), // absurdly low max gas
            default_gas_limit: 200_000,
            priority: 10,
        };
        let strategy = SimpleArbStrategy::new(quoter, config);
        let result = strategy
            .evaluate(&order, &sample_context())
            .await
            .expect("should succeed");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn quoter_error_propagated() {
        let order = sample_order(1_000_000_000);
        let quoter = Arc::new(ErrorQuoter);

        let strategy = SimpleArbStrategy::with_defaults(quoter);
        let result = strategy.evaluate(&order, &sample_context()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_outputs_skipped() {
        let now = Utc::now();
        let order = DutchAuctionOrder {
            id: OrderId::new(fixed_bytes!(
                "2222222222222222222222222222222222222222222222222222222222222222"
            )),
            chain_id: ChainId::Ethereum,
            reactor: address!("0x6000da47483062A0D734Ba3dc7576Ce6A0B645C4"),
            signer: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            nonce: U256::from(1u64),
            decay_start_time: now,
            decay_end_time: now + Duration::minutes(10),
            deadline: now + Duration::minutes(30),
            input: OrderInput {
                token: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                amount: U256::from(1_000_000_000u64),
            },
            outputs: vec![], // no outputs
        };

        let quoter = Arc::new(FixedQuoter::new(100));
        let strategy = SimpleArbStrategy::with_defaults(quoter);
        let result = strategy
            .evaluate(&order, &sample_context())
            .await
            .expect("should succeed");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn bps_threshold_filter() {
        // Input: 10 ETH, cost: 9.999 ETH => profit 0.001 ETH = 1 bps
        let order = sample_order(10_000_000_000_000_000_000);
        let quoter = Arc::new(FixedQuoter::new(9_999_000_000_000_000_000));

        let config = SimpleArbConfig {
            min_profit_wei: U256::from(1u64),
            min_profit_bps: 50, // require 0.5%
            max_gas_cost_wei: U256::from(100_000_000_000_000_000u64),
            default_gas_limit: 200_000,
            priority: 10,
        };
        let strategy = SimpleArbStrategy::new(quoter, config);
        let result = strategy
            .evaluate(&order, &sample_context())
            .await
            .expect("should succeed");

        assert!(result.is_none(), "should be filtered by bps threshold");
    }

    #[test]
    fn confidence_zero_gas() {
        assert_eq!(compute_confidence(U256::from(1000u64), U256::ZERO), 95);
    }

    #[test]
    fn confidence_high_ratio() {
        // profit/gas = 20 => 95
        let confidence = compute_confidence(U256::from(20_000u64), U256::from(1_000u64));
        assert_eq!(confidence, 95);
    }

    #[test]
    fn confidence_medium_ratio() {
        // profit/gas = 5 => 75
        let confidence = compute_confidence(U256::from(5_000u64), U256::from(1_000u64));
        assert_eq!(confidence, 75);
    }

    #[test]
    fn confidence_low_ratio() {
        // profit/gas = 1 => 50
        let confidence = compute_confidence(U256::from(1_000u64), U256::from(1_000u64));
        assert_eq!(confidence, 50);
    }

    #[test]
    fn confidence_very_low_ratio() {
        // profit/gas < 1 => 40
        let confidence = compute_confidence(U256::from(500u64), U256::from(1_000u64));
        assert_eq!(confidence, 40);
    }

    #[test]
    fn default_config() {
        let config = SimpleArbConfig::default();
        assert_eq!(config.min_profit_bps, 10);
        assert_eq!(config.default_gas_limit, 200_000);
        assert_eq!(config.priority, 10);
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = SimpleArbConfig {
            min_profit_wei: U256::from(5000u64),
            min_profit_bps: 25,
            max_gas_cost_wei: U256::from(10_000u64),
            default_gas_limit: 300_000,
            priority: 5,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: SimpleArbConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.min_profit_bps, 25);
        assert_eq!(deserialized.default_gas_limit, 300_000);
    }

    #[test]
    fn strategy_name_and_priority() {
        let quoter = Arc::new(FixedQuoter::new(0));
        let strategy = SimpleArbStrategy::with_defaults(quoter);
        assert_eq!(strategy.name(), "simple_arb");
        assert_eq!(strategy.priority(), 10);
    }

    #[test]
    fn strategy_custom_priority() {
        let quoter = Arc::new(FixedQuoter::new(0));
        let config = SimpleArbConfig {
            priority: 5,
            ..SimpleArbConfig::default()
        };
        let strategy = SimpleArbStrategy::new(quoter, config);
        assert_eq!(strategy.priority(), 5);
    }
}
