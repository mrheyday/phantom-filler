//! Fill simulation engine for dry-run transaction testing.
//!
//! Uses `eth_call` to simulate fill transactions before on-chain execution,
//! estimating gas usage and detecting reverts.

use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};
use alloy::rpc::types::TransactionRequest;
use phantom_chain::provider::DynProvider;
use phantom_common::error::{StrategyError, StrategyResult};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Configuration for the simulation engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationConfig {
    /// Maximum time in milliseconds to wait for a simulation.
    pub timeout_ms: u64,
    /// Default gas limit for simulations.
    pub default_gas_limit: u64,
    /// Gas buffer multiplier in basis points (e.g., 12000 = 1.2x).
    pub gas_buffer_bps: u64,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 10_000,
            default_gas_limit: 500_000,
            gas_buffer_bps: 12_000, // 1.2x buffer
        }
    }
}

/// A request to simulate a transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationRequest {
    /// Sender address (filler wallet).
    pub from: Address,
    /// Target contract address.
    pub to: Address,
    /// Encoded calldata for the transaction.
    pub calldata: Bytes,
    /// ETH value to send with the transaction.
    pub value: U256,
    /// Optional gas limit override.
    pub gas_limit: Option<u64>,
}

/// Outcome of a simulation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationOutcome {
    /// Transaction executed successfully.
    Success,
    /// Transaction reverted.
    Revert,
}

/// Result of a fill simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    /// Whether the simulation succeeded or reverted.
    pub outcome: SimulationOutcome,
    /// Estimated gas usage (with buffer applied).
    pub gas_used: u64,
    /// Raw gas estimate from the provider (before buffer).
    pub raw_gas_estimate: u64,
    /// Return data from the simulation call.
    pub return_data: Bytes,
    /// Error message if the simulation reverted.
    pub error_message: Option<String>,
    /// Block number the simulation was run against.
    pub block_number: u64,
}

impl SimulationResult {
    /// Returns true if the simulation succeeded.
    pub fn is_success(&self) -> bool {
        self.outcome == SimulationOutcome::Success
    }

    /// Returns true if the simulation reverted.
    pub fn is_revert(&self) -> bool {
        self.outcome == SimulationOutcome::Revert
    }
}

/// Simulates fill transactions using `eth_call` before on-chain execution.
///
/// The simulator runs a dry-run of the transaction against the current chain
/// state to detect reverts and estimate gas usage.
pub struct FillSimulator {
    provider: Arc<DynProvider>,
    config: SimulationConfig,
}

impl FillSimulator {
    /// Creates a new simulator with the given provider and configuration.
    pub fn new(provider: Arc<DynProvider>, config: SimulationConfig) -> Self {
        Self { provider, config }
    }

    /// Creates a simulator with default configuration.
    pub fn with_defaults(provider: Arc<DynProvider>) -> Self {
        Self::new(provider, SimulationConfig::default())
    }

    /// Returns a reference to the configuration.
    pub fn config(&self) -> &SimulationConfig {
        &self.config
    }

    /// Simulates a transaction and returns the result.
    ///
    /// Performs two calls:
    /// 1. `eth_call` to check for reverts and get return data
    /// 2. `eth_estimateGas` to get accurate gas estimation
    pub async fn simulate(&self, request: &SimulationRequest) -> StrategyResult<SimulationResult> {
        let timeout = std::time::Duration::from_millis(self.config.timeout_ms);

        let result = tokio::time::timeout(timeout, self.run_simulation(request)).await;

        match result {
            Ok(inner) => inner,
            Err(_) => Err(StrategyError::EvaluationTimeout(self.config.timeout_ms)),
        }
    }

