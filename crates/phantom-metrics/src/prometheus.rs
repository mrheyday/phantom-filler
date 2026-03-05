//! Prometheus metrics integration.
//!
//! Initializes the global metrics recorder using `metrics-exporter-prometheus`
//! and defines domain-specific metric names used across the system.

use std::net::SocketAddr;

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

// ─── Metric Name Constants ───────────────────────────────────────────

/// Order discovery metrics.
pub mod order {
    /// Counter: total orders discovered.
    pub const DISCOVERED: &str = "phantom_orders_discovered_total";
    /// Counter: orders successfully filled.
    pub const FILLED: &str = "phantom_orders_filled_total";
    /// Counter: orders that expired unfilled.
    pub const EXPIRED: &str = "phantom_orders_expired_total";
    /// Counter: fill transactions that reverted.
    pub const REVERTED: &str = "phantom_orders_reverted_total";
    /// Gauge: current number of active (open) orders.
    pub const ACTIVE: &str = "phantom_orders_active";
}

/// Execution metrics.
pub mod execution {
    /// Histogram: time from order discovery to fill submission (seconds).
    pub const FILL_LATENCY: &str = "phantom_execution_fill_latency_seconds";
    /// Counter: total gas spent in wei.
    pub const GAS_SPENT: &str = "phantom_execution_gas_spent_wei_total";
    /// Gauge: currently pending (unconfirmed) fill transactions.
    pub const PENDING_FILLS: &str = "phantom_execution_pending_fills";
    /// Counter: total transactions submitted.
    pub const TX_SUBMITTED: &str = "phantom_execution_tx_submitted_total";
    /// Counter: total transactions confirmed.
    pub const TX_CONFIRMED: &str = "phantom_execution_tx_confirmed_total";
}

/// Pricing metrics.
pub mod pricing {
    /// Histogram: price fetch latency (seconds).
    pub const FETCH_LATENCY: &str = "phantom_pricing_fetch_latency_seconds";
    /// Gauge: number of stale price entries.
    pub const STALE_PRICES: &str = "phantom_pricing_stale_prices";
    /// Counter: total price fetches.
    pub const FETCHES_TOTAL: &str = "phantom_pricing_fetches_total";
}

/// Risk and P&L metrics.
pub mod risk {
    /// Gauge: daily realized P&L in wei.
    pub const DAILY_PNL: &str = "phantom_risk_daily_pnl_wei";
    /// Gauge: total realized P&L in wei.
    pub const TOTAL_PNL: &str = "phantom_risk_total_pnl_wei";
    /// Gauge: position utilization ratio (0.0 - 1.0).
    pub const POSITION_UTILIZATION: &str = "phantom_risk_position_utilization";
}

/// Chain connectivity metrics.
pub mod chain {
    /// Gauge: latest block number per chain.
    pub const BLOCK_HEIGHT: &str = "phantom_chain_block_height";
    /// Histogram: RPC call latency (seconds).
    pub const RPC_LATENCY: &str = "phantom_chain_rpc_latency_seconds";
    /// Gauge: chain connection status (1 = connected, 0 = disconnected).
    pub const CONNECTION_STATUS: &str = "phantom_chain_connection_status";
    /// Counter: total RPC errors.
    pub const RPC_ERRORS: &str = "phantom_chain_rpc_errors_total";
}

/// System-level metrics.
pub mod system {
    /// Gauge: process uptime in seconds.
    pub const UPTIME: &str = "phantom_system_uptime_seconds";
    /// Gauge: number of active tokio tasks.
    pub const ACTIVE_TASKS: &str = "phantom_system_active_tasks";
}

// ─── Standard Label Keys ─────────────────────────────────────────────

/// Standard label key for chain identifier.
pub const LABEL_CHAIN: &str = "chain_id";
/// Standard label key for token address.
pub const LABEL_TOKEN: &str = "token";
/// Standard label key for strategy name.
pub const LABEL_STRATEGY: &str = "strategy";
/// Standard label key for relay name.
pub const LABEL_RELAY: &str = "relay";

// ─── Configuration ───────────────────────────────────────────────────

/// Configuration for the Prometheus metrics exporter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Whether metrics collection is enabled.
    pub enabled: bool,
    /// Address to bind the metrics HTTP server.
    pub listen_addr: String,
    /// Port for the metrics HTTP server.
    pub listen_port: u16,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_addr: "0.0.0.0".to_string(),
            listen_port: 9090,
        }
    }
}

