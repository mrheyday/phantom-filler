//! Gas price oracle and fill transaction cost estimation.
//!
//! Provides EIP-1559 aware gas pricing with multiple urgency levels,
//! per-chain gas model support, and fill transaction cost estimation.
//! Fetches base fee from recent block history and priority fee suggestions
//! from the provider, then applies urgency multipliers.

use std::sync::Arc;

use alloy::primitives::U256;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use phantom_chain::provider::DynProvider;
use phantom_common::error::PricingResult;
use phantom_common::types::ChainId;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Urgency level for gas pricing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GasPriceLevel {
    /// Lowest cost, slower inclusion (~30s+).
    Low,
    /// Balanced cost and speed (~15s).
    Medium,
    /// Fastest inclusion, highest cost (~6s).
    High,
}

/// EIP-1559 gas price components in wei.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GasPrice {
    /// Current base fee per gas (wei).
    pub base_fee: u128,
    /// Priority fee (tip) per gas (wei).
    pub priority_fee: u128,
    /// Maximum fee per gas the sender is willing to pay (wei).
    /// Calculated as `2 * base_fee + priority_fee`.
    pub max_fee_per_gas: u128,
    /// The urgency level used to derive this price.
    pub level: GasPriceLevel,
}

/// Estimated cost for executing a fill transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GasEstimate {
    /// The gas price components used for this estimate.
    pub gas_price: GasPrice,
    /// Estimated gas units required for the transaction.
    pub gas_limit: u64,
    /// Total estimated cost in wei (`gas_limit * max_fee_per_gas`).
    pub total_cost_wei: U256,
}

impl GasEstimate {
    /// Returns the total cost as a floating-point value in Gwei.
    pub fn total_cost_gwei(&self) -> f64 {
        let gwei = self.total_cost_wei / U256::from(1_000_000_000u64);
        // Safe for values < 2^53 (~9.2 million Gwei ≈ 0.0092 ETH)
        gwei.try_into().unwrap_or(u64::MAX) as f64
    }
}

/// Configuration for the gas oracle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GasOracleConfig {
    /// Cache TTL in seconds. Default: 12 (one block).
    pub cache_ttl_seconds: u64,
    /// Number of recent blocks to sample for fee history. Default: 5.
    pub fee_history_blocks: u64,
    /// Priority fee multiplier for Low urgency (basis points, 10000 = 1.0x). Default: 10000.
    pub low_priority_bps: u64,
    /// Priority fee multiplier for Medium urgency (basis points). Default: 12500 (1.25x).
    pub medium_priority_bps: u64,
    /// Priority fee multiplier for High urgency (basis points). Default: 15000 (1.5x).
    pub high_priority_bps: u64,
    /// Fallback base fee in Gwei when provider data is unavailable. Default: 30.
    pub fallback_base_fee_gwei: u64,
    /// Fallback priority fee in Gwei. Default: 2.
    pub fallback_priority_fee_gwei: u64,
    /// Default gas limit for simple fill transactions. Default: 200_000.
    pub default_fill_gas: u64,
    /// Default gas limit for complex (multi-hop) fill transactions. Default: 350_000.
    pub complex_fill_gas: u64,
}

impl Default for GasOracleConfig {
    fn default() -> Self {
        Self {
            cache_ttl_seconds: 12,
            fee_history_blocks: 5,
            low_priority_bps: 10_000,
            medium_priority_bps: 12_500,
            high_priority_bps: 15_000,
            fallback_base_fee_gwei: 30,
            fallback_priority_fee_gwei: 2,
            default_fill_gas: 200_000,
            complex_fill_gas: 350_000,
        }
    }
}

// ---------------------------------------------------------------------------
// L2 gas adjustments
// ---------------------------------------------------------------------------

