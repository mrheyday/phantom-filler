//! Integration tests for the order lifecycle pipeline.
//!
//! Tests cross-crate interactions: discovery → strategy → risk → settlement.

use alloy::primitives::{address, Address, Bytes, B256, U256};
use chrono::{Duration, Utc};
use phantom_common::types::{
    ChainId, DutchAuctionOrder, OrderId, OrderInput, OrderOutput, OrderStatus,
};
use phantom_discovery::orderbook::OrderBook;
use phantom_execution::builder::{TransactionBuilder, TransactionParams};
use phantom_execution::nonce::NonceManager;
use phantom_execution::wallet::WalletManager;
use phantom_inventory::pnl::{FillRecord, FillStatus, PnlTracker};
use phantom_inventory::risk::{RiskCheckOutcome, RiskManager};
use phantom_settlement::confirmation::{ConfirmationMonitor, TxStatus};
use phantom_strategy::registry::StrategyRegistry;

// ─── Order Discovery → OrderBook ────────────────────────────────────

#[test]
fn order_lifecycle_through_orderbook() {
    let book = OrderBook::new();

    // Create a sample Dutch auction order.
    let order = make_sample_order([0x01; 32]);
    let order_id = order.id;

    // Insert: starts in Pending status.
    book.insert(order).expect("insert should succeed");
    let entry = book.get(&order_id).expect("should find order");
    assert_eq!(entry.status, OrderStatus::Pending);

    // Activate: moves to Active.
    book.activate(&order_id).expect("activate should succeed");
    let entry = book.get(&order_id).expect("should find order");
    assert_eq!(entry.status, OrderStatus::Active);
    assert_eq!(book.active_count(), 1);

    // Mark as filled.
    book.mark_filled(&order_id)
        .expect("mark_filled should succeed");
    let entry = book.get(&order_id).expect("should find order");
    assert_eq!(entry.status, OrderStatus::Filled);
    assert_eq!(book.active_count(), 0);
}

#[test]
fn orderbook_rejects_duplicate_orders() {
    let book = OrderBook::new();
    let order = make_sample_order([0x02; 32]);

    book.insert(order.clone()).expect("first insert");
    let result = book.insert(order);
    assert!(result.is_err(), "duplicate insert should fail");
}

#[test]
fn orderbook_handles_expired_orders() {
    let book = OrderBook::new();
    let order = make_sample_order([0x03; 32]);
    let order_id = order.id;

    book.insert(order).unwrap();
    book.activate(&order_id).unwrap();
    book.mark_expired(&order_id).unwrap();

    let entry = book.get(&order_id).unwrap();
    assert_eq!(entry.status, OrderStatus::Expired);
    assert_eq!(book.get_by_status(OrderStatus::Expired).len(), 1);
}

// ─── Strategy Registry ──────────────────────────────────────────────

#[test]
fn strategy_registry_management() {
    let registry = StrategyRegistry::new();

    assert_eq!(registry.active_strategies().len(), 0);
    assert!(registry.list().is_empty());
}

// ─── Risk Manager → Fill Check ──────────────────────────────────────

#[test]
fn risk_manager_approves_valid_fill() {
    let risk = RiskManager::with_defaults();

    // Small fill, no existing position.
    let result = risk.check_fill(U256::from(1_000_000_000u64), U256::ZERO);
    assert_eq!(result.outcome, RiskCheckOutcome::Passed);
    assert!(result.is_passed());
}

#[test]
fn risk_manager_rejects_oversized_fill() {
    let risk = RiskManager::with_defaults();

    // Fill exceeds max single fill value (default 10 ETH).
    let huge_value = U256::from(100_000_000_000_000_000_000u128); // 100 ETH
    let result = risk.check_fill(huge_value, U256::ZERO);
    assert_eq!(result.outcome, RiskCheckOutcome::Rejected);
    assert!(result.is_rejected());
}