impl MetricsConfig {
    /// Returns the socket address for the metrics server.
    pub fn socket_addr(&self) -> Result<SocketAddr, std::net::AddrParseError> {
        format!("{}:{}", self.listen_addr, self.listen_port).parse()
    }
}

// ─── Metric Descriptions ─────────────────────────────────────────────

/// Registers all metric descriptions with the global recorder.
///
/// This should be called once after installing the recorder so that
/// the `/metrics` endpoint includes `HELP` and `TYPE` annotations.
pub fn register_descriptions() {
    // Orders
    describe_counter!(
        order::DISCOVERED,
        "Total orders discovered across all chains"
    );
    describe_counter!(order::FILLED, "Total orders successfully filled");
    describe_counter!(order::EXPIRED, "Total orders that expired unfilled");
    describe_counter!(order::REVERTED, "Total fill transactions that reverted");
    describe_gauge!(order::ACTIVE, "Current number of active open orders");

    // Execution
    describe_histogram!(
        execution::FILL_LATENCY,
        "Time from order discovery to fill submission in seconds"
    );
    describe_counter!(execution::GAS_SPENT, "Total gas spent in wei");
    describe_gauge!(
        execution::PENDING_FILLS,
        "Currently pending unconfirmed fill transactions"
    );
    describe_counter!(
        execution::TX_SUBMITTED,
        "Total transactions submitted to the network"
    );
    describe_counter!(
        execution::TX_CONFIRMED,
        "Total transactions confirmed on-chain"
    );

    // Pricing
    describe_histogram!(pricing::FETCH_LATENCY, "Price fetch latency in seconds");
    describe_gauge!(pricing::STALE_PRICES, "Number of stale price entries");
    describe_counter!(pricing::FETCHES_TOTAL, "Total price fetch operations");

    // Risk
    describe_gauge!(risk::DAILY_PNL, "Daily realized P&L in wei");
    describe_gauge!(risk::TOTAL_PNL, "Total realized P&L in wei");
    describe_gauge!(
        risk::POSITION_UTILIZATION,
        "Position utilization ratio from 0.0 to 1.0"
    );

    // Chain
    describe_gauge!(chain::BLOCK_HEIGHT, "Latest block number per chain");
    describe_histogram!(chain::RPC_LATENCY, "RPC call latency in seconds");
    describe_gauge!(
        chain::CONNECTION_STATUS,
        "Chain connection status (1=connected, 0=disconnected)"
    );
    describe_counter!(chain::RPC_ERRORS, "Total RPC errors per chain");

    // System
    describe_gauge!(system::UPTIME, "Process uptime in seconds");
    describe_gauge!(system::ACTIVE_TASKS, "Number of active tokio tasks");
}

// ─── Initialization ──────────────────────────────────────────────────

/// Installs the Prometheus exporter as the global metrics recorder and
/// starts the HTTP server for scraping.
///
/// Returns `Ok(())` if the recorder was successfully installed, or an
/// error if binding or installation failed.
pub fn init_metrics(config: &MetricsConfig) -> anyhow::Result<()> {
    if !config.enabled {
        info!("metrics collection disabled");
        return Ok(());
    }

    let addr = config
        .socket_addr()
        .map_err(|e| anyhow::anyhow!("invalid metrics listen address: {e}"))?;

    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .map_err(|e| {
            error!(error = %e, "failed to install Prometheus recorder");
            anyhow::anyhow!("failed to install Prometheus recorder: {e}")
        })?;

    register_descriptions();

    info!(
        addr = %addr,
        "Prometheus metrics exporter started"
    );

    Ok(())
}

// ─── Helper Functions ────────────────────────────────────────────────

/// Increments the order discovered counter for a chain.
pub fn record_order_discovered(chain_id: u64) {
    counter!(order::DISCOVERED, LABEL_CHAIN => chain_id.to_string()).increment(1);
}

/// Increments the order filled counter for a chain.
pub fn record_order_filled(chain_id: u64) {
    counter!(order::FILLED, LABEL_CHAIN => chain_id.to_string()).increment(1);
}

/// Increments the order expired counter for a chain.
pub fn record_order_expired(chain_id: u64) {
    counter!(order::EXPIRED, LABEL_CHAIN => chain_id.to_string()).increment(1);
}

/// Increments the order reverted counter for a chain.
pub fn record_order_reverted(chain_id: u64) {
    counter!(order::REVERTED, LABEL_CHAIN => chain_id.to_string()).increment(1);
}

