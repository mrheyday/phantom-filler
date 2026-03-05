//! Transaction builder for constructing EIP-1559 fill transactions.

use alloy::primitives::{Address, Bytes, U256};
use alloy::rpc::types::TransactionRequest;
use phantom_common::error::{ExecutionError, ExecutionResult};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Parameters for building a fill transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionParams {
    /// Sender (filler) address.
    pub from: Address,
    /// Target contract address (reactor).
    pub to: Address,
    /// Encoded calldata for the fill function.
    pub calldata: Bytes,
    /// ETH value to send (usually zero for token fills).
    pub value: U256,
    /// Chain ID for replay protection.
    pub chain_id: u64,
    /// EIP-1559 max fee per gas in wei.
    pub max_fee_per_gas: u128,
    /// EIP-1559 max priority fee per gas in wei.
    pub max_priority_fee_per_gas: u128,
    /// Gas limit for the transaction.
    pub gas_limit: u64,
    /// Transaction nonce.
    pub nonce: u64,
}

/// Configuration for the transaction builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderConfig {
    /// Default gas limit when not specified.
    pub default_gas_limit: u64,
    /// Maximum allowed gas limit.
    pub max_gas_limit: u64,
    /// Maximum allowed max_fee_per_gas in wei.
    pub max_fee_cap: u128,
}

impl Default for BuilderConfig {
    fn default() -> Self {
        Self {
            default_gas_limit: 300_000,
            max_gas_limit: 1_000_000,
            max_fee_cap: 500_000_000_000, // 500 gwei
        }
    }
}

/// Builds EIP-1559 fill transactions from parameters.
///
/// Validates gas limits and fee caps before constructing the
/// `TransactionRequest` used for signing and submission.
pub struct TransactionBuilder {
    config: BuilderConfig,
}

impl TransactionBuilder {
    /// Creates a new builder with the given configuration.
    pub fn new(config: BuilderConfig) -> Self {
        Self { config }
    }

    /// Creates a builder with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(BuilderConfig::default())
    }

    /// Returns a reference to the builder configuration.
    pub fn config(&self) -> &BuilderConfig {
        &self.config
    }

    /// Builds an EIP-1559 `TransactionRequest` from the given parameters.
    ///
    /// Validates gas limit and fee parameters against configured caps.
    pub fn build(&self, params: &TransactionParams) -> ExecutionResult<TransactionRequest> {
        self.validate(params)?;

        let tx = TransactionRequest::default()
            .from(params.from)
            .to(params.to)
            .input(params.calldata.clone().into())
            .value(params.value)
            .nonce(params.nonce)
            .gas_limit(params.gas_limit)
            .max_fee_per_gas(params.max_fee_per_gas)
            .max_priority_fee_per_gas(params.max_priority_fee_per_gas);

        debug!(
            from = %params.from,
            to = %params.to,
            nonce = params.nonce,
            gas_limit = params.gas_limit,
            max_fee = params.max_fee_per_gas,
            priority_fee = params.max_priority_fee_per_gas,
            "built EIP-1559 transaction"
        );

        Ok(tx)
    }

    /// Validates transaction parameters against configured limits.
    fn validate(&self, params: &TransactionParams) -> ExecutionResult<()> {
        if params.gas_limit > self.config.max_gas_limit {
            return Err(ExecutionError::GasEstimationFailed(format!(
                "gas limit {} exceeds maximum {}",
                params.gas_limit, self.config.max_gas_limit
            )));
        }

        if params.max_fee_per_gas > self.config.max_fee_cap {
            return Err(ExecutionError::GasEstimationFailed(format!(
                "max fee per gas {} exceeds cap {}",
                params.max_fee_per_gas, self.config.max_fee_cap
            )));
        }

        if params.max_priority_fee_per_gas > params.max_fee_per_gas {
            return Err(ExecutionError::GasEstimationFailed(
                "priority fee exceeds max fee per gas".into(),
            ));
        }

        if params.to == Address::ZERO {
            return Err(ExecutionError::GasEstimationFailed(
                "target address cannot be zero".into(),
            ));
        }

        Ok(())
    }
}

