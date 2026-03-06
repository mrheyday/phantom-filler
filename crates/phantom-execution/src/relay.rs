//! MEV relay integration for private transaction and bundle submission.
//!
//! Supports Flashbots-style relays that accept signed bundles via JSON-RPC.
//! Protects fill transactions from frontrunning and sandwich attacks.

use std::sync::Arc;

use alloy::primitives::{Bytes, B256};
use async_trait::async_trait;
use phantom_common::error::{ExecutionError, ExecutionResult};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Configuration for an MEV relay endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayConfig {
    /// Relay endpoint URL (e.g., `https://relay.flashbots.net`).
    pub relay_url: String,
    /// Human-readable name for this relay.
    pub name: String,
    /// Request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Maximum number of retry attempts for failed submissions.
    pub max_retries: u32,
    /// Whether this relay is currently enabled.
    pub enabled: bool,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            relay_url: "https://relay.flashbots.net".into(),
            name: "flashbots".into(),
            timeout_ms: 5_000,
            max_retries: 2,
            enabled: true,
        }
    }
}

/// A bundle of signed transactions for relay submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleRequest {
    /// Signed raw transactions (RLP-encoded, hex-prefixed).
    pub transactions: Vec<Bytes>,
    /// Target block number for inclusion.
    pub target_block: u64,
    /// Minimum timestamp for bundle validity (optional).
    pub min_timestamp: Option<u64>,
    /// Maximum timestamp for bundle validity (optional).
    pub max_timestamp: Option<u64>,
}

impl BundleRequest {
    /// Creates a new bundle targeting the given block.
    pub fn new(transactions: Vec<Bytes>, target_block: u64) -> Self {
        Self {
            transactions,
            target_block,
            min_timestamp: None,
            max_timestamp: None,
        }
    }

    /// Sets the valid timestamp range for the bundle.
    pub fn with_timestamps(mut self, min: u64, max: u64) -> Self {
        self.min_timestamp = Some(min);
        self.max_timestamp = Some(max);
        self
    }

    /// Returns the number of transactions in the bundle.
    pub fn tx_count(&self) -> usize {
        self.transactions.len()
    }

    /// Returns true if the bundle is empty.
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }
}

/// Response from an MEV relay after bundle submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleResponse {
    /// Bundle hash returned by the relay.
    pub bundle_hash: B256,
}

/// Status of a submitted bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleStatus {
    /// Bundle was accepted by the relay.
    Pending,
    /// Bundle was included in a block.
    Included,
    /// Bundle was not included (block mined without it).
    Failed,
    /// Bundle status is unknown.
    Unknown,
}

/// Trait for interacting with MEV relays.
///
/// Implementations handle the specific JSON-RPC protocol of each relay
/// (Flashbots, MEV-Share, etc.).
#[async_trait]
pub trait MevRelay: Send + Sync {
    /// Returns the name of this relay.
    fn name(&self) -> &str;

    /// Returns the relay endpoint URL.
    fn relay_url(&self) -> &str;

    /// Submits a bundle of transactions to the relay.
    async fn send_bundle(&self, bundle: &BundleRequest) -> ExecutionResult<BundleResponse>;

    /// Submits a single private transaction targeting a specific block.
    async fn send_private_transaction(
        &self,
        raw_tx: &Bytes,
        max_block_number: u64,
    ) -> ExecutionResult<B256>;
}

/// JSON-RPC request structure for relay communication.
#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a, T: Serialize> {
    jsonrpc: &'a str,
    method: &'a str,
    params: Vec<T>,
    id: u64,
}

/// JSON-RPC response structure from relay.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    #[allow(dead_code)]
    jsonrpc: String,
    result: Option<T>,
    error: Option<JsonRpcError>,
    #[allow(dead_code)]
    id: u64,
}

/// JSON-RPC error from relay.
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

/// Flashbots bundle params for `eth_sendBundle`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FlashbotsBundleParams {
    txs: Vec<String>,
    block_number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_timestamp: Option<u64>,
}

/// Flashbots private tx params for `eth_sendPrivateTransaction`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FlashbotsPrivateTxParams {
    tx: String,
    max_block_number: String,
}

/// Response from `eth_sendBundle`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendBundleResult {
    bundle_hash: B256,
}

/// Flashbots relay client.
///
/// Submits bundles and private transactions to a Flashbots-compatible relay
/// endpoint via JSON-RPC over HTTP.
pub struct FlashbotsRelay {
    client: reqwest::Client,
    config: RelayConfig,
}

impl FlashbotsRelay {
    /// Creates a new Flashbots relay client.
    pub fn new(config: RelayConfig) -> ExecutionResult<Self> {
        let timeout = std::time::Duration::from_millis(config.timeout_ms);
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| {
                ExecutionError::RelayFailed(format!("failed to build HTTP client: {e}"))
            })?;