#[test]
fn risk_manager_rejects_exceeding_position_limit() {
    let risk = RiskManager::with_defaults();

    // Current position near limit; fill pushes over.
    let current = U256::from(999_000_000_000_000_000_000u128); // 999 tokens
    let fill = U256::from(2_000_000_000_000_000_000u128); // 2 tokens
    let result = risk.check_fill(fill, current);
    assert_eq!(result.outcome, RiskCheckOutcome::Rejected);
}

// ─── PnL Tracker ────────────────────────────────────────────────────

#[test]
fn pnl_records_fill_and_updates_summary() {
    let tracker = PnlTracker::with_defaults();

    let record = FillRecord {
        fill_id: "fill-001".into(),
        chain_id: 1,
        token_in: address!("0000000000000000000000000000000000000001"),
        token_out: address!("0000000000000000000000000000000000000002"),
        amount_in: U256::from(1_000_000_000_000_000_000u64),
        amount_out: U256::from(2_000_000_000u64),
        gas_cost_wei: 50_000_000_000_000u128,
        pnl_wei: 100_000_000_000_000i128,
        tx_hash: B256::ZERO,
        timestamp: 1700000000,
        status: FillStatus::Confirmed,
    };

    tracker.record_fill(record);

    assert_eq!(tracker.fill_count(), 1);
    let summary = tracker.summary();
    assert_eq!(summary.total_fills, 1);
    assert!(summary.total_realized_pnl_wei > 0);
}

#[test]
fn pnl_tracks_multiple_fills_across_chains() {
    let tracker = PnlTracker::with_defaults();

    for i in 0..5 {
        let record = FillRecord {
            fill_id: format!("fill-{i:03}"),
            chain_id: if i % 2 == 0 { 1 } else { 42161 },
            token_in: address!("0000000000000000000000000000000000000001"),
            token_out: address!("0000000000000000000000000000000000000002"),
            amount_in: U256::from(1_000_000_000_000_000_000u64),
            amount_out: U256::from(2_000_000_000u64),
            gas_cost_wei: 50_000_000_000_000u128,
            pnl_wei: 100_000_000_000_000i128,
            tx_hash: B256::from([i as u8; 32]),
            timestamp: 1700000000 + (i as u64 * 100),
            status: FillStatus::Confirmed,
        };
        tracker.record_fill(record);
    }

    assert_eq!(tracker.fill_count(), 5);
    let daily = tracker.all_daily_pnl();
    assert!(!daily.is_empty());
    let tokens = tracker.all_token_pnl();
    assert!(!tokens.is_empty());
}

// ─── Settlement Confirmation ────────────────────────────────────────

#[test]
fn confirmation_monitor_tracks_transaction() {
    let monitor = ConfirmationMonitor::with_defaults();

    let tx_hash = B256::from([0xAA; 32]);
    let from = address!("0000000000000000000000000000000000000042");

    monitor
        .track_transaction(tx_hash, 1, from)
        .expect("should track");

    let record = monitor.get_record(&tx_hash).expect("should have record");
    assert_eq!(record.status, TxStatus::Pending);
    assert_eq!(record.chain_id, 1);
    assert_eq!(record.from, from);
    assert!(record.is_pending());
    assert!(!record.is_final());
}

#[test]
fn confirmation_monitor_respects_capacity() {
    let monitor = ConfirmationMonitor::new(phantom_settlement::confirmation::ConfirmationConfig {
        required_confirmations: 2,
        timeout_ms: 120_000,
        max_tracked: 2,
    });

    let from = Address::ZERO;
    monitor
        .track_transaction(B256::from([1; 32]), 1, from)
        .unwrap();
    monitor
        .track_transaction(B256::from([2; 32]), 1, from)
        .unwrap();

    // Third should fail — capacity reached.
    let result = monitor.track_transaction(B256::from([3; 32]), 1, from);
    assert!(result.is_err());
}

// ─── Execution Builder ──────────────────────────────────────────────

