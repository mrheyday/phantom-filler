//! Price aggregation engine.
//!
//! Collects prices from multiple on-chain/off-chain sources, filters outliers,
//! calculates median/mean, scores confidence, and caches results with
//! configurable TTL.

use std::sync::Arc;

use alloy::primitives::U256;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use phantom_common::error::PricingError;
use phantom_common::traits::PriceSource;
use phantom_common::types::{ChainId, Token};
use tracing::{debug, warn};

/// Configuration for the price aggregator.
#[derive(Debug, Clone)]
pub struct AggregatorConfig {
    /// How long cached prices are valid (seconds).
    pub cache_ttl_seconds: u64,
    /// Minimum number of sources required for a valid aggregate price.
    pub min_sources: usize,
    /// Maximum percentage deviation from the median before a price is
    /// considered an outlier and excluded. E.g., `10` means ±10%.
    pub max_deviation_pct: u64,
    /// Per-source query timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for AggregatorConfig {
    fn default() -> Self {
        Self {
            cache_ttl_seconds: 30,
            min_sources: 1,
            max_deviation_pct: 10,
            timeout_ms: 5000,
        }
    }
}

/// An aggregated price derived from multiple sources.
#[derive(Debug, Clone)]
pub struct AggregatedPrice {
    /// Median price across contributing sources.
    pub median_price: U256,
    /// Arithmetic mean price.
    pub mean_price: U256,
    /// Number of sources that contributed (after outlier filtering).
    pub num_sources: usize,
    /// Confidence score from 0–100.
    pub confidence: u8,
    /// When this aggregate was computed.
    pub timestamp: DateTime<Utc>,
    /// Names of the sources that contributed.
    pub sources: Vec<String>,
}

/// Cache key combining the two token addresses and the chain.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    base: alloy::primitives::Address,
    quote: alloy::primitives::Address,
    chain_id: ChainId,
}

/// A cached aggregate price entry.
#[derive(Debug, Clone)]
struct CachedPrice {
    price: AggregatedPrice,
    cached_at: DateTime<Utc>,
}

/// Multi-source price aggregator with caching, outlier filtering, and
/// confidence scoring.
pub struct PriceAggregator {
    sources: Vec<Arc<dyn PriceSource>>,
    cache: DashMap<CacheKey, CachedPrice>,
    config: AggregatorConfig,
}

impl PriceAggregator {
    /// Creates a new aggregator with the given configuration.
    pub fn new(config: AggregatorConfig) -> Self {
        Self {
            sources: Vec::new(),
            cache: DashMap::new(),
            config,
        }
    }

    /// Registers a price source.
    pub fn add_source(&mut self, source: Arc<dyn PriceSource>) {
        self.sources.push(source);
    }

    /// Returns the number of registered sources.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Clears the entire price cache.
    pub fn invalidate_cache(&self) {
        self.cache.clear();
    }

    /// Returns the number of entries in the cache.
    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }

    /// Fetches an aggregated price for the given token pair.
    ///
    /// Checks cache first; on miss, queries all registered sources
    /// concurrently, filters outliers, and caches the result.
    pub async fn get_price(
        &self,
        base: &Token,
        quote: &Token,
        chain_id: ChainId,
    ) -> Result<AggregatedPrice, PricingError> {
        let key = CacheKey {
            base: base.address,
            quote: quote.address,
            chain_id,
        };

        // Check cache.
        if let Some(cached) = self.cache.get(&key) {
            let age = (Utc::now() - cached.cached_at).num_seconds().unsigned_abs();
            if age <= self.config.cache_ttl_seconds {
                debug!(
                    base = %base.symbol,
                    quote_token = %quote.symbol,
                    age_secs = age,
                    "returning cached price"
                );
                return Ok(cached.price.clone());
            }
        }

        // Query all sources concurrently.
        let timeout = std::time::Duration::from_millis(self.config.timeout_ms);

        let futs: Vec<_> = self
            .sources
            .iter()
            .map(|source| {
                let source = Arc::clone(source);
                let base = base.clone();
                let quote = quote.clone();
                async move {
                    let result =
                        tokio::time::timeout(timeout, source.get_price(&base, &quote, chain_id))
                            .await;
                    (source.name().to_string(), result)
                }
            })
            .collect();

        let results = futures::future::join_all(futs).await;

        let mut raw_prices: Vec<(U256, String)> = Vec::new();
        for (name, result) in results {
            match result {
                Ok(Ok(price)) => {
                    debug!(source = %name, price = %price, "got price from source");
                    raw_prices.push((price, name));
                }
                Ok(Err(e)) => {
                    warn!(source = %name, error = %e, "source returned error");
                }
                Err(_) => {
                    warn!(source = %name, "source timed out");
                }
            }
        }

        if raw_prices.is_empty() {
            return Err(PricingError::NoPriceAvailable {
                token_a: base.symbol.clone(),
                token_b: quote.symbol.clone(),
                chain_id,
            });
        }

        // Extract just the prices for math.
        let prices: Vec<U256> = raw_prices.iter().map(|(p, _)| *p).collect();

        // Filter outliers based on median deviation.
        let median = calculate_median(&prices);
        let filtered: Vec<(U256, String)> = raw_prices
            .into_iter()
            .filter(|(p, _)| !is_outlier(*p, median, self.config.max_deviation_pct))
            .collect();

        if filtered.len() < self.config.min_sources {
            return Err(PricingError::NoPriceAvailable {
                token_a: base.symbol.clone(),
                token_b: quote.symbol.clone(),
                chain_id,
            });
        }

        let filtered_prices: Vec<U256> = filtered.iter().map(|(p, _)| *p).collect();
        let source_names: Vec<String> = filtered.iter().map(|(_, s)| s.clone()).collect();

        let agg = AggregatedPrice {
            median_price: calculate_median(&filtered_prices),
            mean_price: calculate_mean(&filtered_prices),
            num_sources: filtered_prices.len(),
            confidence: calculate_confidence(
                filtered_prices.len(),
                self.config.min_sources,
                &filtered_prices,
            ),
            timestamp: Utc::now(),
            sources: source_names,
        };

        // Cache.
        self.cache.insert(
            key,
            CachedPrice {
                price: agg.clone(),
                cached_at: Utc::now(),
            },
        );

        Ok(agg)
    }
}