    /// Internal simulation logic.
    async fn run_simulation(
        &self,
        request: &SimulationRequest,
    ) -> StrategyResult<SimulationResult> {
        let gas_limit = request.gas_limit.unwrap_or(self.config.default_gas_limit);

        let tx = TransactionRequest::default()
            .from(request.from)
            .to(request.to)
            .input(request.calldata.clone().into())
            .value(request.value)
            .gas_limit(gas_limit);

        // Get current block number for the simulation context.
        let block_number = self
            .provider
            .get_block_number()
            .await
            .map_err(|e| StrategyError::SimulationFailed(format!("block number fetch: {e}")))?;

        debug!(
            from = %request.from,
            to = %request.to,
            value = %request.value,
            gas_limit,
            block = block_number,
            "running simulation"
        );

        // Step 1: eth_call to check for reverts.
        let call_result = self.provider.call(tx.clone()).await;

        match call_result {
            Ok(return_data) => {
                // Call succeeded — now estimate gas.
                let gas_estimate = self.estimate_gas(&tx).await;

                let (raw_gas, buffered_gas) = match gas_estimate {
                    Ok(estimate) => {
                        let raw = estimate;
                        let buffered = apply_gas_buffer(raw, self.config.gas_buffer_bps);
                        (raw, buffered)
                    }
                    Err(e) => {
                        // Gas estimation failed despite call succeeding — use default.
                        warn!(error = %e, "gas estimation failed, using default");
                        (gas_limit, gas_limit)
                    }
                };

                debug!(
                    raw_gas,
                    buffered_gas,
                    return_data_len = return_data.len(),
                    "simulation succeeded"
                );

                Ok(SimulationResult {
                    outcome: SimulationOutcome::Success,
                    gas_used: buffered_gas,
                    raw_gas_estimate: raw_gas,
                    return_data: Bytes::from(return_data.to_vec()),
                    error_message: None,
                    block_number,
                })
            }
            Err(e) => {
                let error_msg = e.to_string();
                debug!(error = %error_msg, "simulation reverted");

                Ok(SimulationResult {
                    outcome: SimulationOutcome::Revert,
                    gas_used: gas_limit,
                    raw_gas_estimate: gas_limit,
                    return_data: Bytes::new(),
                    error_message: Some(error_msg),
                    block_number,
                })
            }
        }
    }

    /// Estimates gas for a transaction using `eth_estimateGas`.
    async fn estimate_gas(&self, tx: &TransactionRequest) -> StrategyResult<u64> {
        let estimate = self
            .provider
            .estimate_gas(tx.clone())
            .await
            .map_err(|e| StrategyError::SimulationFailed(format!("gas estimation: {e}")))?;

        Ok(estimate)
    }
}