#[test]
fn transaction_builder_creates_valid_tx() {
    let builder = TransactionBuilder::with_defaults();

    let params = TransactionParams {
        from: address!("0000000000000000000000000000000000000001"),
        to: address!("0000000000000000000000000000000000000002"),
        calldata: Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]),
        value: U256::ZERO,
        chain_id: 1,
        max_fee_per_gas: 30_000_000_000u128,
        max_priority_fee_per_gas: 1_000_000_000u128,
        gas_limit: 200_000,
        nonce: 0,
    };

    let tx = builder.build(&params).expect("should build tx");
    // TransactionRequest should be constructable without error.
    assert!(tx.to.is_some());
}

// ─── Wallet Manager ─────────────────────────────────────────────────

#[test]
fn wallet_manager_import_and_list() {
    let manager = WalletManager::with_defaults();

    // Import a test private key (not a real key, just for testing).
    let test_key = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    let addr = manager
        .import_wallet(test_key, "test-wallet")
        .expect("should import");

    let wallets = manager.list_wallets();
    assert_eq!(wallets.len(), 1);
    assert_eq!(wallets[0].address, addr);
    assert_eq!(wallets[0].label, "test-wallet");
}

// ─── Nonce Manager ──────────────────────────────────────────────────

#[test]
fn nonce_manager_construction() {
    let manager = NonceManager::with_defaults();
    assert_eq!(manager.config().max_pending_per_address, 16);
}

// ─── Cross-crate: Risk + PnL ────────────────────────────────────────

#[test]
fn risk_check_then_pnl_recording_flow() {
    let risk = RiskManager::with_defaults();
    let pnl = PnlTracker::with_defaults();

    // Step 1: Check risk before filling.
    let fill_value = U256::from(1_000_000_000_000_000_000u64); // 1 ETH
    let check = risk.check_fill(fill_value, U256::ZERO);
    assert!(check.is_passed());

    // Step 2: Record the fill start.
    risk.record_fill_start().expect("should record fill start");
    assert_eq!(risk.pending_count(), 1);

    // Step 3: Fill confirmed — record P&L.
    let record = FillRecord {
        fill_id: "flow-fill-001".into(),
        chain_id: 1,
        token_in: address!("0000000000000000000000000000000000000001"),
        token_out: address!("0000000000000000000000000000000000000002"),
        amount_in: U256::from(1_000_000_000_000_000_000u64),
        amount_out: U256::from(2_000_000_000u64),
        gas_cost_wei: 50_000_000_000_000u128,
        pnl_wei: 200_000_000_000_000i128,
        tx_hash: B256::ZERO,
        timestamp: 1700000000,
        status: FillStatus::Confirmed,
    };
    pnl.record_fill(record);

    // Update risk state.
    risk.record_fill_complete(200_000_000_000_000i64);

    assert_eq!(risk.pending_count(), 0);
    assert_eq!(pnl.fill_count(), 1);
}

// ─── Helpers ────────────────────────────────────────────────────────

fn make_sample_order(id_bytes: [u8; 32]) -> DutchAuctionOrder {
    let now = Utc::now();
    DutchAuctionOrder {
        id: OrderId::new(id_bytes.into()),
        chain_id: ChainId::Ethereum,
        reactor: address!("0000000000000000000000000000000000000099"),
        signer: address!("0000000000000000000000000000000000000010"),
        nonce: U256::from(1u64),
        decay_start_time: now,
        decay_end_time: now + Duration::minutes(60),
        deadline: now + Duration::minutes(120),
        input: OrderInput {
            token: address!("0000000000000000000000000000000000000001"),
            amount: U256::from(1_000_000_000_000_000_000u64),
        },
        outputs: vec![OrderOutput {
            token: address!("0000000000000000000000000000000000000002"),
            start_amount: U256::from(2_000_000_000u64),
            end_amount: U256::from(1_900_000_000u64),
            recipient: address!("0000000000000000000000000000000000000010"),
        }],
    }
}
