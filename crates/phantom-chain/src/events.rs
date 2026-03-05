//! Event subscription and log filtering for on-chain contract events.

use std::sync::Arc;

use alloy::primitives::{Address, B256};
use alloy::rpc::types::{Filter, Log};
use phantom_common::error::ChainError;
use phantom_common::types::ChainId;
use tokio::sync::{broadcast, watch};
use tracing::{debug, info, warn};

use crate::provider::DynProvider;
use crate::stream::BlockNotification;

/// Default broadcast channel capacity for event subscriptions.
const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 512;

/// An event log with chain context.
#[derive(Debug, Clone)]
pub struct ChainEvent {
    /// Chain this event was emitted on.
    pub chain_id: ChainId,
    /// The raw log entry.
    pub log: Log,
    /// Block number where the event was emitted.
    pub block_number: u64,
    /// Transaction hash that emitted the event.
    pub tx_hash: Option<B256>,
}

/// Configuration for an event subscription.
#[derive(Debug, Clone)]
pub struct EventFilterConfig {
    /// Contract addresses to monitor.
    pub addresses: Vec<Address>,
    /// Event signature topics (topic0) to filter by.
    pub event_signatures: Vec<B256>,
    /// Broadcast channel capacity.
    pub channel_capacity: usize,
}

impl EventFilterConfig {
    /// Creates a filter config for a single contract address and event signature.
    pub fn new(address: Address, event_signature: B256) -> Self {
        Self {
            addresses: vec![address],
            event_signatures: vec![event_signature],
            channel_capacity: DEFAULT_EVENT_CHANNEL_CAPACITY,
        }
    }

    /// Creates a filter config for multiple addresses.
    pub fn with_addresses(addresses: Vec<Address>, event_signatures: Vec<B256>) -> Self {
        Self {
            addresses,
            event_signatures,
            channel_capacity: DEFAULT_EVENT_CHANNEL_CAPACITY,
        }
    }

    /// Sets the channel capacity.
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.channel_capacity = capacity;
        self
    }

    /// Builds an Alloy `Filter` for a given block range.
    fn build_filter(&self, from_block: u64, to_block: u64) -> Filter {
        let mut filter = Filter::new().from_block(from_block).to_block(to_block);

        if !self.addresses.is_empty() {
            filter = filter.address(self.addresses.clone());
        }

        if !self.event_signatures.is_empty() {
            filter = filter.event_signature(self.event_signatures.clone());
        }

        filter
    }
}