/// Sets the active order count.
pub fn set_active_orders(count: u64) {
    gauge!(order::ACTIVE).set(count as f64);
}

/// Records fill latency in seconds.
pub fn record_fill_latency(seconds: f64, chain_id: u64) {
    histogram!(execution::FILL_LATENCY, LABEL_CHAIN => chain_id.to_string()).record(seconds);
}

/// Adds to the gas spent counter.
pub fn record_gas_spent(wei: u64, chain_id: u64) {
    counter!(execution::GAS_SPENT, LABEL_CHAIN => chain_id.to_string()).increment(wei);
}

/// Sets the pending fills gauge.
pub fn set_pending_fills(count: u64) {
    gauge!(execution::PENDING_FILLS).set(count as f64);
}

/// Records a transaction submission.
pub fn record_tx_submitted(chain_id: u64) {
    counter!(execution::TX_SUBMITTED, LABEL_CHAIN => chain_id.to_string()).increment(1);
}

/// Records a transaction confirmation.
pub fn record_tx_confirmed(chain_id: u64) {
    counter!(execution::TX_CONFIRMED, LABEL_CHAIN => chain_id.to_string()).increment(1);
}

/// Records price fetch latency.
pub fn record_price_fetch_latency(seconds: f64) {
    histogram!(pricing::FETCH_LATENCY).record(seconds);
}

/// Sets the stale prices gauge.
pub fn set_stale_prices(count: u64) {
    gauge!(pricing::STALE_PRICES).set(count as f64);
}

/// Increments the price fetch counter.
pub fn record_price_fetch() {
    counter!(pricing::FETCHES_TOTAL).increment(1);
}

/// Sets the daily P&L gauge.
pub fn set_daily_pnl(wei: f64) {
    gauge!(risk::DAILY_PNL).set(wei);
}

/// Sets the total P&L gauge.
pub fn set_total_pnl(wei: f64) {
    gauge!(risk::TOTAL_PNL).set(wei);
}

/// Sets the position utilization ratio.
pub fn set_position_utilization(ratio: f64) {
    gauge!(risk::POSITION_UTILIZATION).set(ratio);
}

/// Sets the block height for a chain.
pub fn set_block_height(chain_id: u64, height: u64) {
    gauge!(chain::BLOCK_HEIGHT, LABEL_CHAIN => chain_id.to_string()).set(height as f64);
}

/// Records RPC call latency.
pub fn record_rpc_latency(seconds: f64, chain_id: u64) {
    histogram!(chain::RPC_LATENCY, LABEL_CHAIN => chain_id.to_string()).record(seconds);
}

/// Sets chain connection status.
pub fn set_chain_connected(chain_id: u64, connected: bool) {
    gauge!(chain::CONNECTION_STATUS, LABEL_CHAIN => chain_id.to_string()).set(if connected {
        1.0
    } else {
        0.0
    });
}

/// Records an RPC error.
pub fn record_rpc_error(chain_id: u64) {
    counter!(chain::RPC_ERRORS, LABEL_CHAIN => chain_id.to_string()).increment(1);
}

/// Sets the system uptime.
pub fn set_uptime(seconds: f64) {
    gauge!(system::UPTIME).set(seconds);
}

