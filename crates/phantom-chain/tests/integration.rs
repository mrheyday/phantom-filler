//! Integration tests for phantom-chain using Anvil local testnet.
//!
//! These tests require `anvil` to be installed (foundry toolchain).
//! They spawn a local Anvil instance for each test.

use std::sync::Arc;
use std::time::Duration;

use alloy::node_bindings::Anvil;
use alloy::primitives::{Address, B256};
use alloy::providers::ProviderBuilder;
use phantom_chain::events::{EventFilterConfig, EventSubscription};
use phantom_chain::mempool::{MempoolConfig, MempoolMonitor};
use phantom_chain::provider::{DynProvider, ProviderManager};
use phantom_chain::stream::{BlockStreamer, BlockStreamerConfig};
use phantom_common::config::ChainConfig;
use phantom_common::types::ChainId;

/// Helper: creates a ChainConfig pointing to the given Anvil instance.
fn anvil_config(chain_id: ChainId, endpoint: &str) -> ChainConfig {
    ChainConfig {
        chain_id,
        rpc_url: endpoint.to_string(),
        ws_url: None,
        max_concurrent_requests: 64,
        request_timeout_ms: 5000,
        mempool_enabled: true,
    }
}

// ─── ProviderManager tests ───────────────────────────────────────────

#[tokio::test]
async fn provider_manager_health_check_against_anvil() {
    let anvil = Anvil::new().block_time(1).spawn();
    let endpoint = anvil.endpoint();

    let manager = ProviderManager::new();
    manager
        .add_chain(anvil_config(ChainId::Ethereum, &endpoint))
        .await
        .expect("add chain");

    let block_number = manager
        .check_health(ChainId::Ethereum)
        .await
        .expect("health check");

    // Anvil starts at block 0.
    assert!(
        block_number <= 2,
        "expected low block number from fresh anvil"
    );
}

#[tokio::test]
async fn provider_manager_multiple_chains_same_anvil() {
    let anvil = Anvil::new().spawn();
    let endpoint = anvil.endpoint();

    let configs = vec![
        (
            "chain_a".to_string(),
            anvil_config(ChainId::Ethereum, &endpoint),
        ),
        (
            "chain_b".to_string(),
            anvil_config(ChainId::Arbitrum, &endpoint),
        ),
    ];

    let manager = ProviderManager::from_configs(configs)
        .await
        .expect("from configs");

    assert_eq!(manager.chain_count(), 2);
    assert!(manager.get_provider(ChainId::Ethereum).is_ok());
    assert!(manager.get_provider(ChainId::Arbitrum).is_ok());
}

#[tokio::test]
async fn provider_manager_health_check_all() {
    let anvil = Anvil::new().spawn();
    let endpoint = anvil.endpoint();

    let manager = ProviderManager::new();
    manager
        .add_chain(anvil_config(ChainId::Ethereum, &endpoint))
        .await
        .expect("add chain");

    let results = manager.check_all_health().await;
    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_ok());
}

// ─── BlockStreamer tests ─────────────────────────────────────────────

#[tokio::test]
async fn block_streamer_detects_new_blocks() {
    // Anvil with 1-second block time so we get new blocks automatically.
    let anvil = Anvil::new().block_time(1).spawn();
    let endpoint = anvil.endpoint();

    let provider: Arc<DynProvider> =
        Arc::new(ProviderBuilder::new().connect_http(endpoint.parse().unwrap()));

    let config = BlockStreamerConfig {
        poll_interval: Duration::from_millis(200),
        channel_capacity: 64,
    };
    let streamer = BlockStreamer::new(ChainId::Ethereum, provider, config);

    let mut rx = streamer.subscribe();
    let _handle = streamer.start();

    // Wait for at least one block notification.
    let notification = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("should receive block within timeout")
        .expect("channel should not be closed");

    assert_eq!(notification.chain_id, ChainId::Ethereum);
    // Anvil with block_time=1 starts mining, first notification may be block 0 or 1+.
    assert!(
        notification.block_number < 100,
        "block number should be reasonable"
    );
    assert_ne!(notification.block_hash, B256::ZERO);
    assert!(notification.timestamp > 0);

    streamer.stop();
}

#[tokio::test]
async fn block_streamer_multiple_blocks() {
    let anvil = Anvil::new().block_time(1).spawn();
    let endpoint = anvil.endpoint();

    let provider: Arc<DynProvider> =
        Arc::new(ProviderBuilder::new().connect_http(endpoint.parse().unwrap()));

    let config = BlockStreamerConfig {
        poll_interval: Duration::from_millis(200),
        channel_capacity: 64,
    };
    let streamer = BlockStreamer::new(ChainId::Ethereum, provider, config);

    let mut rx = streamer.subscribe();
    let _handle = streamer.start();

    // Collect at least 2 blocks.
    let mut blocks = Vec::new();
    for _ in 0..2 {
        let notification = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("should receive block")
            .expect("channel open");
        blocks.push(notification);
    }

    // Blocks should be in ascending order.
    assert!(blocks[1].block_number > blocks[0].block_number);

    // Second block's parent_hash should be the first block's hash.
    if blocks[1].block_number == blocks[0].block_number + 1 {
        assert_eq!(blocks[1].parent_hash, blocks[0].block_hash);
    }

    streamer.stop();
}

