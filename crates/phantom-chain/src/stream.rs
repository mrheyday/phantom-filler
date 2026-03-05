//! Block streaming service with polling-based new block detection.

use std::sync::Arc;
use std::time::Duration;

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::B256;
use phantom_common::error::ChainError;
use phantom_common::types::ChainId;
use tokio::sync::{broadcast, watch};
use tokio::time;
use tracing::{debug, info, warn};

use crate::provider::DynProvider;

/// Default polling interval for HTTP providers.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Default broadcast channel capacity.
const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Maximum consecutive errors before backoff increases.
const MAX_CONSECUTIVE_ERRORS: u32 = 5;

/// Maximum backoff duration.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Notification emitted when a new block is detected.
#[derive(Debug, Clone)]
pub struct BlockNotification {
    /// Chain this block belongs to.
    pub chain_id: ChainId,
    /// Block number.
    pub block_number: u64,
    /// Block hash.
    pub block_hash: B256,
    /// Block timestamp (seconds since UNIX epoch).
    pub timestamp: u64,
    /// Parent block hash.
    pub parent_hash: B256,
    /// Base fee per gas (if available, post-London).
    pub base_fee_per_gas: Option<u64>,
}

/// Configuration for a block streamer.
#[derive(Debug, Clone)]
pub struct BlockStreamerConfig {
    /// Polling interval for new blocks.
    pub poll_interval: Duration,
    /// Broadcast channel capacity.
    pub channel_capacity: usize,
}

impl Default for BlockStreamerConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

/// Streams new blocks from a single chain via polling.
pub struct BlockStreamer {
    chain_id: ChainId,
    provider: Arc<DynProvider>,
    config: BlockStreamerConfig,
    sender: broadcast::Sender<BlockNotification>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl BlockStreamer {
    /// Creates a new block streamer for the given chain.
    pub fn new(chain_id: ChainId, provider: Arc<DynProvider>, config: BlockStreamerConfig) -> Self {
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

    /// Creates a block streamer with default configuration.
    pub fn with_defaults(chain_id: ChainId, provider: Arc<DynProvider>) -> Self {
        Self::new(chain_id, provider, BlockStreamerConfig::default())
    }

    /// Returns a receiver for block notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<BlockNotification> {
        self.sender.subscribe()
    }

    /// Returns the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Starts the block streaming loop in a background task.
    ///
    /// Returns a handle to the spawned task.
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let chain_id = self.chain_id;
        let provider = Arc::clone(&self.provider);
        let sender = self.sender.clone();
        let poll_interval = self.config.poll_interval;
        let mut shutdown_rx = self.shutdown_rx.clone();

        tokio::spawn(async move {
            info!(%chain_id, ?poll_interval, "block streamer started");

            let mut last_block: Option<u64> = None;
            let mut consecutive_errors: u32 = 0;

            loop {
                // Check for shutdown signal.
                if *shutdown_rx.borrow() {
                    info!(%chain_id, "block streamer shutting down");
                    break;
                }

                match poll_latest_block(&provider, chain_id, &sender, &mut last_block).await {
                    Ok(()) => {
                        consecutive_errors = 0;
                    }
                    Err(e) => {
                        consecutive_errors = consecutive_errors.saturating_add(1);
                        let backoff = calculate_backoff(consecutive_errors, poll_interval);
                        warn!(
                            %chain_id,
                            error = %e,
                            consecutive_errors,
                            ?backoff,
                            "block polling error, backing off"
                        );
                        tokio::select! {
                            _ = time::sleep(backoff) => {}
                            _ = shutdown_rx.changed() => {
                                info!(%chain_id, "block streamer shutting down during backoff");
                                break;
                            }
                        }
                        continue;
                    }
                }

                // Wait for next poll or shutdown.
                tokio::select! {
                    _ = time::sleep(poll_interval) => {}
                    _ = shutdown_rx.changed() => {
                        info!(%chain_id, "block streamer shutting down");
                        break;
                    }
                }
            }
        })
    }