/// Per-chain gas model adjustments.
/// L2s generally have much lower base fees but may have L1 data costs.
fn chain_base_fee_floor(chain_id: ChainId) -> u128 {
    match chain_id {
        ChainId::Ethereum => 1_000_000_000, // 1 Gwei floor
        ChainId::Arbitrum => 100_000_000,   // 0.1 Gwei
        ChainId::Base => 1_000_000,         // 0.001 Gwei
        ChainId::Optimism => 1_000_000,     // 0.001 Gwei
        ChainId::Polygon => 30_000_000_000, // 30 Gwei (Polygon is cheap but higher min)
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CachedGasData {
    base_fee: u128,
    suggested_priority_fee: u128,
    cached_at: DateTime<Utc>,
}

type CacheKey = ChainId;

// ---------------------------------------------------------------------------
// GasOracle
// ---------------------------------------------------------------------------

/// Gas price oracle that fetches EIP-1559 fee data from an on-chain provider.
pub struct GasOracle {
    provider: Arc<DynProvider>,
    chain_id: ChainId,
    config: GasOracleConfig,
    cache: DashMap<CacheKey, CachedGasData>,
}

impl GasOracle {
    /// Creates a new gas oracle for the given chain.
    pub fn new(provider: Arc<DynProvider>, chain_id: ChainId, config: GasOracleConfig) -> Self {
        Self {
            provider,
            chain_id,
            config,
            cache: DashMap::new(),
        }
    }

    /// Creates a gas oracle with default configuration.
    pub fn with_defaults(provider: Arc<DynProvider>, chain_id: ChainId) -> Self {
        Self::new(provider, chain_id, GasOracleConfig::default())
    }

    /// Returns the chain this oracle is configured for.
    pub fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    /// Returns a reference to the current configuration.
    pub fn config(&self) -> &GasOracleConfig {
        &self.config
    }

    /// Clears the cached gas data.
    pub fn invalidate_cache(&self) {
        self.cache.clear();
    }

    /// Returns the current gas price for the given urgency level.
    pub async fn get_gas_price(&self, level: GasPriceLevel) -> PricingResult<GasPrice> {
        let (base_fee, suggested_priority) = self.fetch_fee_data().await?;

        let priority_fee = apply_urgency(suggested_priority, level, &self.config);
        let max_fee_per_gas = compute_max_fee(base_fee, priority_fee);

        Ok(GasPrice {
            base_fee,
            priority_fee,
            max_fee_per_gas,
            level,
        })
    }

    /// Estimates the cost of a fill transaction with the given gas limit.
    pub async fn estimate_fill_cost(
        &self,
        gas_limit: u64,
        level: GasPriceLevel,
    ) -> PricingResult<GasEstimate> {
        let gas_price = self.get_gas_price(level).await?;
        let total_cost_wei = U256::from(gas_limit) * U256::from(gas_price.max_fee_per_gas);

        Ok(GasEstimate {
            gas_price,
            gas_limit,
            total_cost_wei,
        })
    }

    /// Estimates cost for a standard (simple) fill.
    pub async fn estimate_simple_fill(&self, level: GasPriceLevel) -> PricingResult<GasEstimate> {
        self.estimate_fill_cost(self.config.default_fill_gas, level)
            .await
    }

    /// Estimates cost for a complex (multi-hop) fill.
    pub async fn estimate_complex_fill(&self, level: GasPriceLevel) -> PricingResult<GasEstimate> {
        self.estimate_fill_cost(self.config.complex_fill_gas, level)
            .await
    }

    /// Fetches fee data from the provider, using cache when fresh.
    async fn fetch_fee_data(&self) -> PricingResult<(u128, u128)> {
        // Check cache.
        if let Some(cached) = self.cache.get(&self.chain_id) {
            let age = (Utc::now() - cached.cached_at).num_seconds().unsigned_abs();
            if age <= self.config.cache_ttl_seconds {
                debug!(
                    chain = ?self.chain_id,
                    age_secs = age,
                    "returning cached gas data"
                );
                return Ok((cached.base_fee, cached.suggested_priority_fee));
            }
        }

        // Fetch fresh data from provider.
        let (base_fee, priority_fee) = self.fetch_from_provider().await;

        // Cache result.
        self.cache.insert(
            self.chain_id,
            CachedGasData {
                base_fee,
                suggested_priority_fee: priority_fee,
                cached_at: Utc::now(),
            },
        );

        Ok((base_fee, priority_fee))
    }

    /// Queries the provider for base fee (via fee history) and priority fee.
    /// Falls back to configured defaults on any error.
    async fn fetch_from_provider(&self) -> (u128, u128) {
        let base_fee = self.fetch_base_fee().await;
        let priority_fee = self.fetch_priority_fee().await;
        (base_fee, priority_fee)
    }

    /// Fetches the latest base fee from fee history.
    async fn fetch_base_fee(&self) -> u128 {
        match self
            .provider
            .get_fee_history(
                self.config.fee_history_blocks,
                alloy::eips::BlockNumberOrTag::Latest,
                &[],
            )
            .await
        {
            Ok(history) => {
                // Use the latest base fee from history.
                if let Some(&fee) = history.base_fee_per_gas.last() {
                    let fee = fee.max(chain_base_fee_floor(self.chain_id));
                    debug!(chain = ?self.chain_id, base_fee = fee, "fetched base fee");
                    fee
                } else {
                    warn!(chain = ?self.chain_id, "empty fee history, using fallback");
                    u128::from(self.config.fallback_base_fee_gwei) * 1_000_000_000
                }
            }
            Err(e) => {
                warn!(
                    chain = ?self.chain_id,
                    error = %e,
                    "fee history fetch failed, using fallback"
                );
                u128::from(self.config.fallback_base_fee_gwei) * 1_000_000_000
            }
        }
    }

    /// Fetches the suggested priority fee from the provider.
    async fn fetch_priority_fee(&self) -> u128 {
        match self.provider.get_max_priority_fee_per_gas().await {
            Ok(fee) => {
                debug!(chain = ?self.chain_id, priority_fee = fee, "fetched priority fee");
                fee
            }
            Err(e) => {
                warn!(
                    chain = ?self.chain_id,
                    error = %e,
                    "priority fee fetch failed, using fallback"
                );
                u128::from(self.config.fallback_priority_fee_gwei) * 1_000_000_000
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Applies the urgency multiplier to the suggested priority fee.
fn apply_urgency(suggested: u128, level: GasPriceLevel, config: &GasOracleConfig) -> u128 {
    let bps = match level {
        GasPriceLevel::Low => config.low_priority_bps,
        GasPriceLevel::Medium => config.medium_priority_bps,
        GasPriceLevel::High => config.high_priority_bps,
    };
    // suggested * bps / 10_000
    suggested
        .checked_mul(u128::from(bps))
        .map(|v| v / 10_000)
        .unwrap_or(suggested)
}

/// Computes EIP-1559 max fee: `2 * base_fee + priority_fee`.
fn compute_max_fee(base_fee: u128, priority_fee: u128) -> u128 {
    base_fee
        .checked_mul(2)
        .and_then(|doubled| doubled.checked_add(priority_fee))
        .unwrap_or(u128::MAX)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- GasPriceLevel -------------------------------------------------------

    #[test]
    fn gas_price_level_serialization() {
        let json = serde_json::to_string(&GasPriceLevel::Medium).expect("serialize");
        assert_eq!(json, "\"medium\"");

        let parsed: GasPriceLevel = serde_json::from_str("\"high\"").expect("deserialize");
        assert_eq!(parsed, GasPriceLevel::High);
    }

    // -- GasOracleConfig defaults --------------------------------------------

    #[test]
    fn config_defaults() {
        let config = GasOracleConfig::default();
        assert_eq!(config.cache_ttl_seconds, 12);
        assert_eq!(config.fee_history_blocks, 5);
        assert_eq!(config.low_priority_bps, 10_000);
        assert_eq!(config.medium_priority_bps, 12_500);
        assert_eq!(config.high_priority_bps, 15_000);
        assert_eq!(config.fallback_base_fee_gwei, 30);
        assert_eq!(config.fallback_priority_fee_gwei, 2);
        assert_eq!(config.default_fill_gas, 200_000);
        assert_eq!(config.complex_fill_gas, 350_000);
    }

    // -- apply_urgency -------------------------------------------------------

    #[test]
    fn urgency_low_is_1x() {
        let config = GasOracleConfig::default();
        let result = apply_urgency(2_000_000_000, GasPriceLevel::Low, &config);
        assert_eq!(result, 2_000_000_000); // 1.0x
    }

    #[test]
    fn urgency_medium_is_1_25x() {
        let config = GasOracleConfig::default();
        let result = apply_urgency(2_000_000_000, GasPriceLevel::Medium, &config);
        assert_eq!(result, 2_500_000_000); // 1.25x
    }

    #[test]
    fn urgency_high_is_1_5x() {
        let config = GasOracleConfig::default();
        let result = apply_urgency(2_000_000_000, GasPriceLevel::High, &config);
        assert_eq!(result, 3_000_000_000); // 1.5x
    }

    #[test]
    fn urgency_zero_fee() {
        let config = GasOracleConfig::default();
        let result = apply_urgency(0, GasPriceLevel::High, &config);
        assert_eq!(result, 0);
    }

    #[test]
    fn urgency_custom_bps() {
        let config = GasOracleConfig {
            high_priority_bps: 20_000, // 2x
            ..Default::default()
        };
        let result = apply_urgency(1_000_000_000, GasPriceLevel::High, &config);
        assert_eq!(result, 2_000_000_000);
    }

    // -- compute_max_fee -----------------------------------------------------

    #[test]
    fn max_fee_formula() {
        // max_fee = 2 * 30 Gwei + 2 Gwei = 62 Gwei
        let base = 30_000_000_000u128;
        let priority = 2_000_000_000u128;
        let max_fee = compute_max_fee(base, priority);
        assert_eq!(max_fee, 62_000_000_000u128);
    }

    #[test]
    fn max_fee_zero_base() {
        let max_fee = compute_max_fee(0, 1_000_000_000);
        assert_eq!(max_fee, 1_000_000_000);
    }

    #[test]
    fn max_fee_overflow_saturates() {
        let max_fee = compute_max_fee(u128::MAX, 1);
        assert_eq!(max_fee, u128::MAX);
    }

    // -- GasEstimate ---------------------------------------------------------

    #[test]
    fn gas_estimate_total_cost() {
        let estimate = GasEstimate {
            gas_price: GasPrice {
                base_fee: 30_000_000_000,
                priority_fee: 2_000_000_000,
                max_fee_per_gas: 62_000_000_000,
                level: GasPriceLevel::Medium,
            },
            gas_limit: 200_000,
            total_cost_wei: U256::from(200_000u64) * U256::from(62_000_000_000u128),
        };
        // 200k * 62 Gwei = 12,400,000 Gwei = 0.0124 ETH
        let expected = U256::from(200_000u64) * U256::from(62_000_000_000u128);
        assert_eq!(estimate.total_cost_wei, expected);
    }

    #[test]
    fn gas_estimate_gwei_conversion() {
        let estimate = GasEstimate {
            gas_price: GasPrice {
                base_fee: 30_000_000_000,
                priority_fee: 2_000_000_000,
                max_fee_per_gas: 62_000_000_000,
                level: GasPriceLevel::Medium,
            },
            gas_limit: 200_000,
            total_cost_wei: U256::from(12_400_000_000_000_000u128), // 12.4M Gwei
        };
        let gwei = estimate.total_cost_gwei();
        assert!((gwei - 12_400_000.0).abs() < 1.0);
    }

    // -- chain_base_fee_floor ------------------------------------------------

    #[test]
    fn ethereum_base_fee_floor() {
        assert_eq!(chain_base_fee_floor(ChainId::Ethereum), 1_000_000_000);
    }

    #[test]
    fn arbitrum_base_fee_floor() {
        assert_eq!(chain_base_fee_floor(ChainId::Arbitrum), 100_000_000);
    }

    #[test]
    fn l2_chains_have_lower_floor() {
        assert!(chain_base_fee_floor(ChainId::Base) < chain_base_fee_floor(ChainId::Ethereum));
        assert!(chain_base_fee_floor(ChainId::Optimism) < chain_base_fee_floor(ChainId::Ethereum));
    }

    // -- GasOracle construction ----------------------------------------------

    #[test]
    fn oracle_creation() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let oracle = GasOracle::with_defaults(provider, ChainId::Ethereum);
        assert_eq!(oracle.chain_id(), ChainId::Ethereum);
        assert_eq!(oracle.config().cache_ttl_seconds, 12);
    }

    #[test]
    fn oracle_custom_config() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let config = GasOracleConfig {
            cache_ttl_seconds: 6,
            default_fill_gas: 300_000,
            ..Default::default()
        };
        let oracle = GasOracle::new(provider, ChainId::Arbitrum, config);
        assert_eq!(oracle.chain_id(), ChainId::Arbitrum);
        assert_eq!(oracle.config().default_fill_gas, 300_000);
        assert_eq!(oracle.config().cache_ttl_seconds, 6);
    }

    #[test]
    fn oracle_invalidate_cache() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let oracle = GasOracle::with_defaults(provider, ChainId::Ethereum);
        // Insert a cache entry manually.
        oracle.cache.insert(
            ChainId::Ethereum,
            CachedGasData {
                base_fee: 30_000_000_000,
                suggested_priority_fee: 2_000_000_000,
                cached_at: Utc::now(),
            },
        );
        assert_eq!(oracle.cache.len(), 1);
        oracle.invalidate_cache();
        assert_eq!(oracle.cache.len(), 0);
    }

    // -- GasPrice serialization ----------------------------------------------

    #[test]
    fn gas_price_serialization() {
        let price = GasPrice {
            base_fee: 30_000_000_000,
            priority_fee: 2_000_000_000,
            max_fee_per_gas: 62_000_000_000,
            level: GasPriceLevel::Medium,
        };
        let json = serde_json::to_string(&price).expect("serialize");
        assert!(json.contains("\"level\":\"medium\""));
        let parsed: GasPrice = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, price);
    }

    // -- Async tests with fallback provider ----------------------------------

    #[tokio::test]
    async fn oracle_fallback_on_provider_error() {
        // Using an unreachable provider triggers fallback values.
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://unreachable.invalid".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let oracle = GasOracle::with_defaults(provider, ChainId::Ethereum);
        let price = oracle.get_gas_price(GasPriceLevel::Medium).await;

        // Should succeed with fallback values, not error.
        let price = price.expect("should fallback gracefully");
        // Fallback: base=30 Gwei, priority=2 Gwei * 1.25 = 2.5 Gwei
        assert_eq!(price.base_fee, 30_000_000_000);
        assert_eq!(price.priority_fee, 2_500_000_000); // 2 Gwei * 1.25
        assert_eq!(price.max_fee_per_gas, 62_500_000_000); // 2*30 + 2.5
    }

    #[tokio::test]
    async fn oracle_estimate_simple_fill_fallback() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://unreachable.invalid".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let oracle = GasOracle::with_defaults(provider, ChainId::Ethereum);
        let estimate = oracle
            .estimate_simple_fill(GasPriceLevel::Low)
            .await
            .expect("should fallback");

        assert_eq!(estimate.gas_limit, 200_000);
        // Low urgency: priority = 2 Gwei * 1.0 = 2 Gwei
        // max_fee = 2*30 + 2 = 62 Gwei
        // total = 200k * 62 Gwei = 12,400,000 Gwei
        let expected_cost = U256::from(200_000u64) * U256::from(62_000_000_000u128);
        assert_eq!(estimate.total_cost_wei, expected_cost);
    }

    #[tokio::test]
    async fn oracle_estimate_complex_fill_fallback() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://unreachable.invalid".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let oracle = GasOracle::with_defaults(provider, ChainId::Ethereum);
        let estimate = oracle
            .estimate_complex_fill(GasPriceLevel::High)
            .await
            .expect("should fallback");

        assert_eq!(estimate.gas_limit, 350_000);
        // High urgency: priority = 2 Gwei * 1.5 = 3 Gwei
        // max_fee = 2*30 + 3 = 63 Gwei
        let expected_cost = U256::from(350_000u64) * U256::from(63_000_000_000u128);
        assert_eq!(estimate.total_cost_wei, expected_cost);
    }

    #[tokio::test]
    async fn oracle_caches_across_calls() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://unreachable.invalid".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let oracle = GasOracle::with_defaults(provider, ChainId::Ethereum);

        // First call populates cache.
        let price1 = oracle
            .get_gas_price(GasPriceLevel::Medium)
            .await
            .expect("first call");

        // Second call should hit cache (same values).
        let price2 = oracle
            .get_gas_price(GasPriceLevel::Medium)
            .await
            .expect("second call");

        assert_eq!(price1.base_fee, price2.base_fee);
        assert_eq!(oracle.cache.len(), 1);
    }

    #[tokio::test]
    async fn oracle_different_levels_same_base() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://unreachable.invalid".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let oracle = GasOracle::with_defaults(provider, ChainId::Ethereum);

        let low = oracle.get_gas_price(GasPriceLevel::Low).await.expect("low");
        let high = oracle
            .get_gas_price(GasPriceLevel::High)
            .await
            .expect("high");

        // Same base fee, different priority fees.
        assert_eq!(low.base_fee, high.base_fee);
        assert!(high.priority_fee > low.priority_fee);
        assert!(high.max_fee_per_gas > low.max_fee_per_gas);
    }
}
