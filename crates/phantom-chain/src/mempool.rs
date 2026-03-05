//! Mempool monitoring for pending transaction detection and classification.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use alloy::consensus::Transaction;
use alloy::network::TransactionResponse;
use alloy::primitives::{Address, B256};
use phantom_common::error::ChainError;
use phantom_common::types::ChainId;
use tokio::sync::{broadcast, watch};
use tracing::{debug, info, warn};

use crate::provider::DynProvider;

/// Default polling interval for pending transactions.
const DEFAULT_MEMPOOL_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Default broadcast channel capacity for pending transactions.
const DEFAULT_MEMPOOL_CHANNEL_CAPACITY: usize = 1024;

/// Maximum number of recently-seen tx hashes to track (dedup window).
const MAX_SEEN_CAPACITY: usize = 10_000;

/// A pending transaction detected in the mempool.
#[derive(Debug, Clone)]
pub struct PendingTransaction {
    /// Chain this transaction is on.
    pub chain_id: ChainId,
    /// Transaction hash.
    pub tx_hash: B256,
    /// Sender address (if known).
    pub from: Option<Address>,
    /// Recipient address (if known).
    pub to: Option<Address>,
    /// Value transferred in wei (if known).
    pub value: Option<alloy::primitives::U256>,
    /// Input data (calldata), if available.
    pub input: Option<alloy::primitives::Bytes>,
}

/// Configuration for mempool monitoring.
#[derive(Debug, Clone)]
pub struct MempoolConfig {
    /// Polling interval for new pending transactions.
    pub poll_interval: Duration,
    /// Broadcast channel capacity.
    pub channel_capacity: usize,
    /// Optional filter: only track transactions to these addresses.
    pub watched_addresses: Vec<Address>,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_MEMPOOL_POLL_INTERVAL,
            channel_capacity: DEFAULT_MEMPOOL_CHANNEL_CAPACITY,
            watched_addresses: Vec::new(),
        }
    }
}

impl MempoolConfig {
    /// Creates a config that watches specific contract addresses.
    pub fn watching(addresses: Vec<Address>) -> Self {
        Self {
            watched_addresses: addresses,
            ..Default::default()
        }
    }
}