// ── Pure helper functions ────────────────────────────────────────────

/// Calculates the median of a non-empty slice of U256 values.
pub fn calculate_median(prices: &[U256]) -> U256 {
    assert!(!prices.is_empty(), "cannot compute median of empty slice");
    let mut sorted = prices.to_vec();
    sorted.sort();
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        // Average of the two middle values.
        (sorted[mid - 1] + sorted[mid]) / U256::from(2u64)
    } else {
        sorted[mid]
    }
}

/// Calculates the arithmetic mean of a non-empty slice.
pub fn calculate_mean(prices: &[U256]) -> U256 {
    assert!(!prices.is_empty(), "cannot compute mean of empty slice");
    let sum: U256 = prices.iter().copied().fold(U256::ZERO, |acc, p| acc + p);
    sum / U256::from(prices.len() as u64)
}

/// Returns `true` if `price` deviates from `median` by more than
/// `max_deviation_pct` percent.
pub fn is_outlier(price: U256, median: U256, max_deviation_pct: u64) -> bool {
    if median.is_zero() {
        return false;
    }
    let threshold = median * U256::from(max_deviation_pct) / U256::from(100u64);
    let diff = if price > median {
        price - median
    } else {
        median - price
    };
    diff > threshold
}

/// Scores confidence from 0–100 based on source count and price agreement.
///
/// - More sources → higher confidence
/// - Tighter spread → higher confidence
pub fn calculate_confidence(num_sources: usize, min_sources: usize, prices: &[U256]) -> u8 {
    if num_sources == 0 || prices.is_empty() {
        return 0;
    }

    // Source count component (0–50): scales linearly, max at 5 sources.
    let source_score = ((num_sources as u64).min(5) * 10).min(50);

    // Agreement component (0–50): based on relative spread.
    let agreement_score = if prices.len() < 2 {
        // Single source → moderate agreement.
        25u64
    } else {
        let sorted: Vec<U256> = {
            let mut v = prices.to_vec();
            v.sort();
            v
        };
        let min_p = sorted[0];
        let max_p = sorted[sorted.len() - 1];
        let median = calculate_median(prices);

        if median.is_zero() {
            0
        } else {
            let spread_pct = (max_p - min_p) * U256::from(100u64) / median;
            let spread: u64 = spread_pct.try_into().unwrap_or(u64::MAX);
            // 0% spread → 50 points, 20%+ spread → 0 points.
            50u64.saturating_sub(spread.saturating_mul(50) / 20)
        }
    };

    // Penalise if we barely meet the minimum.
    let min_penalty = if num_sources <= min_sources { 10u64 } else { 0 };

    let total = (source_score + agreement_score).saturating_sub(min_penalty);
    total.min(100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    // ── Pure function tests ──────────────────────────────────────────

    #[test]
    fn median_single_value() {
        assert_eq!(calculate_median(&[U256::from(100u64)]), U256::from(100u64));
    }

    #[test]
    fn median_odd_count() {
        let prices = [U256::from(10u64), U256::from(30u64), U256::from(20u64)];
        assert_eq!(calculate_median(&prices), U256::from(20u64));
    }

    #[test]
    fn median_even_count() {
        let prices = [
            U256::from(10u64),
            U256::from(20u64),
            U256::from(30u64),
            U256::from(40u64),
        ];
        // (20 + 30) / 2 = 25
        assert_eq!(calculate_median(&prices), U256::from(25u64));
    }

    #[test]
    fn mean_basic() {
        let prices = [U256::from(10u64), U256::from(20u64), U256::from(30u64)];
        assert_eq!(calculate_mean(&prices), U256::from(20u64));
    }

    #[test]
    fn mean_single() {
        assert_eq!(calculate_mean(&[U256::from(42u64)]), U256::from(42u64));
    }

    #[test]
    fn outlier_within_threshold() {
        let median = U256::from(1000u64);
        // 5% deviation = within 10% threshold.
        assert!(!is_outlier(U256::from(1050u64), median, 10));
        assert!(!is_outlier(U256::from(950u64), median, 10));
    }

    #[test]
    fn outlier_exceeds_threshold() {
        let median = U256::from(1000u64);
        // 15% deviation > 10% threshold.
        assert!(is_outlier(U256::from(1150u64), median, 10));
        assert!(is_outlier(U256::from(850u64), median, 10));
    }

    #[test]
    fn outlier_zero_median() {
        // Zero median → nothing is an outlier (avoid divide-by-zero).
        assert!(!is_outlier(U256::from(100u64), U256::ZERO, 10));
    }

    #[test]
    fn outlier_exact_threshold() {
        let median = U256::from(1000u64);
        // Exactly 10% deviation → threshold = 100 → diff = 100 → NOT outlier (> not >=).
        assert!(!is_outlier(U256::from(1100u64), median, 10));
    }

    #[test]
    fn confidence_no_sources() {
        assert_eq!(calculate_confidence(0, 1, &[]), 0);
    }

    #[test]
    fn confidence_single_source() {
        let prices = [U256::from(1000u64)];
        let conf = calculate_confidence(1, 1, &prices);
        // 1 source: source_score=10, agreement=25, min_penalty=10 → 25
        assert_eq!(conf, 25);
    }

    #[test]
    fn confidence_multiple_tight_sources() {
        let prices = [U256::from(1000u64), U256::from(1001u64), U256::from(999u64)];
        let conf = calculate_confidence(3, 1, &prices);
        // 3 sources: source_score=30, tight spread → high agreement
        assert!(conf > 50);
    }

    #[test]
    fn confidence_increases_with_sources() {
        let prices_1 = [U256::from(1000u64)];
        let prices_3 = [
            U256::from(1000u64),
            U256::from(1000u64),
            U256::from(1000u64),
        ];
        let conf_1 = calculate_confidence(1, 1, &prices_1);
        let conf_3 = calculate_confidence(3, 1, &prices_3);
        assert!(conf_3 > conf_1);
    }

    // ── Aggregator tests ─────────────────────────────────────────────

    #[test]
    fn aggregator_default_config() {
        let config = AggregatorConfig::default();
        assert_eq!(config.cache_ttl_seconds, 30);
        assert_eq!(config.min_sources, 1);
        assert_eq!(config.max_deviation_pct, 10);
    }

    #[test]
    fn aggregator_add_sources() {
        use async_trait::async_trait;

        struct MockSource;

        #[async_trait]
        impl PriceSource for MockSource {
            fn name(&self) -> &str {
                "mock"
            }
            async fn get_price(
                &self,
                _base: &Token,
                _quote: &Token,
                _chain_id: ChainId,
            ) -> Result<U256, PricingError> {
                Ok(U256::from(1000u64))
            }
        }

        let mut agg = PriceAggregator::new(AggregatorConfig::default());
        assert_eq!(agg.source_count(), 0);

        agg.add_source(Arc::new(MockSource));
        assert_eq!(agg.source_count(), 1);

        agg.add_source(Arc::new(MockSource));
        assert_eq!(agg.source_count(), 2);
    }

    #[tokio::test]
    async fn aggregator_single_source() {
        use async_trait::async_trait;

        struct FixedSource(U256);

        #[async_trait]
        impl PriceSource for FixedSource {
            fn name(&self) -> &str {
                "fixed"
            }
            async fn get_price(
                &self,
                _base: &Token,
                _quote: &Token,
                _chain_id: ChainId,
            ) -> Result<U256, PricingError> {
                Ok(self.0)
            }
        }

        let mut agg = PriceAggregator::new(AggregatorConfig::default());
        agg.add_source(Arc::new(FixedSource(U256::from(2000u64))));

        let base = Token::new(
            address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            ChainId::Ethereum,
            18,
            "WETH",
        );
        let quote_tok = Token::new(
            address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            ChainId::Ethereum,
            6,
            "USDC",
        );

        let result = agg.get_price(&base, &quote_tok, ChainId::Ethereum).await;
        assert!(result.is_ok());
        let price = result.unwrap();
        assert_eq!(price.median_price, U256::from(2000u64));
        assert_eq!(price.mean_price, U256::from(2000u64));
        assert_eq!(price.num_sources, 1);
    }

    #[tokio::test]
    async fn aggregator_multiple_sources_outlier_filtered() {
        use async_trait::async_trait;

        struct FixedSource {
            name: &'static str,
            price: U256,
        }

        #[async_trait]
        impl PriceSource for FixedSource {
            fn name(&self) -> &str {
                self.name
            }
            async fn get_price(
                &self,
                _base: &Token,
                _quote: &Token,
                _chain_id: ChainId,
            ) -> Result<U256, PricingError> {
                Ok(self.price)
            }
        }

        let mut agg = PriceAggregator::new(AggregatorConfig {
            max_deviation_pct: 10,
            ..AggregatorConfig::default()
        });

        // Two reasonable prices and one extreme outlier.
        agg.add_source(Arc::new(FixedSource {
            name: "a",
            price: U256::from(1000u64),
        }));
        agg.add_source(Arc::new(FixedSource {
            name: "b",
            price: U256::from(1020u64),
        }));
        agg.add_source(Arc::new(FixedSource {
            name: "outlier",
            price: U256::from(5000u64), // 400% above median
        }));

        let base = Token::new(
            address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            ChainId::Ethereum,
            18,
            "WETH",
        );
        let quote_tok = Token::new(
            address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            ChainId::Ethereum,
            6,
            "USDC",
        );

        let result = agg.get_price(&base, &quote_tok, ChainId::Ethereum).await;
        assert!(result.is_ok());
        let price = result.unwrap();
        // Outlier (5000) should be filtered; only 1000 and 1020 remain.
        assert_eq!(price.num_sources, 2);
        assert_eq!(price.median_price, U256::from(1010u64)); // (1000+1020)/2
        assert!(!price.sources.contains(&"outlier".to_string()));
    }

    #[tokio::test]
    async fn aggregator_cache_hit() {
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

        struct CountingSource;

        #[async_trait]
        impl PriceSource for CountingSource {
            fn name(&self) -> &str {
                "counting"
            }
            async fn get_price(
                &self,
                _base: &Token,
                _quote: &Token,
                _chain_id: ChainId,
            ) -> Result<U256, PricingError> {
                CALL_COUNT.fetch_add(1, Ordering::SeqCst);
                Ok(U256::from(500u64))
            }
        }

        CALL_COUNT.store(0, Ordering::SeqCst);

        let mut agg = PriceAggregator::new(AggregatorConfig {
            cache_ttl_seconds: 60,
            ..AggregatorConfig::default()
        });
        agg.add_source(Arc::new(CountingSource));

        let base = Token::new(
            address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            ChainId::Ethereum,
            18,
            "WETH",
        );
        let quote_tok = Token::new(
            address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            ChainId::Ethereum,
            6,
            "USDC",
        );

        // First call → queries source.
        let _ = agg
            .get_price(&base, &quote_tok, ChainId::Ethereum)
            .await
            .unwrap();
        assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 1);

        // Second call → cached.
        let _ = agg
            .get_price(&base, &quote_tok, ChainId::Ethereum)
            .await
            .unwrap();
        assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 1); // still 1

        assert_eq!(agg.cache_size(), 1);
    }

    #[tokio::test]
    async fn aggregator_no_sources_fails() {
        let agg = PriceAggregator::new(AggregatorConfig::default());

        let base = Token::new(
            address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            ChainId::Ethereum,
            18,
            "WETH",
        );
        let quote_tok = Token::new(
            address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            ChainId::Ethereum,
            6,
            "USDC",
        );

        let result = agg.get_price(&base, &quote_tok, ChainId::Ethereum).await;
        assert!(result.is_err());
    }

    #[test]
    fn invalidate_cache() {
        let agg = PriceAggregator::new(AggregatorConfig::default());
        agg.cache.insert(
            CacheKey {
                base: address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                quote: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                chain_id: ChainId::Ethereum,
            },
            CachedPrice {
                price: AggregatedPrice {
                    median_price: U256::from(100u64),
                    mean_price: U256::from(100u64),
                    num_sources: 1,
                    confidence: 50,
                    timestamp: Utc::now(),
                    sources: vec!["test".to_string()],
                },
                cached_at: Utc::now(),
            },
        );
        assert_eq!(agg.cache_size(), 1);
        agg.invalidate_cache();
        assert_eq!(agg.cache_size(), 0);
    }
}