// ─── EventSubscription tests ─────────────────────────────────────────

#[tokio::test]
async fn event_subscription_historical_logs_empty() {
    let anvil = Anvil::new().spawn();
    let endpoint = anvil.endpoint();

    let provider: Arc<DynProvider> =
        Arc::new(ProviderBuilder::new().connect_http(endpoint.parse().unwrap()));

    // Filter for events from address 0x42 — fresh Anvil has no such logs.
    let config = EventFilterConfig::new(Address::with_last_byte(0x42), B256::ZERO);
    let sub = EventSubscription::new(ChainId::Ethereum, provider, config);

    let logs = sub.get_historical_logs(0, 0).await.expect("get logs");

    assert!(logs.is_empty(), "fresh anvil should have no matching logs");
}

#[tokio::test]
async fn event_subscription_responds_to_block_stream() {
    let anvil = Anvil::new().block_time(1).spawn();
    let endpoint = anvil.endpoint();

    let provider: Arc<DynProvider> =
        Arc::new(ProviderBuilder::new().connect_http(endpoint.parse().unwrap()));

    // Set up block streamer.
    let block_config = BlockStreamerConfig {
        poll_interval: Duration::from_millis(200),
        channel_capacity: 64,
    };
    let streamer = BlockStreamer::new(ChainId::Ethereum, Arc::clone(&provider), block_config);

    // Set up event subscription (watching for any events — won't match but should not panic).
    let event_config = EventFilterConfig::new(Address::ZERO, B256::ZERO);
    let event_sub = EventSubscription::new(ChainId::Ethereum, provider, event_config);

    let block_rx = streamer.subscribe();
    let _event_handle = event_sub.start(block_rx);
    let _block_handle = streamer.start();

    // Let it run for a couple of blocks.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Verify it didn't panic and is still running.
    // Subscription is still alive — no panics occurred.
    let _ = event_sub.subscriber_count();

    event_sub.stop();
    streamer.stop();
}

// ─── MempoolMonitor tests ────────────────────────────────────────────

#[tokio::test]
async fn mempool_monitor_starts_and_stops() {
    let anvil = Anvil::new().spawn();
    let endpoint = anvil.endpoint();

    let provider: Arc<DynProvider> =
        Arc::new(ProviderBuilder::new().connect_http(endpoint.parse().unwrap()));

    let config = MempoolConfig {
        poll_interval: Duration::from_millis(200),
        ..Default::default()
    };
    let monitor = MempoolMonitor::new(ChainId::Ethereum, provider, config);

    let _rx = monitor.subscribe();
    let handle = monitor.start();

    // Let it poll a few times.
    tokio::time::sleep(Duration::from_millis(500)).await;
    monitor.stop();

    let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(result.is_ok(), "monitor should stop cleanly");
}

// ─── Cross-component tests ──────────────────────────────────────────

#[tokio::test]
async fn full_chain_connector_lifecycle() {
    let anvil = Anvil::new().block_time(1).spawn();
    let endpoint = anvil.endpoint();

    // 1. Create provider manager and add chain.
    let manager = ProviderManager::new();
    manager
        .add_chain(anvil_config(ChainId::Ethereum, &endpoint))
        .await
        .expect("add chain");

    // 2. Health check.
    let block = manager
        .check_health(ChainId::Ethereum)
        .await
        .expect("health");
    assert!(block <= 2);

    // 3. Get provider and start block streaming.
    let provider = manager
        .get_provider(ChainId::Ethereum)
        .expect("get provider");
    let streamer = BlockStreamer::new(
        ChainId::Ethereum,
        Arc::clone(&provider),
        BlockStreamerConfig {
            poll_interval: Duration::from_millis(200),
            channel_capacity: 64,
        },
    );
    let mut block_rx = streamer.subscribe();
    let _stream_handle = streamer.start();

    // 4. Start event subscription.
    let event_sub = EventSubscription::new(
        ChainId::Ethereum,
        Arc::clone(&provider),
        EventFilterConfig::new(Address::ZERO, B256::ZERO),
    );
    let event_block_rx = streamer.subscribe();
    let _event_handle = event_sub.start(event_block_rx);

    // 5. Wait for a block.
    let notification = tokio::time::timeout(Duration::from_secs(5), block_rx.recv())
        .await
        .expect("block timeout")
        .expect("channel open");

    assert_eq!(notification.chain_id, ChainId::Ethereum);
    // Anvil with block_time=1 starts mining, first notification may be block 0 or 1+.
    assert!(
        notification.block_number < 100,
        "block number should be reasonable"
    );

    // 6. Clean shutdown.
    event_sub.stop();
    streamer.stop();
    manager.remove_chain(ChainId::Ethereum);
    assert_eq!(manager.chain_count(), 0);
}
