//! Integration tests for the API server.
//!
//! Spins up a real axum server and verifies HTTP endpoints
//! return correct responses with real (default) state.

use std::sync::Arc;

use alloy::primitives::{address, B256, U256};
use phantom_api::config::ApiConfig;
use phantom_api::state::AppState;
use phantom_api::ws::EventBus;
use phantom_inventory::balance::BalanceTracker;
use phantom_inventory::pnl::{FillRecord, FillStatus, PnlTracker};
use phantom_inventory::risk::RiskManager;
use phantom_metrics::health::{ComponentStatus, HealthRegistry};
use serde_json::Value;

/// Finds an available port for testing.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Starts an API server on a random port and returns the base URL.
async fn start_test_server(state: AppState) -> String {
    let port = free_port();
    let config = ApiConfig {
        enabled: true,
        listen_addr: "127.0.0.1".into(),
        port,
        cors_origins: vec![],
        request_timeout_secs: 5,
    };

    tokio::spawn(async move {
        phantom_api::server::start_server(&config, state).await.ok();
    });

    // Give the server a moment to start.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    format!("http://127.0.0.1:{port}")
}

// ─── Health Endpoints ───────────────────────────────────────────────

#[tokio::test]
async fn health_live_returns_200() {
    let state = AppState::with_defaults();
    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/health/live"))
        .await
        .expect("request should succeed");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"]["alive"], true);
}

#[tokio::test]
async fn health_report_returns_system_status() {
    let health = Arc::new(HealthRegistry::default());
    health.register("test-component", true);
    health.update("test-component", ComponentStatus::Healthy, "running");

    let state = AppState::new(
        health,
        Arc::new(PnlTracker::with_defaults()),
        Arc::new(BalanceTracker::with_defaults()),
        Arc::new(RiskManager::with_defaults()),
        Arc::new(EventBus::with_defaults()),
    );

    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/health"))
        .await
        .expect("request should succeed");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["data"]["status"].is_string());
    assert!(body["data"]["components"].is_array());
}

#[tokio::test]
async fn health_ready_reflects_component_health() {
    let health = Arc::new(HealthRegistry::default());
    health.register("critical-svc", true);
    health.update("critical-svc", ComponentStatus::Healthy, "ok");

    let state = AppState::new(
        health,
        Arc::new(PnlTracker::with_defaults()),
        Arc::new(BalanceTracker::with_defaults()),
        Arc::new(RiskManager::with_defaults()),
        Arc::new(EventBus::with_defaults()),
    );

    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/health/ready")).await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"]["ready"], true);
}

// ─── P&L Endpoints ──────────────────────────────────────────────────

#[tokio::test]
async fn pnl_summary_empty_by_default() {
    let state = AppState::with_defaults();
    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/api/v1/pnl")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"]["total_fills"], 0);
    assert_eq!(body["data"]["total_realized_pnl_wei"], 0);
}

#[tokio::test]
async fn pnl_summary_reflects_recorded_fills() {
    let pnl = Arc::new(PnlTracker::with_defaults());

    pnl.record_fill(FillRecord {
        fill_id: "api-test-fill".into(),
        chain_id: 1,
        token_in: address!("0000000000000000000000000000000000000001"),
        token_out: address!("0000000000000000000000000000000000000002"),
        amount_in: U256::from(1_000_000_000_000_000_000u64),
        amount_out: U256::from(2_000_000_000u64),
        gas_cost_wei: 50_000_000_000_000u128,
        pnl_wei: 500_000_000_000_000i128,
        tx_hash: B256::ZERO,
        timestamp: 1700000000,
        status: FillStatus::Confirmed,
    });

    let state = AppState::new(
        Arc::new(HealthRegistry::default()),
        pnl,
        Arc::new(BalanceTracker::with_defaults()),
        Arc::new(RiskManager::with_defaults()),
        Arc::new(EventBus::with_defaults()),
    );

    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/api/v1/pnl")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"]["total_fills"], 1);
}

#[tokio::test]
async fn pnl_daily_endpoint() {
    let state = AppState::with_defaults();
    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/api/v1/pnl/daily"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["data"].is_array());
}

#[tokio::test]
async fn pnl_tokens_endpoint() {
    let state = AppState::with_defaults();
    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/api/v1/pnl/tokens"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["data"].is_array());
}

// ─── Fill Endpoints ─────────────────────────────────────────────────

#[tokio::test]
async fn fills_list_empty_by_default() {
    let state = AppState::with_defaults();
    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/api/v1/fills")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["data"].is_array());
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn fills_list_returns_recorded_fills() {
    let pnl = Arc::new(PnlTracker::with_defaults());

    for i in 0..3 {
        pnl.record_fill(FillRecord {
            fill_id: format!("list-fill-{i}"),
            chain_id: 1,
            token_in: address!("0000000000000000000000000000000000000001"),
            token_out: address!("0000000000000000000000000000000000000002"),
            amount_in: U256::from(1_000_000_000_000_000_000u64),
            amount_out: U256::from(2_000_000_000u64),
            gas_cost_wei: 50_000_000_000_000u128,
            pnl_wei: 100_000_000_000_000i128,
            tx_hash: B256::from([(i + 1) as u8; 32]),
            timestamp: 1700000000 + (i as u64 * 100),
            status: FillStatus::Confirmed,
        });
    }

    let state = AppState::new(
        Arc::new(HealthRegistry::default()),
        pnl,
        Arc::new(BalanceTracker::with_defaults()),
        Arc::new(RiskManager::with_defaults()),
        Arc::new(EventBus::with_defaults()),
    );

    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/api/v1/fills")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 3);
}

// ─── Risk Endpoint ──────────────────────────────────────────────────

#[tokio::test]
async fn risk_status_endpoint() {
    let state = AppState::with_defaults();
    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/api/v1/risk")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["data"]["enabled"].is_boolean());
}

// ─── System Status Endpoint ─────────────────────────────────────────

#[tokio::test]
async fn system_status_endpoint() {
    let state = AppState::with_defaults();
    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/api/v1/status")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert!(body["data"].is_object());
}

// ─── 404 Handling ───────────────────────────────────────────────────

#[tokio::test]
async fn unknown_route_returns_404() {
    let state = AppState::with_defaults();
    let base = start_test_server(state).await;

    let resp = reqwest::get(format!("{base}/nonexistent")).await.unwrap();
    assert_eq!(resp.status(), 404);
}