        info!(
            relay = %config.name,
            url = %config.relay_url,
            "initialized relay client"
        );

        Ok(Self { client, config })
    }

    /// Creates a relay client with default Flashbots mainnet configuration.
    pub fn with_defaults() -> ExecutionResult<Self> {
        Self::new(RelayConfig::default())
    }

    /// Returns a reference to the relay configuration.
    pub fn config(&self) -> &RelayConfig {
        &self.config
    }

    /// Sends a JSON-RPC request to the relay.
    async fn rpc_call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Vec<P>,
    ) -> ExecutionResult<R> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            method,
            params,
            id: 1,
        };

        let mut last_error = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                debug!(
                    relay = %self.config.name,
                    method,
                    attempt,
                    "retrying relay request"
                );
            }

            let result = self
                .client
                .post(&self.config.relay_url)
                .json(&request)
                .send()
                .await;

            match result {
                Ok(response) => {
                    let status = response.status();
                    if !status.is_success() {
                        let body = response.text().await.unwrap_or_default();
                        last_error = Some(ExecutionError::RelayFailed(format!(
                            "relay returned HTTP {status}: {body}"
                        )));
                        continue;
                    }

                    let rpc_response: JsonRpcResponse<R> = response.json().await.map_err(|e| {
                        ExecutionError::RelayFailed(format!("failed to parse relay response: {e}"))
                    })?;

                    if let Some(error) = rpc_response.error {
                        return Err(ExecutionError::RelayFailed(format!(
                            "relay RPC error: {}",
                            error.message
                        )));
                    }

                    return rpc_response.result.ok_or_else(|| {
                        ExecutionError::RelayFailed("relay returned null result".into())
                    });
                }
                Err(e) => {
                    warn!(
                        relay = %self.config.name,
                        method,
                        attempt,
                        error = %e,
                        "relay request failed"
                    );
                    last_error = Some(ExecutionError::RelayFailed(format!(
                        "relay request failed: {e}"
                    )));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ExecutionError::RelayFailed("relay request failed after retries".into())
        }))
    }
}

#[async_trait]
impl MevRelay for FlashbotsRelay {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn relay_url(&self) -> &str {
        &self.config.relay_url
    }

    async fn send_bundle(&self, bundle: &BundleRequest) -> ExecutionResult<BundleResponse> {
        if bundle.is_empty() {
            return Err(ExecutionError::RelayFailed(
                "cannot send empty bundle".into(),
            ));
        }

        let params = FlashbotsBundleParams {
            txs: bundle
                .transactions
                .iter()
                .map(|tx| format!("0x{}", hex::encode(tx)))
                .collect(),
            block_number: format!("0x{:x}", bundle.target_block),
            min_timestamp: bundle.min_timestamp,
            max_timestamp: bundle.max_timestamp,
        };

        debug!(
            relay = %self.config.name,
            tx_count = bundle.tx_count(),
            target_block = bundle.target_block,
            "sending bundle"
        );

        let result: SendBundleResult = self.rpc_call("eth_sendBundle", vec![params]).await?;

        info!(
            relay = %self.config.name,
            bundle_hash = %result.bundle_hash,
            target_block = bundle.target_block,
            "bundle submitted"
        );

        Ok(BundleResponse {
            bundle_hash: result.bundle_hash,
        })
    }

    async fn send_private_transaction(
        &self,
        raw_tx: &Bytes,
        max_block_number: u64,
    ) -> ExecutionResult<B256> {
        let params = FlashbotsPrivateTxParams {
            tx: format!("0x{}", hex::encode(raw_tx)),
            max_block_number: format!("0x{:x}", max_block_number),
        };

        debug!(
            relay = %self.config.name,
            max_block = max_block_number,
            "sending private transaction"
        );

        let tx_hash: B256 = self
            .rpc_call("eth_sendPrivateTransaction", vec![params])
            .await?;

        info!(
            relay = %self.config.name,
            tx_hash = %tx_hash,
            "private transaction submitted"
        );

        Ok(tx_hash)
    }
}

/// Manages multiple MEV relays and coordinates bundle submission.
///
/// Submits to all enabled relays in parallel for maximum inclusion probability.
pub struct RelayManager {
    relays: Vec<Arc<dyn MevRelay>>,
}

impl RelayManager {
    /// Creates a new relay manager with the given relays.
    pub fn new(relays: Vec<Arc<dyn MevRelay>>) -> Self {
        Self { relays }
    }

    /// Creates an empty relay manager.
    pub fn empty() -> Self {
        Self { relays: Vec::new() }
    }

