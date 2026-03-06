//! Phantom Filler — high-performance intent execution engine for DeFi.
//!
//! This binary orchestrates all subsystems: chain connectivity, order discovery,
//! pricing, strategy evaluation, execution, settlement, inventory management,
//! observability, and the REST/WebSocket API.

use std::sync::Arc;

use phantom_api::config::ApiConfig as ServerApiConfig;
use phantom_api::state::AppState;
use phantom_api::ws::EventBus;
use phantom_common::config::AppConfig;
use phantom_inventory::balance::BalanceTracker;
use phantom_inventory::pnl::PnlTracker;
use phantom_inventory::risk::RiskManager;
use phantom_metrics::health::{ComponentStatus, HealthRegistry};
use phantom_metrics::logging::{init_logging, LogFormat, LoggingConfig};
use phantom_metrics::prometheus::{init_metrics, MetricsConfig};
use tracing::{error, info};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── CLI Arguments ────────────────────────────────────────────────
    let config_path = parse_config_path();

    // ── Configuration ────────────────────────────────────────────────
    let mut config = AppConfig::from_file(&config_path)?;
    config.apply_env_overrides();

    // ── Logging ──────────────────────────────────────────────────────
    let logging_config = LoggingConfig {
        enabled: true,
        level: config.metrics.log_level.clone(),
        format: if config.metrics.json_logs {
            LogFormat::Json
        } else {
            LogFormat::Pretty
        },
        ..Default::default()
    };
    init_logging(&logging_config)?;

    print_banner();

    info!(
        config_path = %config_path,
        chains = config.chains.len(),
        "configuration loaded"
    );

    // ── Metrics ──────────────────────────────────────────────────────
    let metrics_config = MetricsConfig {
        enabled: config.metrics.enabled,
        listen_addr: "0.0.0.0".to_string(),
        listen_port: config.metrics.port,
    };
    init_metrics(&metrics_config)?;

    // ── Core Components ──────────────────────────────────────────────
    let health = Arc::new(HealthRegistry::default());
    let event_bus = Arc::new(EventBus::with_defaults());
    let pnl = Arc::new(PnlTracker::with_defaults());
    let balances = Arc::new(BalanceTracker::with_defaults());
    let risk = Arc::new(RiskManager::with_defaults());

    // Register critical subsystems with health registry.
    health.register("api", true);
    health.register("chain_connector", true);
    health.register("order_discovery", true);
    health.register("execution_engine", true);
    health.register("settlement", false);

    info!(
        components = health.component_count(),
        "health checks registered"
    );

    // ── API Server ───────────────────────────────────────────────────
    let api_config = ServerApiConfig {
        enabled: true,
        listen_addr: config.api.host.clone(),
        port: config.api.port,
        cors_origins: Vec::new(),
        request_timeout_secs: 30,
    };

    let api_state = AppState::new(
        Arc::clone(&health),
        Arc::clone(&pnl),
        Arc::clone(&balances),
        Arc::clone(&risk),
        Arc::clone(&event_bus),
    );

    // Mark API as healthy once the server starts.
    health.update("api", ComponentStatus::Healthy, "server starting");

    let api_handle = tokio::spawn({
        let config = api_config.clone();
        let state = api_state.clone();
        async move {
            if let Err(e) = phantom_api::server::start_server(&config, state).await {
                error!(error = %e, "api server failed");
            }
        }
    });

    info!(
        host = %config.api.host,
        port = config.api.port,
        "api server started"
    );

    // ── Startup Complete ─────────────────────────────────────────────
    info!(
        version = VERSION,
        chains = config.chains.len(),
        "phantom-filler is ready"
    );

    // ── Graceful Shutdown ────────────────────────────────────────────
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            info!("shutdown signal received, stopping services");
        }
        Err(e) => {
            error!(error = %e, "failed to listen for shutdown signal");
        }
    }

    // Abort background tasks.
    api_handle.abort();

    info!("phantom-filler shut down");
    Ok(())
}

/// Parses the config file path from CLI arguments.
///
/// Usage: `phantom-filler [--config <path>]`
/// Default: `config.toml`
fn parse_config_path() -> String {
    let args: Vec<String> = std::env::args().collect();

    if args.contains(&"--help".to_string()) || args.contains(&"-h".to_string()) {
        println!("phantom-filler v{VERSION}");
        println!();
        println!("Usage: phantom-filler [OPTIONS]");
        println!();
        println!("Options:");
        println!("  --config <path>  Path to TOML config file (default: config.toml)");
        println!("  --help, -h       Show this help message");
        std::process::exit(0);
    }

    for i in 0..args.len() {
        if args[i] == "--config" {
            if let Some(path) = args.get(i + 1) {
                return path.clone();
            }
        }
    }

    "config.toml".to_string()
}

/// Prints the startup banner.
fn print_banner() {
    info!("╔══════════════════════════════════════════╗");
    info!("║         Phantom Filler v{:<17}║", VERSION);
    info!("║   High-Performance Intent Execution      ║");
    info!("╚══════════════════════════════════════════╝");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn default_config_path() {
        // When no args, defaults to "config.toml".
        let path = parse_config_path();
        assert_eq!(path, "config.toml");
    }

    #[test]
    fn banner_does_not_panic() {
        // Just ensure print_banner doesn't panic.
        // (It uses tracing::info which is a no-op without a subscriber.)
        print_banner();
    }

    #[test]
    fn logging_config_from_app_config() {
        let logging = LoggingConfig {
            enabled: true,
            level: "debug".to_string(),
            format: LogFormat::Json,
            ..Default::default()
        };
        assert_eq!(logging.level, "debug");
        assert_eq!(logging.format, LogFormat::Json);
    }

    #[test]
    fn metrics_config_mapping() {
        let config = MetricsConfig {
            enabled: true,
            listen_addr: "0.0.0.0".to_string(),
            listen_port: 9090,
        };
        assert!(config.enabled);
        assert_eq!(config.listen_port, 9090);
    }

    #[test]
    fn api_config_mapping() {
        let api = ServerApiConfig {
            enabled: true,
            listen_addr: "127.0.0.1".to_string(),
            port: 3000,
            cors_origins: vec!["http://localhost".to_string()],
            request_timeout_secs: 30,
        };
        assert_eq!(api.bind_addr(), "127.0.0.1:3000");
    }

    #[test]
    fn app_state_construction() {
        let health = Arc::new(HealthRegistry::default());
        let pnl = Arc::new(PnlTracker::with_defaults());
        let balances = Arc::new(BalanceTracker::with_defaults());
        let risk = Arc::new(RiskManager::with_defaults());
        let event_bus = Arc::new(EventBus::with_defaults());

        let state = AppState::new(
            Arc::clone(&health),
            Arc::clone(&pnl),
            Arc::clone(&balances),
            Arc::clone(&risk),
            Arc::clone(&event_bus),
        );

        assert_eq!(state.health.component_count(), 0);
        assert_eq!(state.pnl.fill_count(), 0);
    }

    #[test]
    fn health_registration() {
        let health = HealthRegistry::default();
        health.register("api", true);
        health.register("chain", true);
        health.register("settlement", false);
        assert_eq!(health.component_count(), 3);
    }
}
