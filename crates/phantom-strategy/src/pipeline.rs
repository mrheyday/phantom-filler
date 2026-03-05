//! Strategy pipeline with priority-based execution.

use std::sync::Arc;

use alloy::primitives::U256;
use phantom_common::error::StrategyResult;
use phantom_common::traits::{EvaluationContext, FillDecision, FillStrategy};
use phantom_common::types::DutchAuctionOrder;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::registry::StrategyRegistry;

/// Configuration for the strategy pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Minimum confidence score (0-100) to consider a fill decision.
    pub min_confidence: u8,
    /// Minimum estimated profit in wei to consider a fill decision.
    pub min_profit_wei: U256,
    /// Maximum time in milliseconds to wait for a single strategy evaluation.
    pub evaluation_timeout_ms: u64,
    /// Maximum number of strategies to evaluate concurrently.
    pub max_concurrent_evaluations: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            min_confidence: 50,
            min_profit_wei: U256::ZERO,
            evaluation_timeout_ms: 5_000,
            max_concurrent_evaluations: 10,
        }
    }
}

/// Orchestrates strategy evaluation for incoming orders.
///
/// The pipeline runs registered strategies in priority order against an order,
/// collecting fill decisions and selecting the best one based on a composite
/// score of confidence and estimated profit.
pub struct StrategyPipeline {
    registry: Arc<StrategyRegistry>,
    config: PipelineConfig,
}

impl StrategyPipeline {
    /// Creates a new pipeline with the given registry and configuration.
    pub fn new(registry: Arc<StrategyRegistry>, config: PipelineConfig) -> Self {
        Self { registry, config }
    }

    /// Creates a pipeline with default configuration.
    pub fn with_defaults(registry: Arc<StrategyRegistry>) -> Self {
        Self::new(registry, PipelineConfig::default())
    }

    /// Returns a reference to the pipeline configuration.
    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }

    /// Returns a reference to the strategy registry.
    pub fn registry(&self) -> &StrategyRegistry {
        &self.registry
    }

    /// Evaluates an order against all active strategies and returns the best
    /// fill decision, or `None` if no strategy recommends filling.
    ///
    /// Strategies are evaluated concurrently (up to `max_concurrent_evaluations`),
    /// each with an individual timeout. Results are filtered by `min_confidence`
    /// and `min_profit_wei`, then ranked by composite score.
    pub async fn evaluate_order(
        &self,
        order: &DutchAuctionOrder,
        context: &EvaluationContext,
    ) -> StrategyResult<Option<FillDecision>> {
        let strategies = self.registry.active_strategies();
        if strategies.is_empty() {
            debug!(order_id = %order.id, "no active strategies, skipping");
            return Ok(None);
        }

        debug!(
            order_id = %order.id,
            strategy_count = strategies.len(),
            "evaluating order against strategies"
        );

        let decisions = self.evaluate_strategies(&strategies, order, context).await;

        let best = self.select_best(decisions);

        if let Some(ref decision) = best {
            info!(
                order_id = %order.id,
                strategy = %decision.strategy_name,
                profit = %decision.estimated_profit,
                confidence = decision.confidence,
                "selected fill decision"
            );
        } else {
            debug!(order_id = %order.id, "no strategy recommends filling");
        }

        Ok(best)
    }

    /// Runs evaluation on all strategies concurrently with timeouts.
    async fn evaluate_strategies(
        &self,
        strategies: &[Arc<dyn FillStrategy>],
        order: &DutchAuctionOrder,
        context: &EvaluationContext,
    ) -> Vec<FillDecision> {
        let timeout = std::time::Duration::from_millis(self.config.evaluation_timeout_ms);

        let futs: Vec<_> = strategies
            .iter()
            .take(self.config.max_concurrent_evaluations)
            .map(|strategy| {
                let strategy = Arc::clone(strategy);
                let order = order.clone();
                let context = context.clone();
                async move {
                    let name = strategy.name().to_string();
                    let result =
                        tokio::time::timeout(timeout, strategy.evaluate(&order, &context)).await;
                    (name, result)
                }
            })
            .collect();

        let results = futures::future::join_all(futs).await;

        let mut decisions = Vec::new();
        for (name, result) in results {
            match result {
                Ok(Ok(Some(decision))) => {
                    debug!(
                        strategy = %name,
                        profit = %decision.estimated_profit,
                        confidence = decision.confidence,
                        "strategy recommends fill"
                    );
                    decisions.push(decision);
                }
                Ok(Ok(None)) => {
                    debug!(strategy = %name, "strategy skipped order");
                }
                Ok(Err(e)) => {
                    warn!(strategy = %name, error = %e, "strategy evaluation failed");
                }
                Err(_) => {
                    warn!(
                        strategy = %name,
                        timeout_ms = self.config.evaluation_timeout_ms,
                        "strategy evaluation timed out"
                    );
                }
            }
        }

        decisions
    }

    /// Filters decisions by min thresholds and selects the one with the highest
    /// composite score (profit * confidence).
    fn select_best(&self, decisions: Vec<FillDecision>) -> Option<FillDecision> {
        decisions
            .into_iter()
            .filter(|d| d.confidence >= self.config.min_confidence)
            .filter(|d| d.estimated_profit >= self.config.min_profit_wei)
            .max_by_key(composite_score)
    }
}

