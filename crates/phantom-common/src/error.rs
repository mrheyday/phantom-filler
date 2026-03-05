//! Domain error types for the Phantom Filler engine.

use crate::types::ChainId;

/// Top-level error type encompassing all domain errors.
#[derive(Debug, thiserror::Error)]
pub enum PhantomError {
    /// Chain connector errors.
    #[error("chain error: {0}")]
    Chain(#[from] ChainError),

    /// Intent discovery errors.
    #[error("discovery error: {0}")]
    Discovery(#[from] DiscoveryError),

    /// Pricing engine errors.
    #[error("pricing error: {0}")]
    Pricing(#[from] PricingError),

    /// Strategy engine errors.
    #[error("strategy error: {0}")]
    Strategy(#[from] StrategyError),

    /// Execution engine errors.
    #[error("execution error: {0}")]
    Execution(#[from] ExecutionError),

    /// Configuration errors.
    #[error("config error: {0}")]
    Config(String),

    /// Database errors.
    #[error("database error: {0}")]
    Database(String),

    /// Generic internal error.
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

/// Errors from the chain connector layer.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    /// Failed to connect to an RPC provider.
    #[error("RPC connection failed for {chain_id}: {reason}")]
    ConnectionFailed { chain_id: ChainId, reason: String },

    /// Provider returned an error.
    #[error("provider error on {chain_id}: {reason}")]
    ProviderError { chain_id: ChainId, reason: String },

    /// Block stream was interrupted.
    #[error("block stream interrupted on {chain_id}")]
    StreamInterrupted { chain_id: ChainId },

    /// Requested chain is not configured.
    #[error("unsupported chain: {0}")]
    UnsupportedChain(ChainId),

    /// Transaction submission failed.
    #[error("transaction failed on {chain_id}: {reason}")]
    TransactionFailed { chain_id: ChainId, reason: String },
}

/// Errors from the intent discovery service.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// Failed to decode an order from event logs.
    #[error("order decode failed: {0}")]
    DecodeFailed(String),

    /// ABI decoding failed.
    #[error("decoding failed: {reason}")]
    DecodingFailed { reason: String },

    /// Order validation failed.
    #[error("validation failed: {reason}")]
    ValidationFailed { reason: String },

    /// Order signature is invalid.
    #[error("invalid order signature: {0}")]
    InvalidSignature(String),

    /// Order has expired.
    #[error("order expired: {0}")]
    OrderExpired(String),

    /// Order was not found in the order book.
    #[error("order not found: {0}")]
    OrderNotFound(String),

    /// Reactor contract interaction failed.
    #[error("reactor error: {0}")]
    ReactorError(String),

    /// Order already exists in the order book.
    #[error("order already exists: {0}")]
    OrderAlreadyExists(String),

    /// Invalid status transition attempted.
    #[error("invalid transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
}

/// Errors from the pricing engine.
#[derive(Debug, thiserror::Error)]
pub enum PricingError {
    /// No price available for the requested pair.
    #[error("no price available for {token_a}/{token_b} on {chain_id}")]
    NoPriceAvailable {
        token_a: String,
        token_b: String,
        chain_id: ChainId,
    },

    /// Price source returned stale data.
    #[error("stale price data from {provider}: age {age_seconds}s")]
    StalePrice { provider: String, age_seconds: u64 },

    /// Price source is unavailable.
    #[error("price source unavailable: {0}")]
    SourceUnavailable(String),

    /// Gas estimation failed.
    #[error("gas estimation failed: {0}")]
    GasEstimationFailed(String),
}

/// Errors from the strategy engine.
#[derive(Debug, thiserror::Error)]
pub enum StrategyError {
    /// Fill simulation failed.
    #[error("simulation failed: {0}")]
    SimulationFailed(String),

    /// Order is not profitable to fill.
    #[error("unprofitable order: expected loss {0}")]
    Unprofitable(String),

    /// Insufficient liquidity to fill the order.
    #[error("insufficient liquidity: {0}")]
    InsufficientLiquidity(String),

    /// Strategy evaluation timed out.
    #[error("evaluation timeout after {0}ms")]
    EvaluationTimeout(u64),
}

/// Errors from the execution engine.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    /// Transaction was reverted on-chain.
    #[error("transaction reverted: {0}")]
    Reverted(String),

    /// Nonce conflict or management error.
    #[error("nonce error: {0}")]
    NonceError(String),

    /// Gas estimation failed.
    #[error("gas estimation failed: {0}")]
    GasEstimationFailed(String),

    /// Transaction was not included in time.
    #[error("transaction not included after {attempts} attempts")]
    NotIncluded { attempts: u32 },

    /// Relay (Flashbots/MEV-Share) submission failed.
    #[error("relay submission failed: {0}")]
    RelayFailed(String),
}

/// Convenience result alias using the top-level error type.
pub type PhantomResult<T> = std::result::Result<T, PhantomError>;

/// Result alias for chain operations.
pub type ChainResult<T> = std::result::Result<T, ChainError>;

/// Result alias for discovery operations.
pub type DiscoveryResult<T> = std::result::Result<T, DiscoveryError>;

/// Result alias for pricing operations.
pub type PricingResult<T> = std::result::Result<T, PricingError>;

/// Result alias for strategy operations.
pub type StrategyResult<T> = std::result::Result<T, StrategyError>;

/// Result alias for execution operations.
pub type ExecutionResult<T> = std::result::Result<T, ExecutionError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phantom_error_from_chain_error() {
        let err = ChainError::UnsupportedChain(ChainId::Ethereum);
        let phantom: PhantomError = err.into();
        assert!(matches!(phantom, PhantomError::Chain(_)));
        assert!(phantom.to_string().contains("Ethereum"));
    }

    #[test]
    fn phantom_error_from_discovery_error() {
        let err = DiscoveryError::OrderNotFound("abc123".into());
        let phantom: PhantomError = err.into();
        assert!(matches!(phantom, PhantomError::Discovery(_)));
        assert!(phantom.to_string().contains("abc123"));
    }

    #[test]
    fn phantom_error_from_pricing_error() {
        let err = PricingError::NoPriceAvailable {
            token_a: "WETH".into(),
            token_b: "USDC".into(),
            chain_id: ChainId::Arbitrum,
        };
        let phantom: PhantomError = err.into();
        assert!(matches!(phantom, PhantomError::Pricing(_)));
        assert!(phantom.to_string().contains("WETH"));
    }

    #[test]
    fn phantom_error_from_strategy_error() {
        let err = StrategyError::EvaluationTimeout(500);
        let phantom: PhantomError = err.into();
        assert!(matches!(phantom, PhantomError::Strategy(_)));
        assert!(phantom.to_string().contains("500"));
    }

    #[test]
    fn phantom_error_from_execution_error() {
        let err = ExecutionError::NotIncluded { attempts: 3 };
        let phantom: PhantomError = err.into();
        assert!(matches!(phantom, PhantomError::Execution(_)));
        assert!(phantom.to_string().contains("3"));
    }

    #[test]
    fn chain_error_display() {
        let err = ChainError::ConnectionFailed {
            chain_id: ChainId::Base,
            reason: "timeout".into(),
        };
        let display = err.to_string();
        assert!(display.contains("Base"));
        assert!(display.contains("timeout"));
    }

    #[test]
    fn execution_error_display() {
        let err = ExecutionError::RelayFailed("bundle rejected".into());
        assert!(err.to_string().contains("bundle rejected"));
    }

    #[test]
    fn result_aliases_work() {
        fn chain_op() -> ChainResult<u64> {
            Ok(42)
        }
        fn phantom_op() -> PhantomResult<u64> {
            Ok(chain_op()?)
        }
        assert_eq!(phantom_op().unwrap(), 42);
    }
}