/// Monitors a chain's mempool for pending transactions.
pub struct MempoolMonitor {
    chain_id: ChainId,
    provider: Arc<DynProvider>,
    config: MempoolConfig,
    sender: broadcast::Sender<PendingTransaction>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl MempoolMonitor {
    /// Creates a new mempool monitor.
    pub fn new(chain_id: ChainId, provider: Arc<DynProvider>, config: MempoolConfig) -> Self {
        let (sender, _) = broadcast::channel(config.channel_capacity);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Self {
            chain_id,
            provider,
            config,
            sender,
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Creates a mempool monitor with default configuration.
    pub fn with_defaults(chain_id: ChainId, provider: Arc<DynProvider>) -> Self {
        Self::new(chain_id, provider, MempoolConfig::default())
    }

    /// Returns a receiver for pending transaction notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<PendingTransaction> {
        self.sender.subscribe()
    }

    /// Returns the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Starts the mempool monitoring loop.
    ///
    /// Uses `eth_newPendingTransactionFilter` + `eth_getFilterChanges` for
    /// polling-based pending transaction detection.
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let chain_id = self.chain_id;
        let provider = Arc::clone(&self.provider);
        let sender = self.sender.clone();
        let poll_interval = self.config.poll_interval;
        let watched_addresses = self.config.watched_addresses.clone();
        let mut shutdown_rx = self.shutdown_rx.clone();

        tokio::spawn(async move {
            info!(%chain_id, ?poll_interval, "mempool monitor started");

            // Track recently-seen tx hashes to avoid duplicate notifications.
            let mut seen_hashes: HashSet<B256> = HashSet::new();

            loop {
                if *shutdown_rx.borrow() {
                    info!(%chain_id, "mempool monitor shutting down");
                    break;
                }

                match poll_pending_transactions(
                    &provider,
                    chain_id,
                    &sender,
                    &mut seen_hashes,
                    &watched_addresses,
                )
                .await
                {
                    Ok(count) => {
                        if count > 0 {
                            debug!(%chain_id, new_txs = count, "detected pending transactions");
                        }
                    }
                    Err(e) => {
                        warn!(%chain_id, error = %e, "mempool poll error");
                    }
                }

                // Prune seen hashes if they grow too large.
                if seen_hashes.len() > MAX_SEEN_CAPACITY {
                    seen_hashes.clear();
                    debug!(%chain_id, "cleared seen tx hash cache");
                }

                tokio::select! {
                    _ = tokio::time::sleep(poll_interval) => {}
                    _ = shutdown_rx.changed() => {
                        info!(%chain_id, "mempool monitor shutting down");
                        break;
                    }
                }
            }
        })
    }

    /// Signals the monitor to stop.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Polls for pending transaction hashes via `eth_getBlockByNumber("pending")`.
///
/// For providers that support it, fetches the pending block to discover
/// pending transaction hashes. Falls back gracefully if unsupported.
async fn poll_pending_transactions(
    provider: &Arc<DynProvider>,
    chain_id: ChainId,
    sender: &broadcast::Sender<PendingTransaction>,
    seen_hashes: &mut HashSet<B256>,
    watched_addresses: &[Address],
) -> Result<usize, ChainError> {
    // Use eth_pendingTransactions via the raw client to get pending tx hashes.
    // Most HTTP providers support eth_getBlockByNumber("pending") which gives us
    // transaction hashes in the pending block.
    let pending_block = provider
        .get_block_by_number(alloy::eips::BlockNumberOrTag::Pending)
        .await
        .map_err(|e| ChainError::ProviderError {
            chain_id,
            reason: format!("failed to get pending block: {e}"),
        })?;

    let block = match pending_block {
        Some(b) => b,
        None => return Ok(0),
    };

    let tx_hashes: Vec<B256> = block.transactions.hashes().collect();

    let mut new_count = 0;
    for tx_hash in tx_hashes {
        if !seen_hashes.insert(tx_hash) {
            continue; // Already seen.
        }

        // Optionally fetch full transaction for address filtering.
        if !watched_addresses.is_empty() {
            if let Ok(Some(tx)) = provider.get_transaction_by_hash(tx_hash).await {
                let to_addr = tx.to();

                // Filter: only emit if `to` matches a watched address.
                let matches = to_addr.is_some_and(|to| watched_addresses.contains(&to));
                if !matches {
                    continue;
                }

                if sender.receiver_count() > 0 {
                    let _ = sender.send(PendingTransaction {
                        chain_id,
                        tx_hash,
                        from: Some(tx.from()),
                        to: tx.to(),
                        value: Some(tx.value()),
                        input: Some(tx.input().clone()),
                    });
                }
            }
        } else if sender.receiver_count() > 0 {
            // No address filter — emit hash-only notification.
            let _ = sender.send(PendingTransaction {
                chain_id,
                tx_hash,
                from: None,
                to: None,
                value: None,
                input: None,
            });
        }

        new_count += 1;
    }

    Ok(new_count)
}

/// Checks if a transaction's calldata matches a known function selector.
pub fn matches_selector(input: &[u8], selector: &[u8; 4]) -> bool {
    input.len() >= 4 && input[..4] == selector[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = MempoolConfig::default();
        assert_eq!(config.poll_interval, Duration::from_millis(500));
        assert_eq!(config.channel_capacity, DEFAULT_MEMPOOL_CHANNEL_CAPACITY);
        assert!(config.watched_addresses.is_empty());
    }

    #[test]
    fn config_watching() {
        let addr = Address::with_last_byte(0x42);
        let config = MempoolConfig::watching(vec![addr]);
        assert_eq!(config.watched_addresses.len(), 1);
        assert_eq!(config.watched_addresses[0], addr);
    }

    #[test]
    fn pending_tx_clone() {
        let tx = PendingTransaction {
            chain_id: ChainId::Ethereum,
            tx_hash: B256::ZERO,
            from: Some(Address::ZERO),
            to: Some(Address::with_last_byte(1)),
            value: Some(alloy::primitives::U256::from(1000)),
            input: None,
        };
        let cloned = tx.clone();
        assert_eq!(cloned.chain_id, ChainId::Ethereum);
        assert_eq!(cloned.tx_hash, B256::ZERO);
    }

    #[test]
    fn matches_selector_valid() {
        let selector: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb]; // transfer(address,uint256)
        let input = vec![0xa9, 0x05, 0x9c, 0xbb, 0x00, 0x01, 0x02, 0x03];
        assert!(matches_selector(&input, &selector));
    }

    #[test]
    fn matches_selector_mismatch() {
        let selector: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
        let input = vec![0x00, 0x00, 0x00, 0x00];
        assert!(!matches_selector(&input, &selector));
    }

    #[test]
    fn matches_selector_too_short() {
        let selector: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
        let input = vec![0xa9, 0x05];
        assert!(!matches_selector(&input, &selector));
    }

    #[tokio::test]
    async fn monitor_subscribe() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let monitor = MempoolMonitor::with_defaults(ChainId::Ethereum, provider);

        assert_eq!(monitor.subscriber_count(), 0);
        let _rx = monitor.subscribe();
        assert_eq!(monitor.subscriber_count(), 1);
    }

    #[tokio::test]
    async fn monitor_stop() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let monitor = MempoolMonitor::new(
            ChainId::Ethereum,
            provider,
            MempoolConfig {
                poll_interval: Duration::from_millis(50),
                ..Default::default()
            },
        );

        let handle = monitor.start();
        tokio::time::sleep(Duration::from_millis(10)).await;
        monitor.stop();

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "monitor should have stopped");
    }
}