    /// Adds a relay to the manager.
    pub fn add_relay(&mut self, relay: Arc<dyn MevRelay>) {
        info!(relay = %relay.name(), "added relay");
        self.relays.push(relay);
    }

    /// Returns the number of configured relays.
    pub fn relay_count(&self) -> usize {
        self.relays.len()
    }

    /// Returns true if no relays are configured.
    pub fn is_empty(&self) -> bool {
        self.relays.is_empty()
    }

    /// Lists the names of all configured relays.
    pub fn relay_names(&self) -> Vec<&str> {
        self.relays.iter().map(|r| r.name()).collect()
    }

    /// Sends a bundle to all configured relays in parallel.
    ///
    /// Returns results from each relay. Failures on individual relays
    /// do not prevent submission to others.
    pub async fn broadcast_bundle(
        &self,
        bundle: &BundleRequest,
    ) -> Vec<(&str, ExecutionResult<BundleResponse>)> {
        if self.relays.is_empty() {
            return Vec::new();
        }

        let mut handles = Vec::with_capacity(self.relays.len());

        for relay in &self.relays {
            let relay = Arc::clone(relay);
            let bundle = bundle.clone();
            handles.push(tokio::spawn(async move {
                let name = relay.name().to_string();
                let result = relay.send_bundle(&bundle).await;
                (name, result)
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok((name, result)) => {
                    // Find the relay name reference from our stored relays.
                    let relay_name = self
                        .relays
                        .iter()
                        .find(|r| r.name() == name)
                        .map(|r| r.name())
                        .unwrap_or("unknown");
                    results.push((relay_name, result));
                }
                Err(e) => {
                    warn!(error = %e, "relay task panicked");
                }
            }
        }

        results
    }

    /// Sends a private transaction to all configured relays in parallel.
    pub async fn broadcast_private_transaction(
        &self,
        raw_tx: &Bytes,
        max_block_number: u64,
    ) -> Vec<(&str, ExecutionResult<B256>)> {
        if self.relays.is_empty() {
            return Vec::new();
        }

        let mut handles = Vec::with_capacity(self.relays.len());

        for relay in &self.relays {
            let relay = Arc::clone(relay);
            let tx = raw_tx.clone();
            let max_block = max_block_number;
            handles.push(tokio::spawn(async move {
                let name = relay.name().to_string();
                let result = relay.send_private_transaction(&tx, max_block).await;
                (name, result)
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok((name, result)) => {
                    let relay_name = self
                        .relays
                        .iter()
                        .find(|r| r.name() == name)
                        .map(|r| r.name())
                        .unwrap_or("unknown");
                    results.push((relay_name, result));
                }
                Err(e) => {
                    warn!(error = %e, "relay task panicked");
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::b256;

    #[test]
    fn relay_config_default() {
        let config = RelayConfig::default();
        assert_eq!(config.relay_url, "https://relay.flashbots.net");
        assert_eq!(config.name, "flashbots");
        assert_eq!(config.timeout_ms, 5_000);
        assert_eq!(config.max_retries, 2);
        assert!(config.enabled);
    }

    #[test]
    fn relay_config_serde_roundtrip() {
        let config = RelayConfig {
            relay_url: "https://custom.relay.io".into(),
            name: "custom".into(),
            timeout_ms: 10_000,
            max_retries: 3,
            enabled: false,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: RelayConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.relay_url, "https://custom.relay.io");
        assert_eq!(deserialized.name, "custom");
        assert!(!deserialized.enabled);
    }

    #[test]
    fn bundle_request_new() {
        let txs = vec![Bytes::from(vec![0x01, 0x02]), Bytes::from(vec![0x03, 0x04])];
        let bundle = BundleRequest::new(txs, 19_000_000);

        assert_eq!(bundle.tx_count(), 2);
        assert!(!bundle.is_empty());
        assert_eq!(bundle.target_block, 19_000_000);
        assert!(bundle.min_timestamp.is_none());
        assert!(bundle.max_timestamp.is_none());
    }

    #[test]
    fn bundle_request_with_timestamps() {
        let bundle = BundleRequest::new(vec![Bytes::from(vec![0x01])], 19_000_000)
            .with_timestamps(1700000000, 1700000060);

        assert_eq!(bundle.min_timestamp, Some(1700000000));
        assert_eq!(bundle.max_timestamp, Some(1700000060));
    }

    #[test]
    fn bundle_request_empty() {
        let bundle = BundleRequest::new(vec![], 19_000_000);
        assert!(bundle.is_empty());
        assert_eq!(bundle.tx_count(), 0);
    }

    #[test]
    fn bundle_request_serde_roundtrip() {
        let bundle = BundleRequest::new(vec![Bytes::from(vec![0xde, 0xad])], 19_500_000)
            .with_timestamps(100, 200);

        let json = serde_json::to_string(&bundle).expect("serialize");
        let deserialized: BundleRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.target_block, 19_500_000);
        assert_eq!(deserialized.tx_count(), 1);
        assert_eq!(deserialized.min_timestamp, Some(100));
    }

    #[test]
    fn bundle_response_serde_roundtrip() {
        let response = BundleResponse {
            bundle_hash: b256!(
                "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ),
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let deserialized: BundleResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.bundle_hash, response.bundle_hash);
    }

    #[test]
    fn bundle_status_serde() {
        let statuses = [
            BundleStatus::Pending,
            BundleStatus::Included,
            BundleStatus::Failed,
            BundleStatus::Unknown,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).expect("serialize");
            let deserialized: BundleStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*status, deserialized);
        }
    }

    #[test]
    fn flashbots_relay_creation() {
        let relay = FlashbotsRelay::with_defaults().expect("create relay");
        assert_eq!(relay.name(), "flashbots");
        assert_eq!(relay.relay_url(), "https://relay.flashbots.net");
    }

    #[test]
    fn flashbots_relay_custom_config() {
        let config = RelayConfig {
            relay_url: "https://custom.relay.io".into(),
            name: "custom-relay".into(),
            timeout_ms: 3_000,
            max_retries: 1,
            enabled: true,
        };
        let relay = FlashbotsRelay::new(config).expect("create relay");
        assert_eq!(relay.name(), "custom-relay");
        assert_eq!(relay.config().timeout_ms, 3_000);
    }

    #[test]
    fn relay_manager_empty() {
        let manager = RelayManager::empty();
        assert!(manager.is_empty());
        assert_eq!(manager.relay_count(), 0);
        assert!(manager.relay_names().is_empty());
    }

    #[test]
    fn relay_manager_add_relay() {
        let relay = Arc::new(FlashbotsRelay::with_defaults().expect("create"));
        let mut manager = RelayManager::empty();
        manager.add_relay(relay);

        assert_eq!(manager.relay_count(), 1);
        assert!(!manager.is_empty());
        assert_eq!(manager.relay_names(), vec!["flashbots"]);
    }

    #[test]
    fn relay_manager_multiple_relays() {
        let relay1 = Arc::new(FlashbotsRelay::with_defaults().expect("create"));
        let config2 = RelayConfig {
            relay_url: "https://relay2.io".into(),
            name: "relay-2".into(),
            ..RelayConfig::default()
        };
        let relay2 = Arc::new(FlashbotsRelay::new(config2).expect("create"));

        let manager = RelayManager::new(vec![relay1, relay2]);
        assert_eq!(manager.relay_count(), 2);
        assert!(manager.relay_names().contains(&"flashbots"));
        assert!(manager.relay_names().contains(&"relay-2"));
    }

    #[tokio::test]
    async fn send_empty_bundle_rejected() {
        let relay = FlashbotsRelay::with_defaults().expect("create");
        let bundle = BundleRequest::new(vec![], 19_000_000);

        let result = relay.send_bundle(&bundle).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty bundle"));
    }

    #[tokio::test]
    async fn broadcast_to_empty_manager() {
        let manager = RelayManager::empty();
        let bundle = BundleRequest::new(vec![Bytes::from(vec![0x01])], 19_000_000);

        let results = manager.broadcast_bundle(&bundle).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn broadcast_private_tx_to_empty_manager() {
        let manager = RelayManager::empty();
        let tx = Bytes::from(vec![0x01, 0x02]);

        let results = manager.broadcast_private_transaction(&tx, 19_000_000).await;
        assert!(results.is_empty());
    }

    #[test]
    fn flashbots_bundle_params_serialization() {
        let params = FlashbotsBundleParams {
            txs: vec!["0xdead".into(), "0xbeef".into()],
            block_number: format!("0x{:x}", 19_000_000u64),
            min_timestamp: Some(1700000000),
            max_timestamp: None,
        };

        let json = serde_json::to_string(&params).expect("serialize");
        assert!(json.contains("\"txs\""));
        assert!(json.contains("\"blockNumber\""));
        assert!(json.contains("\"minTimestamp\""));
        // max_timestamp should be skipped (None).
        assert!(!json.contains("maxTimestamp"));
    }

    #[test]
    fn flashbots_private_tx_params_serialization() {
        let params = FlashbotsPrivateTxParams {
            tx: "0xdeadbeef".into(),
            max_block_number: format!("0x{:x}", 19_000_100u64),
        };

        let json = serde_json::to_string(&params).expect("serialize");
        assert!(json.contains("\"tx\""));
        assert!(json.contains("\"maxBlockNumber\""));
        assert!(json.contains("0xdeadbeef"));
    }
}
