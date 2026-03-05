//! Multi-chain provider manager for RPC/WebSocket connections.

use std::sync::Arc;

use alloy::network::Ethereum;
use alloy::providers::{Provider, ProviderBuilder};
use dashmap::DashMap;
use phantom_common::config::ChainConfig;
use phantom_common::error::ChainError;
use phantom_common::types::ChainId;
use tracing::{info, warn};

/// Type-erased Alloy provider.
pub type DynProvider = dyn Provider<Ethereum> + Send + Sync;

/// Manages RPC/WebSocket provider connections across multiple chains.
pub struct ProviderManager {
    /// Providers indexed by chain ID.
    providers: DashMap<ChainId, Arc<DynProvider>>,
    /// Chain configurations.
    configs: DashMap<ChainId, ChainConfig>,
}

impl ProviderManager {
    /// Creates a new empty provider manager.
    pub fn new() -> Self {
        Self {
            providers: DashMap::new(),
            configs: DashMap::new(),
        }
    }

    /// Creates a provider manager and connects to all configured chains.
    pub async fn from_configs(
        configs: impl IntoIterator<Item = (String, ChainConfig)>,
    ) -> Result<Self, ChainError> {
        let manager = Self::new();

        for (name, config) in configs {
            info!(chain = %name, chain_id = %config.chain_id, "connecting to chain");
            manager.add_chain(config).await?;
        }

        Ok(manager)
    }

    /// Adds a new chain connection using an HTTP RPC endpoint.
    pub async fn add_chain(&self, config: ChainConfig) -> Result<(), ChainError> {
        let chain_id = config.chain_id;

        let rpc_url = config
            .rpc_url
            .parse()
            .map_err(|e| ChainError::ConnectionFailed {
                chain_id,
                reason: format!("invalid RPC URL: {e}"),
            })?;

        let provider = ProviderBuilder::new().connect_http(rpc_url);

        self.providers.insert(chain_id, Arc::new(provider));
        self.configs.insert(chain_id, config);

        info!(%chain_id, "chain provider registered");
        Ok(())
    }

    /// Returns the provider for a chain.
    pub fn get_provider(&self, chain_id: ChainId) -> Result<Arc<DynProvider>, ChainError> {
        self.providers
            .get(&chain_id)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or(ChainError::UnsupportedChain(chain_id))
    }

    /// Returns the configuration for a chain.
    pub fn get_config(&self, chain_id: ChainId) -> Option<ChainConfig> {
        self.configs
            .get(&chain_id)
            .map(|entry| entry.value().clone())
    }

    /// Returns all connected chain IDs.
    pub fn connected_chains(&self) -> Vec<ChainId> {
        self.providers.iter().map(|entry| *entry.key()).collect()
    }

    /// Checks health of a specific chain's provider by fetching the latest block number.
    pub async fn check_health(&self, chain_id: ChainId) -> Result<u64, ChainError> {
        let provider = self.get_provider(chain_id)?;
        let block_number =
            provider
                .get_block_number()
                .await
                .map_err(|e| ChainError::ProviderError {
                    chain_id,
                    reason: format!("health check failed: {e}"),
                })?;

        info!(%chain_id, block_number, "health check passed");
        Ok(block_number)
    }

    /// Checks health of all connected chains and returns results.
    pub async fn check_all_health(&self) -> Vec<(ChainId, Result<u64, ChainError>)> {
        let chains = self.connected_chains();
        let mut results = Vec::with_capacity(chains.len());

        for chain_id in chains {
            let result = self.check_health(chain_id).await;
            if let Err(ref e) = result {
                warn!(%chain_id, error = %e, "health check failed");
            }
            results.push((chain_id, result));
        }

        results
    }

    /// Removes a chain connection.
    pub fn remove_chain(&self, chain_id: ChainId) -> bool {
        let removed = self.providers.remove(&chain_id).is_some();
        self.configs.remove(&chain_id);
        if removed {
            info!(%chain_id, "chain provider removed");
        }
        removed
    }

    /// Returns the number of connected chains.
    pub fn chain_count(&self) -> usize {
        self.providers.len()
    }
}

impl Default for ProviderManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(chain_id: ChainId, url: &str) -> ChainConfig {
        ChainConfig {
            chain_id,
            rpc_url: url.to_string(),
            ws_url: None,
            max_concurrent_requests: 64,
            request_timeout_ms: 5000,
            mempool_enabled: false,
        }
    }

    #[tokio::test]
    async fn create_empty_manager() {
        let manager = ProviderManager::new();
        assert_eq!(manager.chain_count(), 0);
        assert!(manager.connected_chains().is_empty());
    }

    #[tokio::test]
    async fn add_and_retrieve_chain() {
        let manager = ProviderManager::new();
        let config = test_config(ChainId::Ethereum, "https://eth.example.com");

        manager.add_chain(config).await.expect("add chain");
        assert_eq!(manager.chain_count(), 1);
        assert!(manager.get_provider(ChainId::Ethereum).is_ok());
    }

    #[tokio::test]
    async fn get_unsupported_chain_returns_error() {
        let manager = ProviderManager::new();
        let result = manager.get_provider(ChainId::Arbitrum);
        assert!(matches!(result, Err(ChainError::UnsupportedChain(_))));
    }

    #[tokio::test]
    async fn add_multiple_chains() {
        let manager = ProviderManager::new();
        manager
            .add_chain(test_config(ChainId::Ethereum, "https://eth.example.com"))
            .await
            .expect("add eth");
        manager
            .add_chain(test_config(ChainId::Arbitrum, "https://arb.example.com"))
            .await
            .expect("add arb");
        manager
            .add_chain(test_config(ChainId::Base, "https://base.example.com"))
            .await
            .expect("add base");

        assert_eq!(manager.chain_count(), 3);
        let chains = manager.connected_chains();
        assert!(chains.contains(&ChainId::Ethereum));
        assert!(chains.contains(&ChainId::Arbitrum));
        assert!(chains.contains(&ChainId::Base));
    }

    #[tokio::test]
    async fn remove_chain() {
        let manager = ProviderManager::new();
        manager
            .add_chain(test_config(ChainId::Ethereum, "https://eth.example.com"))
            .await
            .expect("add chain");

        assert!(manager.remove_chain(ChainId::Ethereum));
        assert_eq!(manager.chain_count(), 0);
        assert!(!manager.remove_chain(ChainId::Ethereum));
    }

    #[tokio::test]
    async fn get_config() {
        let manager = ProviderManager::new();
        let config = test_config(ChainId::Polygon, "https://polygon.example.com");
        manager.add_chain(config).await.expect("add chain");

        let retrieved = manager.get_config(ChainId::Polygon).expect("config exists");
        assert_eq!(retrieved.rpc_url, "https://polygon.example.com");
        assert!(manager.get_config(ChainId::Optimism).is_none());
    }

    #[tokio::test]
    async fn from_configs() {
        let configs = vec![
            (
                "ethereum".to_string(),
                test_config(ChainId::Ethereum, "https://eth.example.com"),
            ),
            (
                "arbitrum".to_string(),
                test_config(ChainId::Arbitrum, "https://arb.example.com"),
            ),
        ];

        let manager = ProviderManager::from_configs(configs)
            .await
            .expect("from configs");
        assert_eq!(manager.chain_count(), 2);
    }

    #[tokio::test]
    async fn invalid_url_returns_error() {
        let manager = ProviderManager::new();
        let config = test_config(ChainId::Ethereum, "not a valid url");
        let result = manager.add_chain(config).await;
        assert!(result.is_err());
    }
}
