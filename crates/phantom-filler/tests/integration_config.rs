//! Integration tests for configuration loading and validation.

use phantom_common::config::AppConfig;

#[test]
fn config_roundtrip_from_toml_string() {
    let toml = r#"
[chains.ethereum]
chain_id = "ethereum"
rpc_url = "https://eth.example.com"

[database]
url = "postgres://user:pass@localhost:5432/phantom"
max_connections = 5
min_connections = 1
connect_timeout_secs = 10

[redis]
url = "redis://127.0.0.1:6379"
pool_size = 4

[strategy]
min_profit_bps = 25
max_gas_price_gwei = 100
evaluation_timeout_ms = 5000

[execution]
flashbots_enabled = false

[api]
host = "127.0.0.1"
port = 8080
websocket_enabled = true

[metrics]
enabled = true
port = 9090
log_level = "info"
json_logs = false
"#;

    let config = AppConfig::from_toml_str(toml).expect("valid TOML config");
    assert_eq!(config.database.max_connections, 5);
    assert_eq!(config.redis.pool_size, 4);
    assert_eq!(config.api.port, 8080);
    assert!(config.metrics.enabled);
}

#[test]
fn config_validation_catches_empty_database_url() {
    let toml = r#"
[chains.ethereum]
chain_id = "ethereum"
rpc_url = "https://eth.example.com"

[database]
url = ""

[redis]
url = "redis://127.0.0.1:6379"

[strategy]

[execution]

[api]

[metrics]
"#;

    // Empty database URL should fail validation.
    assert!(AppConfig::from_toml_str(toml).is_err());
}

#[test]
fn config_validation_catches_empty_chains() {
    let toml = r#"
[database]
url = "postgres://user:pass@localhost/phantom"

[redis]
url = "redis://127.0.0.1:6379"

[strategy]

[execution]

[api]

[metrics]
"#;

    // No chains configured should fail validation.
    let result = AppConfig::from_toml_str(toml);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("chain"));
}

#[test]
fn config_with_chains() {
    let toml = r#"
[chains.ethereum]
chain_id = "ethereum"
rpc_url = "https://eth.example.com"
max_concurrent_requests = 10
request_timeout_ms = 5000

[chains.arbitrum]
chain_id = "arbitrum"
rpc_url = "https://arb.example.com"
ws_url = "wss://arb.example.com"
max_concurrent_requests = 20
request_timeout_ms = 3000
mempool_enabled = true

[database]
url = "postgres://user:pass@localhost/phantom"
max_connections = 5
min_connections = 1
connect_timeout_secs = 10

[redis]
url = "redis://127.0.0.1:6379"
pool_size = 4

[strategy]
min_profit_bps = 25
max_gas_price_gwei = 100
evaluation_timeout_ms = 5000

[execution]
flashbots_enabled = false

[api]
host = "0.0.0.0"
port = 3000
websocket_enabled = false

[metrics]
enabled = false
port = 9090
log_level = "debug"
json_logs = true
"#;

    let config = AppConfig::from_toml_str(toml).expect("valid TOML");
    assert_eq!(config.chains.len(), 2);
    assert!(config.chains.contains_key("ethereum"));
    assert!(config.chains.contains_key("arbitrum"));

    let arb = &config.chains["arbitrum"];
    assert!(arb.mempool_enabled);
    assert_eq!(arb.max_concurrent_requests, 20);
    assert!(arb.ws_url.is_some());
}

#[test]
fn env_overrides_apply_correctly() {
    let toml = r#"
[chains.ethereum]
chain_id = "ethereum"
rpc_url = "https://eth.example.com"

[database]
url = "postgres://original@localhost/phantom"

[redis]
url = "redis://127.0.0.1:6379"

[strategy]

[execution]

[api]
host = "127.0.0.1"
port = 8080
websocket_enabled = true

[metrics]
enabled = true
port = 9090
log_level = "info"
json_logs = false
"#;

    // Set env var before applying overrides.
    std::env::set_var(
        "PHANTOM_DATABASE_URL",
        "postgres://overridden@localhost/phantom",
    );

    let mut config = AppConfig::from_toml_str(toml).expect("valid TOML");
    config.apply_env_overrides();

    assert_eq!(
        config.database.url,
        "postgres://overridden@localhost/phantom"
    );

    // Clean up.
    std::env::remove_var("PHANTOM_DATABASE_URL");
}