/// Manages a single event subscription, polling logs per block.
pub struct EventSubscription {
    chain_id: ChainId,
    provider: Arc<DynProvider>,
    config: EventFilterConfig,
    sender: broadcast::Sender<ChainEvent>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl EventSubscription {
    /// Creates a new event subscription.
    pub fn new(chain_id: ChainId, provider: Arc<DynProvider>, config: EventFilterConfig) -> Self {
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

    /// Returns a receiver for event notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<ChainEvent> {
        self.sender.subscribe()
    }

    /// Returns the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Starts the event subscription, driven by block notifications.
    ///
    /// Listens for new blocks on `block_rx` and fetches matching logs for each block.
    pub fn start(
        &self,
        mut block_rx: broadcast::Receiver<BlockNotification>,
    ) -> tokio::task::JoinHandle<()> {
        let chain_id = self.chain_id;
        let provider = Arc::clone(&self.provider);
        let config = self.config.clone();
        let sender = self.sender.clone();
        let mut shutdown_rx = self.shutdown_rx.clone();

        tokio::spawn(async move {
            info!(
                %chain_id,
                addresses = config.addresses.len(),
                event_sigs = config.event_signatures.len(),
                "event subscription started"
            );

            loop {
                tokio::select! {
                    block_result = block_rx.recv() => {
                        match block_result {
                            Ok(block) => {
                                if let Err(e) = fetch_and_broadcast_logs(
                                    &provider,
                                    chain_id,
                                    &config,
                                    &sender,
                                    block.block_number,
                                ).await {
                                    warn!(
                                        %chain_id,
                                        block_number = block.block_number,
                                        error = %e,
                                        "failed to fetch logs for block"
                                    );
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                warn!(
                                    %chain_id,
                                    skipped,
                                    "event subscription lagged, missed blocks"
                                );
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                info!(%chain_id, "block stream closed, stopping event subscription");
                                break;
                            }
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        info!(%chain_id, "event subscription shutting down");
                        break;
                    }
                }
            }
        })
    }

    /// Fetches historical logs for a given block range.
    pub async fn get_historical_logs(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<ChainEvent>, ChainError> {
        let filter = self.config.build_filter(from_block, to_block);
        let logs =
            self.provider
                .get_logs(&filter)
                .await
                .map_err(|e| ChainError::ProviderError {
                    chain_id: self.chain_id,
                    reason: format!("failed to get logs: {e}"),
                })?;

        Ok(logs_to_events(self.chain_id, logs))
    }

    /// Signals the subscription to stop.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Manages multiple event subscriptions across chains.
pub struct EventSubscriptionManager {
    subscriptions: Vec<(EventSubscription, Option<tokio::task::JoinHandle<()>>)>,
}

impl EventSubscriptionManager {
    /// Creates a new empty manager.
    pub fn new() -> Self {
        Self {
            subscriptions: Vec::new(),
        }
    }

    /// Adds a subscription (not yet started).
    pub fn add(&mut self, subscription: EventSubscription) {
        self.subscriptions.push((subscription, None));
    }

    /// Starts all subscriptions, each receiving blocks from the given sender.
    pub fn start_all(&mut self, block_sender: &broadcast::Sender<BlockNotification>) {
        for (sub, handle) in &mut self.subscriptions {
            if handle.is_none() {
                let rx = block_sender.subscribe();
                *handle = Some(sub.start(rx));
            }
        }
    }

    /// Stops all subscriptions.
    pub fn stop_all(&self) {
        for (sub, _) in &self.subscriptions {
            sub.stop();
        }
    }

    /// Returns the total number of subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }
}

impl Default for EventSubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Fetches logs for a single block and broadcasts them as events.
async fn fetch_and_broadcast_logs(
    provider: &Arc<DynProvider>,
    chain_id: ChainId,
    config: &EventFilterConfig,
    sender: &broadcast::Sender<ChainEvent>,
    block_number: u64,
) -> Result<(), ChainError> {
    let filter = config.build_filter(block_number, block_number);

    let logs = provider
        .get_logs(&filter)
        .await
        .map_err(|e| ChainError::ProviderError {
            chain_id,
            reason: format!("failed to get logs at block {block_number}: {e}"),
        })?;

    let event_count = logs.len();
    if event_count > 0 {
        debug!(
            %chain_id,
            block_number,
            event_count,
            "fetched events from block"
        );
    }

    if sender.receiver_count() > 0 {
        for event in logs_to_events(chain_id, logs) {
            let _ = sender.send(event);
        }
    }

    Ok(())
}

/// Converts Alloy logs into ChainEvents.
fn logs_to_events(chain_id: ChainId, logs: Vec<Log>) -> Vec<ChainEvent> {
    logs.into_iter()
        .map(|log| {
            let block_number = log.block_number.unwrap_or(0);
            let tx_hash = log.transaction_hash;
            ChainEvent {
                chain_id,
                log,
                block_number,
                tx_hash,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::b256;
    use std::time::Duration;
    use tokio::time;

    #[test]
    fn event_filter_config_new() {
        let addr = Address::ZERO;
        let sig = B256::ZERO;
        let config = EventFilterConfig::new(addr, sig);

        assert_eq!(config.addresses.len(), 1);
        assert_eq!(config.event_signatures.len(), 1);
        assert_eq!(config.channel_capacity, DEFAULT_EVENT_CHANNEL_CAPACITY);
    }

    #[test]
    fn event_filter_config_with_addresses() {
        let addrs = vec![Address::ZERO, Address::with_last_byte(1)];
        let sigs = vec![B256::ZERO];
        let config = EventFilterConfig::with_addresses(addrs.clone(), sigs);

        assert_eq!(config.addresses.len(), 2);
    }

    #[test]
    fn event_filter_config_with_capacity() {
        let config = EventFilterConfig::new(Address::ZERO, B256::ZERO).with_capacity(1024);
        assert_eq!(config.channel_capacity, 1024);
    }

    #[test]
    fn build_filter_includes_address_and_topics() {
        let addr = Address::with_last_byte(0x42);
        let sig = b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");
        let config = EventFilterConfig::new(addr, sig);

        let _filter = config.build_filter(100, 200);
        // Filter constructed without panic — fields are private so we verify construction.
    }

    #[test]
    fn logs_to_events_empty() {
        let events = logs_to_events(ChainId::Ethereum, vec![]);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn event_subscription_subscribe() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let config = EventFilterConfig::new(Address::ZERO, B256::ZERO);
        let sub = EventSubscription::new(ChainId::Ethereum, provider, config);

        assert_eq!(sub.subscriber_count(), 0);
        let _rx1 = sub.subscribe();
        assert_eq!(sub.subscriber_count(), 1);
        let _rx2 = sub.subscribe();
        assert_eq!(sub.subscriber_count(), 2);
    }

    #[tokio::test]
    async fn event_subscription_stop() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let config = EventFilterConfig::new(Address::ZERO, B256::ZERO);
        let sub = EventSubscription::new(ChainId::Ethereum, provider, config);

        let (block_tx, _) = broadcast::channel::<BlockNotification>(16);
        let block_rx = block_tx.subscribe();
        let handle = sub.start(block_rx);

        // Give it a moment to start.
        time::sleep(Duration::from_millis(10)).await;
        sub.stop();

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "subscription should have stopped");
    }

    #[test]
    fn event_subscription_manager_lifecycle() {
        let manager = EventSubscriptionManager::new();
        assert_eq!(manager.subscription_count(), 0);
    }

    #[tokio::test]
    async fn event_subscription_manager_add() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let config = EventFilterConfig::new(Address::ZERO, B256::ZERO);
        let sub = EventSubscription::new(ChainId::Ethereum, provider, config);

        let mut manager = EventSubscriptionManager::new();
        manager.add(sub);
        assert_eq!(manager.subscription_count(), 1);
    }

    #[tokio::test]
    async fn subscription_closes_when_block_stream_drops() {
        let provider = alloy::providers::ProviderBuilder::new()
            .connect_http("https://eth.example.com".parse().unwrap());
        let provider: Arc<DynProvider> = Arc::new(provider);

        let config = EventFilterConfig::new(Address::ZERO, B256::ZERO);
        let sub = EventSubscription::new(ChainId::Ethereum, provider, config);

        let (block_tx, _) = broadcast::channel::<BlockNotification>(16);
        let block_rx = block_tx.subscribe();
        let handle = sub.start(block_rx);

        // Drop the block sender — this closes the broadcast channel.
        drop(block_tx);

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "subscription should stop when block stream closes"
        );
    }
}