/// Sets the active tasks gauge.
pub fn set_active_tasks(count: u64) {
    gauge!(system::ACTIVE_TASKS).set(count as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default() {
        let config = MetricsConfig::default();
        assert!(config.enabled);
        assert_eq!(config.listen_port, 9090);
        assert_eq!(config.listen_addr, "0.0.0.0");
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = MetricsConfig {
            enabled: false,
            listen_addr: "127.0.0.1".to_string(),
            listen_port: 8080,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: MetricsConfig = serde_json::from_str(&json).expect("deserialize");
        assert!(!deserialized.enabled);
        assert_eq!(deserialized.listen_port, 8080);
    }

    #[test]
    fn config_socket_addr() {
        let config = MetricsConfig::default();
        let addr = config.socket_addr().expect("valid address");
        assert_eq!(addr.port(), 9090);
    }

    #[test]
    fn config_socket_addr_invalid() {
        let config = MetricsConfig {
            listen_addr: "not-an-ip".to_string(),
            ..Default::default()
        };
        assert!(config.socket_addr().is_err());
    }

    #[test]
    fn metric_name_constants() {
        // Verify all metric names follow the phantom_ prefix convention.
        assert!(order::DISCOVERED.starts_with("phantom_"));
        assert!(order::FILLED.starts_with("phantom_"));
        assert!(order::EXPIRED.starts_with("phantom_"));
        assert!(order::REVERTED.starts_with("phantom_"));
        assert!(order::ACTIVE.starts_with("phantom_"));

        assert!(execution::FILL_LATENCY.starts_with("phantom_"));
        assert!(execution::GAS_SPENT.starts_with("phantom_"));
        assert!(execution::PENDING_FILLS.starts_with("phantom_"));
        assert!(execution::TX_SUBMITTED.starts_with("phantom_"));
        assert!(execution::TX_CONFIRMED.starts_with("phantom_"));

        assert!(pricing::FETCH_LATENCY.starts_with("phantom_"));
        assert!(pricing::STALE_PRICES.starts_with("phantom_"));
        assert!(pricing::FETCHES_TOTAL.starts_with("phantom_"));

        assert!(risk::DAILY_PNL.starts_with("phantom_"));
        assert!(risk::TOTAL_PNL.starts_with("phantom_"));
        assert!(risk::POSITION_UTILIZATION.starts_with("phantom_"));

        assert!(chain::BLOCK_HEIGHT.starts_with("phantom_"));
        assert!(chain::RPC_LATENCY.starts_with("phantom_"));
        assert!(chain::CONNECTION_STATUS.starts_with("phantom_"));
        assert!(chain::RPC_ERRORS.starts_with("phantom_"));

        assert!(system::UPTIME.starts_with("phantom_"));
        assert!(system::ACTIVE_TASKS.starts_with("phantom_"));
    }

    #[test]
    fn label_constants() {
        assert_eq!(LABEL_CHAIN, "chain_id");
        assert_eq!(LABEL_TOKEN, "token");
        assert_eq!(LABEL_STRATEGY, "strategy");
        assert_eq!(LABEL_RELAY, "relay");
    }

    #[test]
    fn metric_names_unique() {
        let names = [
            order::DISCOVERED,
            order::FILLED,
            order::EXPIRED,
            order::REVERTED,
            order::ACTIVE,
            execution::FILL_LATENCY,
            execution::GAS_SPENT,
            execution::PENDING_FILLS,
            execution::TX_SUBMITTED,
            execution::TX_CONFIRMED,
            pricing::FETCH_LATENCY,
            pricing::STALE_PRICES,
            pricing::FETCHES_TOTAL,
            risk::DAILY_PNL,
            risk::TOTAL_PNL,
            risk::POSITION_UTILIZATION,
            chain::BLOCK_HEIGHT,
            chain::RPC_LATENCY,
            chain::CONNECTION_STATUS,
            chain::RPC_ERRORS,
            system::UPTIME,
            system::ACTIVE_TASKS,
        ];
        let mut seen = std::collections::HashSet::new();
        for name in &names {
            assert!(seen.insert(name), "duplicate metric name: {name}");
        }
    }

    #[test]
    fn init_metrics_disabled() {
        let config = MetricsConfig {
            enabled: false,
            ..Default::default()
        };
        // Should succeed immediately when disabled.
        let result = init_metrics(&config);
        assert!(result.is_ok());
    }

    // NOTE: init_metrics with enabled=true installs a global recorder
    // which can only be done once per process. We test the disabled path
    // and config validation instead. Integration tests with the actual
    // exporter should use a dedicated test binary or #[ignore].

    #[test]
    fn helper_functions_do_not_panic() {
        // Without a global recorder installed, metric macros use a no-op.
        // These should all complete without panicking.
        record_order_discovered(1);
        record_order_filled(1);
        record_order_expired(1);
        record_order_reverted(1);
        set_active_orders(10);
        record_fill_latency(0.5, 1);
        record_gas_spent(21000, 1);
        set_pending_fills(3);
        record_tx_submitted(1);
        record_tx_confirmed(1);
        record_price_fetch_latency(0.1);
        set_stale_prices(2);
        record_price_fetch();
        set_daily_pnl(1000.0);
        set_total_pnl(5000.0);
        set_position_utilization(0.75);
        set_block_height(1, 12345678);
        record_rpc_latency(0.05, 1);
        set_chain_connected(1, true);
        set_chain_connected(1, false);
        record_rpc_error(1);
        set_uptime(3600.0);
        set_active_tasks(50);
    }
}