/// Computes a composite score for ranking fill decisions.
/// Score = estimated_profit * confidence / 100.
fn composite_score(decision: &FillDecision) -> U256 {
    decision
        .estimated_profit
        .checked_mul(U256::from(decision.confidence))
        .unwrap_or(U256::MAX)
        / U256::from(100u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use phantom_common::error::StrategyError;
    use phantom_common::types::{ChainId, OrderId, OrderInput, OrderOutput};

    use alloy::primitives::{address, fixed_bytes};
    use chrono::{Duration, Utc};

    /// A test strategy that always fills with a configurable profit and confidence.
    struct AlwaysFillStrategy {
        name: String,
        priority: u32,
        profit: U256,
        confidence: u8,
    }

    impl AlwaysFillStrategy {
        fn new(name: &str, priority: u32, profit: u64, confidence: u8) -> Self {
            Self {
                name: name.to_string(),
                priority,
                profit: U256::from(profit),
                confidence,
            }
        }
    }

    #[async_trait]
    impl FillStrategy for AlwaysFillStrategy {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> u32 {
            self.priority
        }

        async fn evaluate(
            &self,
            order: &DutchAuctionOrder,
            _context: &EvaluationContext,
        ) -> StrategyResult<Option<FillDecision>> {
            Ok(Some(FillDecision {
                order_id: order.id,
                strategy_name: self.name.clone(),
                estimated_profit: self.profit,
                estimated_gas_cost: U256::from(100_000u64),
                confidence: self.confidence,
            }))
        }
    }

    /// A test strategy that always skips.
    struct NeverFillStrategy;

    #[async_trait]
    impl FillStrategy for NeverFillStrategy {
        fn name(&self) -> &str {
            "never_fill"
        }

        fn priority(&self) -> u32 {
            1
        }

        async fn evaluate(
            &self,
            _order: &DutchAuctionOrder,
            _context: &EvaluationContext,
        ) -> StrategyResult<Option<FillDecision>> {
            Ok(None)
        }
    }

    /// A test strategy that always errors.
    struct ErrorStrategy;

    #[async_trait]
    impl FillStrategy for ErrorStrategy {
        fn name(&self) -> &str {
            "error_strategy"
        }

        fn priority(&self) -> u32 {
            1
        }

        async fn evaluate(
            &self,
            _order: &DutchAuctionOrder,
            _context: &EvaluationContext,
        ) -> StrategyResult<Option<FillDecision>> {
            Err(StrategyError::SimulationFailed("test error".into()))
        }
    }

    /// A test strategy that hangs forever (for timeout tests).
    struct SlowStrategy;

    #[async_trait]
    impl FillStrategy for SlowStrategy {
        fn name(&self) -> &str {
            "slow_strategy"
        }

        fn priority(&self) -> u32 {
            1
        }

        async fn evaluate(
            &self,
            _order: &DutchAuctionOrder,
            _context: &EvaluationContext,
        ) -> StrategyResult<Option<FillDecision>> {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(None)
        }
    }

    fn sample_order() -> DutchAuctionOrder {
        let now = Utc::now();
        DutchAuctionOrder {
            id: OrderId::new(fixed_bytes!(
                "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
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
            base_fee: 30_000_000_000,
            priority_fee: 2_000_000_000,
        }
    }

    #[tokio::test]
    async fn no_strategies_returns_none() {
        let registry = Arc::new(StrategyRegistry::new());
        let pipeline = StrategyPipeline::with_defaults(registry);

        let result = pipeline
            .evaluate_order(&sample_order(), &sample_context())
            .await
            .expect("should succeed");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn single_fill_strategy() {
        let registry = Arc::new(StrategyRegistry::new());
        registry.register(Arc::new(AlwaysFillStrategy::new("arb", 1, 1000, 80)));

        let pipeline = StrategyPipeline::with_defaults(registry);
        let result = pipeline
            .evaluate_order(&sample_order(), &sample_context())
            .await
            .expect("should succeed");

        let decision = result.expect("should have decision");
        assert_eq!(decision.strategy_name, "arb");
        assert_eq!(decision.estimated_profit, U256::from(1000u64));
        assert_eq!(decision.confidence, 80);
    }

    #[tokio::test]
    async fn best_decision_selected() {
        let registry = Arc::new(StrategyRegistry::new());
        registry.register(Arc::new(AlwaysFillStrategy::new("low", 1, 500, 60)));
        registry.register(Arc::new(AlwaysFillStrategy::new("high", 2, 2000, 90)));
        registry.register(Arc::new(AlwaysFillStrategy::new("mid", 3, 1000, 80)));

        let pipeline = StrategyPipeline::with_defaults(registry);
        let result = pipeline
            .evaluate_order(&sample_order(), &sample_context())
            .await
            .expect("should succeed");

        let decision = result.expect("should have decision");
        // high: 2000 * 90 / 100 = 1800 (best)
        // mid:  1000 * 80 / 100 = 800
        // low:   500 * 60 / 100 = 300
        assert_eq!(decision.strategy_name, "high");
    }

    #[tokio::test]
    async fn never_fill_returns_none() {
        let registry = Arc::new(StrategyRegistry::new());
        registry.register(Arc::new(NeverFillStrategy));

        let pipeline = StrategyPipeline::with_defaults(registry);
        let result = pipeline
            .evaluate_order(&sample_order(), &sample_context())
            .await
            .expect("should succeed");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn error_strategy_handled_gracefully() {
        let registry = Arc::new(StrategyRegistry::new());
        registry.register(Arc::new(ErrorStrategy));
        registry.register(Arc::new(AlwaysFillStrategy::new("backup", 2, 500, 70)));

        let pipeline = StrategyPipeline::with_defaults(registry);
        let result = pipeline
            .evaluate_order(&sample_order(), &sample_context())
            .await
            .expect("should succeed");

        let decision = result.expect("backup should produce decision");
        assert_eq!(decision.strategy_name, "backup");
    }

    #[tokio::test]
    async fn timeout_strategy_handled() {
        let registry = Arc::new(StrategyRegistry::new());
        registry.register(Arc::new(SlowStrategy));
        registry.register(Arc::new(AlwaysFillStrategy::new("fast", 2, 500, 70)));

        let config = PipelineConfig {
            evaluation_timeout_ms: 100, // very short timeout
            ..PipelineConfig::default()
        };

        let pipeline = StrategyPipeline::new(
            {
                let r = Arc::new(StrategyRegistry::new());
                r.register(Arc::new(SlowStrategy));
                r.register(Arc::new(AlwaysFillStrategy::new("fast", 2, 500, 70)));
                r
            },
            config,
        );

        let result = pipeline
            .evaluate_order(&sample_order(), &sample_context())
            .await
            .expect("should succeed");

        let decision = result.expect("fast strategy should produce decision");
        assert_eq!(decision.strategy_name, "fast");
    }

    #[tokio::test]
    async fn min_confidence_filter() {
        let registry = Arc::new(StrategyRegistry::new());
        registry.register(Arc::new(AlwaysFillStrategy::new("low_conf", 1, 1000, 30)));

        let config = PipelineConfig {
            min_confidence: 50,
            ..PipelineConfig::default()
        };
        let pipeline = StrategyPipeline::new(registry, config);

        let result = pipeline
            .evaluate_order(&sample_order(), &sample_context())
            .await
            .expect("should succeed");
        assert!(result.is_none(), "low confidence should be filtered");
    }

    #[tokio::test]
    async fn min_profit_filter() {
        let registry = Arc::new(StrategyRegistry::new());
        registry.register(Arc::new(AlwaysFillStrategy::new("tiny", 1, 10, 90)));

        let config = PipelineConfig {
            min_profit_wei: U256::from(1000u64),
            ..PipelineConfig::default()
        };
        let pipeline = StrategyPipeline::new(registry, config);

        let result = pipeline
            .evaluate_order(&sample_order(), &sample_context())
            .await
            .expect("should succeed");
        assert!(result.is_none(), "low profit should be filtered");
    }

    #[test]
    fn composite_score_calculation() {
        let decision = FillDecision {
            order_id: OrderId::new(fixed_bytes!(
                "0000000000000000000000000000000000000000000000000000000000000000"
            )),
            strategy_name: "test".into(),
            estimated_profit: U256::from(1000u64),
            estimated_gas_cost: U256::from(100u64),
            confidence: 80,
        };

        let score = composite_score(&decision);
        // 1000 * 80 / 100 = 800
        assert_eq!(score, U256::from(800u64));
    }

    #[test]
    fn pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.min_confidence, 50);
        assert_eq!(config.min_profit_wei, U256::ZERO);
        assert_eq!(config.evaluation_timeout_ms, 5_000);
        assert_eq!(config.max_concurrent_evaluations, 10);
    }

    #[test]
    fn pipeline_config_serde_roundtrip() {
        let config = PipelineConfig {
            min_confidence: 75,
            min_profit_wei: U256::from(500u64),
            evaluation_timeout_ms: 3_000,
            max_concurrent_evaluations: 5,
        };

        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: PipelineConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.min_confidence, 75);
        assert_eq!(deserialized.max_concurrent_evaluations, 5);
    }
}