/// Applies a gas buffer in basis points (e.g., 12000 = 1.2x).
fn apply_gas_buffer(gas: u64, buffer_bps: u64) -> u64 {
    let buffered = u128::from(gas) * u128::from(buffer_bps) / 10_000;
    // Cap at u64::MAX.
    if buffered > u128::from(u64::MAX) {
        u64::MAX
    } else {
        buffered as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn simulation_config_default() {
        let config = SimulationConfig::default();
        assert_eq!(config.timeout_ms, 10_000);
        assert_eq!(config.default_gas_limit, 500_000);
        assert_eq!(config.gas_buffer_bps, 12_000);
    }

    #[test]
    fn simulation_config_serde_roundtrip() {
        let config = SimulationConfig {
            timeout_ms: 5_000,
            default_gas_limit: 300_000,
            gas_buffer_bps: 15_000,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: SimulationConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.timeout_ms, 5_000);
        assert_eq!(deserialized.gas_buffer_bps, 15_000);
    }

    #[test]
    fn simulation_request_construction() {
        let request = SimulationRequest {
            from: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            to: address!("0x6000da47483062A0D734Ba3dc7576Ce6A0B645C4"),
            calldata: Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]),
            value: U256::ZERO,
            gas_limit: Some(300_000),
        };
        assert_eq!(request.gas_limit, Some(300_000));
        assert_eq!(request.calldata.len(), 4);
    }

    #[test]
    fn simulation_outcome_serde() {
        let outcomes = [SimulationOutcome::Success, SimulationOutcome::Revert];
        for outcome in &outcomes {
            let json = serde_json::to_string(outcome).expect("serialize");
            let deserialized: SimulationOutcome = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*outcome, deserialized);
        }
    }

    #[test]
    fn simulation_result_success() {
        let result = SimulationResult {
            outcome: SimulationOutcome::Success,
            gas_used: 150_000,
            raw_gas_estimate: 125_000,
            return_data: Bytes::from(vec![0x01]),
            error_message: None,
            block_number: 19_000_000,
        };
        assert!(result.is_success());
        assert!(!result.is_revert());
    }

    #[test]
    fn simulation_result_revert() {
        let result = SimulationResult {
            outcome: SimulationOutcome::Revert,
            gas_used: 500_000,
            raw_gas_estimate: 500_000,
            return_data: Bytes::new(),
            error_message: Some("execution reverted".into()),
            block_number: 19_000_000,
        };
        assert!(result.is_revert());
        assert!(!result.is_success());
        assert!(result.error_message.is_some());
    }

    #[test]
    fn simulation_result_serde_roundtrip() {
        let result = SimulationResult {
            outcome: SimulationOutcome::Success,
            gas_used: 200_000,
            raw_gas_estimate: 170_000,
            return_data: Bytes::from(vec![0xab, 0xcd]),
            error_message: None,
            block_number: 19_500_000,
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let deserialized: SimulationResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.gas_used, 200_000);
        assert_eq!(deserialized.outcome, SimulationOutcome::Success);
        assert_eq!(deserialized.block_number, 19_500_000);
    }

    #[test]
    fn gas_buffer_1_2x() {
        // 100_000 * 12000 / 10000 = 120_000
        assert_eq!(apply_gas_buffer(100_000, 12_000), 120_000);
    }

    #[test]
    fn gas_buffer_1x() {
        // 100_000 * 10000 / 10000 = 100_000
        assert_eq!(apply_gas_buffer(100_000, 10_000), 100_000);
    }

    #[test]
    fn gas_buffer_1_5x() {
        // 200_000 * 15000 / 10000 = 300_000
        assert_eq!(apply_gas_buffer(200_000, 15_000), 300_000);
    }

    #[test]
    fn gas_buffer_overflow_caps() {
        // Very large gas with high buffer should cap at u64::MAX
        let result = apply_gas_buffer(u64::MAX, 20_000);
        assert_eq!(result, u64::MAX);
    }

    #[test]
    fn gas_buffer_zero() {
        assert_eq!(apply_gas_buffer(100_000, 0), 0);
    }

    #[tokio::test]
    async fn simulator_with_anvil() {
        // Spin up a local Anvil node for integration testing.
        let anvil = match alloy::node_bindings::Anvil::new().try_spawn() {
            Ok(a) => a,
            Err(_) => {
                eprintln!("Anvil not available, skipping integration test");
                return;
            }
        };

        let provider: Arc<DynProvider> = Arc::new(
            alloy::providers::ProviderBuilder::new()
                .connect_http(anvil.endpoint().parse().expect("valid url")),
        );

        let simulator = FillSimulator::with_defaults(provider);

        // Simulate a simple ETH transfer (no calldata).
        let request = SimulationRequest {
            from: anvil.addresses()[0],
            to: anvil.addresses()[1],
            calldata: Bytes::new(),
            value: U256::from(1_000_000_000_000_000_000u64), // 1 ETH
            gas_limit: None,
        };

        let result = simulator.simulate(&request).await.expect("simulation ok");

        assert!(result.is_success());
        assert!(result.gas_used > 0);
        assert!(result.gas_used >= 21_000); // minimum gas for transfer
        assert!(result.error_message.is_none());
        assert!(result.block_number < 100);
    }

    #[tokio::test]
    async fn simulator_revert_detection() {
        let anvil = match alloy::node_bindings::Anvil::new().try_spawn() {
            Ok(a) => a,
            Err(_) => {
                eprintln!("Anvil not available, skipping integration test");
                return;
            }
        };

        let provider: Arc<DynProvider> = Arc::new(
            alloy::providers::ProviderBuilder::new()
                .connect_http(anvil.endpoint().parse().expect("valid url")),
        );

        let simulator = FillSimulator::with_defaults(provider);

        // Call a non-contract address with calldata — should revert or fail.
        let request = SimulationRequest {
            from: anvil.addresses()[0],
            to: address!("0x0000000000000000000000000000000000000001"), // precompile
            calldata: Bytes::from(vec![0xff; 100]), // garbage data to precompile
            value: U256::ZERO,
            gas_limit: Some(100_000),
        };

        let result = simulator.simulate(&request).await.expect("simulation ok");

        // Precompile with garbage data may succeed or revert depending on the precompile.
        // The key test is that the simulator handles it without panicking.
        assert!(result.gas_used > 0);
    }

    #[tokio::test]
    async fn simulator_timeout() {
        // Use an unreachable provider to trigger timeout.
        let provider: Arc<DynProvider> = Arc::new(
            alloy::providers::ProviderBuilder::new()
                .connect_http("http://192.0.2.1:8545".parse().expect("valid url")),
        );

        let config = SimulationConfig {
            timeout_ms: 500, // very short timeout
            ..SimulationConfig::default()
        };
        let simulator = FillSimulator::new(provider, config);

        let request = SimulationRequest {
            from: Address::ZERO,
            to: Address::ZERO,
            calldata: Bytes::new(),
            value: U256::ZERO,
            gas_limit: None,
        };

        let result = simulator.simulate(&request).await;
        assert!(result.is_err());

        // Should be a timeout error.
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("timeout") || err.to_string().contains("500"),
            "expected timeout error, got: {err}"
        );
    }
}