impl Default for TransactionBuilder {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn sample_params() -> TransactionParams {
        TransactionParams {
            from: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            to: address!("0x6000da47483062A0D734Ba3dc7576Ce6A0B645C4"),
            calldata: Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]),
            value: U256::ZERO,
            chain_id: 1,
            max_fee_per_gas: 50_000_000_000,         // 50 gwei
            max_priority_fee_per_gas: 2_000_000_000, // 2 gwei
            gas_limit: 200_000,
            nonce: 42,
        }
    }

    #[test]
    fn build_valid_transaction() {
        let builder = TransactionBuilder::with_defaults();
        let params = sample_params();
        let tx = builder.build(&params).expect("should build");

        assert_eq!(tx.from, Some(params.from));
        assert_eq!(tx.nonce, Some(params.nonce));
        assert_eq!(tx.gas, Some(params.gas_limit));
        assert_eq!(tx.max_fee_per_gas, Some(params.max_fee_per_gas));
        assert_eq!(
            tx.max_priority_fee_per_gas,
            Some(params.max_priority_fee_per_gas)
        );
    }

    #[test]
    fn gas_limit_exceeds_max() {
        let builder = TransactionBuilder::with_defaults();
        let mut params = sample_params();
        params.gas_limit = 2_000_000; // exceeds default max of 1M

        let result = builder.build(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("gas limit"));
    }

    #[test]
    fn max_fee_exceeds_cap() {
        let builder = TransactionBuilder::with_defaults();
        let mut params = sample_params();
        params.max_fee_per_gas = 1_000_000_000_000; // exceeds 500 gwei cap

        let result = builder.build(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max fee"));
    }

    #[test]
    fn priority_fee_exceeds_max_fee() {
        let builder = TransactionBuilder::with_defaults();
        let mut params = sample_params();
        params.max_priority_fee_per_gas = 100_000_000_000; // > max_fee
        params.max_fee_per_gas = 50_000_000_000;

        let result = builder.build(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("priority fee"));
    }

    #[test]
    fn zero_target_address_rejected() {
        let builder = TransactionBuilder::with_defaults();
        let mut params = sample_params();
        params.to = Address::ZERO;

        let result = builder.build(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("zero"));
    }

    #[test]
    fn custom_config() {
        let config = BuilderConfig {
            default_gas_limit: 500_000,
            max_gas_limit: 5_000_000,
            max_fee_cap: 1_000_000_000_000, // 1000 gwei
        };
        let builder = TransactionBuilder::new(config);
        let mut params = sample_params();
        params.gas_limit = 3_000_000; // allowed with higher max

        let result = builder.build(&params);
        assert!(result.is_ok());
    }

    #[test]
    fn transaction_params_serde_roundtrip() {
        let params = sample_params();
        let json = serde_json::to_string(&params).expect("serialize");
        let deserialized: TransactionParams = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.nonce, 42);
        assert_eq!(deserialized.chain_id, 1);
        assert_eq!(deserialized.gas_limit, 200_000);
    }

    #[test]
    fn builder_config_default() {
        let config = BuilderConfig::default();
        assert_eq!(config.default_gas_limit, 300_000);
        assert_eq!(config.max_gas_limit, 1_000_000);
        assert_eq!(config.max_fee_cap, 500_000_000_000);
    }

    #[test]
    fn builder_config_serde_roundtrip() {
        let config = BuilderConfig {
            default_gas_limit: 400_000,
            max_gas_limit: 2_000_000,
            max_fee_cap: 800_000_000_000,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: BuilderConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.max_gas_limit, 2_000_000);
    }

    #[test]
    fn default_builder() {
        let builder = TransactionBuilder::default();
        assert_eq!(builder.config().default_gas_limit, 300_000);
    }

    #[test]
    fn calldata_preserved() {
        let builder = TransactionBuilder::with_defaults();
        let params = sample_params();
        let tx = builder.build(&params).expect("should build");

        let input = tx.input.input().cloned().unwrap_or_default();
        assert_eq!(input, Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]));
    }

    #[test]
    fn value_preserved() {
        let builder = TransactionBuilder::with_defaults();
        let mut params = sample_params();
        params.value = U256::from(1_000_000_000_000_000_000u64); // 1 ETH

        let tx = builder.build(&params).expect("should build");
        assert_eq!(tx.value, Some(U256::from(1_000_000_000_000_000_000u64)));
    }
}