    /// Signals the streamer to stop.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Polls the provider for the latest block and broadcasts if new.
async fn poll_latest_block(
    provider: &Arc<DynProvider>,
    chain_id: ChainId,
    sender: &broadcast::Sender<BlockNotification>,
    last_block: &mut Option<u64>,
) -> Result<(), ChainError> {
    let current_number =
        provider
            .get_block_number()
            .await
            .map_err(|e| ChainError::ProviderError {
                chain_id,
                reason: format!("failed to get block number: {e}"),
            })?;

    // Skip if we've already seen this block.
    if let Some(last) = last_block {
        if current_number <= *last {
            return Ok(());
        }

        // If we missed blocks, log a warning.
        let missed = current_number - *last - 1;
        if missed > 0 {
            warn!(%chain_id, missed_blocks = missed, "detected missed blocks");
        }
    }

    // Fetch the full block header.
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Number(current_number))
        .await
        .map_err(|e| ChainError::ProviderError {
            chain_id,
            reason: format!("failed to get block {current_number}: {e}"),
        })?
        .ok_or_else(|| ChainError::ProviderError {
            chain_id,
            reason: format!("block {current_number} not found"),
        })?;

    let notification = BlockNotification {
        chain_id,
        block_number: current_number,
        block_hash: block.header.hash,
        timestamp: block.header.inner.timestamp,
        parent_hash: block.header.inner.parent_hash,
        base_fee_per_gas: block.header.inner.base_fee_per_gas,
    };

    debug!(
        %chain_id,
        block_number = current_number,
        block_hash = %notification.block_hash,
        "new block detected"
    );

    // Only send if there are subscribers.
    if sender.receiver_count() > 0 {
        let _ = sender.send(notification);
    }

    *last_block = Some(current_number);
    Ok(())
}

/// Calculates exponential backoff duration.
fn calculate_backoff(consecutive_errors: u32, base_interval: Duration) -> Duration {
    let multiplier = 2u64.saturating_pow(consecutive_errors.min(MAX_CONSECUTIVE_ERRORS));
    let backoff = base_interval.saturating_mul(multiplier as u32);
    backoff.min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = BlockStreamerConfig::default();
        assert_eq!(config.poll_interval, Duration::from_millis(1000));
        assert_eq!(config.channel_capacity, 256);
    }

    #[test]
    fn block_notification_clone() {
        let notification = BlockNotification {
            chain_id: ChainId::Ethereum,
            block_number: 12345,
            block_hash: B256::ZERO,
            timestamp: 1_700_000_000,
            parent_hash: B256::ZERO,
            base_fee_per_gas: Some(30_000_000_000),
        };
        let cloned = notification.clone();
        assert_eq!(cloned.block_number, 12345);
        assert_eq!(cloned.chain_id, ChainId::Ethereum);
        assert_eq!(cloned.base_fee_per_gas, Some(30_000_000_000));
    }

    #[test]
    fn backoff_calculation() {
        let base = Duration::from_millis(1000);

        // First error: 2x
        assert_eq!(calculate_backoff(1, base), Duration::from_millis(2000));
        // Second error: 4x
        assert_eq!(calculate_backoff(2, base), Duration::from_millis(4000));
        // Third error: 8x
        assert_eq!(calculate_backoff(3, base), Duration::from_millis(8000));
        // Capped at MAX_BACKOFF
        assert_eq!(calculate_backoff(10, base), MAX_BACKOFF);
    }

    #[test]
    fn backoff_zero_errors() {
        let base = Duration::from_millis(1000);
        // Zero errors: 1x (2^0 = 1)
        assert_eq!(calculate_backoff(0, base), Duration::from_millis(1000));
    }

    #[tokio::test]
    async fn streamer_subscribe() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let streamer = BlockStreamer::with_defaults(ChainId::Ethereum, provider);

        assert_eq!(streamer.subscriber_count(), 0);
        let _rx1 = streamer.subscribe();
        assert_eq!(streamer.subscriber_count(), 1);
        let _rx2 = streamer.subscribe();
        assert_eq!(streamer.subscriber_count(), 2);
    }

    #[tokio::test]
    async fn streamer_stop() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let streamer = BlockStreamer::new(
            ChainId::Ethereum,
            provider,
            BlockStreamerConfig {
                poll_interval: Duration::from_millis(50),
                channel_capacity: 16,
            },
        );

        let handle = streamer.start();
        // Give the task a moment to start.
        time::sleep(Duration::from_millis(10)).await;
        streamer.stop();

        // The task should finish promptly after stop signal.
        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "streamer task should have stopped");
    }
}
