//! Wallet management and transaction signing.

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use dashmap::DashMap;
use phantom_common::error::{ExecutionError, ExecutionResult};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Configuration for wallet management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletConfig {
    /// Maximum number of wallets allowed.
    pub max_wallets: usize,
}

impl Default for WalletConfig {
    fn default() -> Self {
        Self { max_wallets: 10 }
    }
}

/// Information about a managed wallet (without exposing the private key).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    /// Wallet address.
    pub address: Address,
    /// Human-readable label for the wallet.
    pub label: String,
}

/// Manages signing wallets for fill transaction execution.
///
/// Stores private key signers indexed by address, supporting multiple
/// filler wallets for parallel execution across chains.
pub struct WalletManager {
    wallets: DashMap<Address, WalletEntry>,
    config: WalletConfig,
}

struct WalletEntry {
    signer: PrivateKeySigner,
    label: String,
}

impl WalletManager {
    /// Creates a new empty wallet manager.
    pub fn new(config: WalletConfig) -> Self {
        Self {
            wallets: DashMap::new(),
            config,
        }
    }

    /// Creates a wallet manager with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(WalletConfig::default())
    }

    /// Imports a wallet from a hex-encoded private key.
    ///
    /// The key should be a 64-character hex string (with or without 0x prefix).
    pub fn import_wallet(&self, private_key_hex: &str, label: &str) -> ExecutionResult<Address> {
        if self.wallets.len() >= self.config.max_wallets {
            return Err(ExecutionError::NonceError(format!(
                "wallet limit reached (max {})",
                self.config.max_wallets
            )));
        }

        let signer: PrivateKeySigner = private_key_hex
            .parse()
            .map_err(|e| ExecutionError::NonceError(format!("invalid private key: {e}")))?;

        let address = signer.address();

        info!(address = %address, label = %label, "imported wallet");

        self.wallets.insert(
            address,
            WalletEntry {
                signer,
                label: label.to_string(),
            },
        );

        Ok(address)
    }

    /// Returns the signer for a given wallet address.
    pub fn get_signer(&self, address: &Address) -> ExecutionResult<PrivateKeySigner> {
        self.wallets
            .get(address)
            .map(|entry| entry.signer.clone())
            .ok_or_else(|| ExecutionError::NonceError(format!("wallet not found: {address}")))
    }

    /// Returns an `EthereumWallet` for use with alloy providers.
    pub fn get_ethereum_wallet(
        &self,
        address: &Address,
    ) -> ExecutionResult<alloy::network::EthereumWallet> {
        let signer = self.get_signer(address)?;
        Ok(alloy::network::EthereumWallet::from(signer))
    }

    /// Builds an `EthereumWallet` and parsed RPC URL pair for constructing
    /// a signing provider.
    ///
    /// Callers use the returned wallet with
    /// `ProviderBuilder::new().wallet(wallet).connect_http(url)`
    /// to create a provider that can sign and send transactions.
    pub fn prepare_signing(
        &self,
        address: &Address,
        rpc_url: &str,
    ) -> ExecutionResult<(alloy::network::EthereumWallet, reqwest::Url)> {
        let wallet = self.get_ethereum_wallet(address)?;
        let url: reqwest::Url = rpc_url
            .parse()
            .map_err(|e| ExecutionError::NonceError(format!("invalid RPC URL: {e}")))?;

        debug!(address = %address, rpc = %rpc_url, "prepared signing wallet");

        Ok((wallet, url))
    }

    /// Removes a wallet by address. Returns true if removed.
    pub fn remove_wallet(&self, address: &Address) -> bool {
        if self.wallets.remove(address).is_some() {
            info!(address = %address, "removed wallet");
            true
        } else {
            warn!(address = %address, "attempted to remove unknown wallet");
            false
        }
    }

    /// Lists all managed wallets (address and label only).
    pub fn list_wallets(&self) -> Vec<WalletInfo> {
        self.wallets
            .iter()
            .map(|entry| WalletInfo {
                address: *entry.key(),
                label: entry.label.clone(),
            })
            .collect()
    }

    /// Returns the number of managed wallets.
    pub fn wallet_count(&self) -> usize {
        self.wallets.len()
    }

    /// Returns true if no wallets are managed.
    pub fn is_empty(&self) -> bool {
        self.wallets.is_empty()
    }

    /// Returns true if a wallet with the given address exists.
    pub fn has_wallet(&self, address: &Address) -> bool {
        self.wallets.contains_key(address)
    }
}

impl Default for WalletManager {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known test private key (Anvil default account 0).
    // 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
    const TEST_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    // Second Anvil key.
    const TEST_KEY_2: &str = "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

    #[test]
    fn import_wallet() {
        let manager = WalletManager::with_defaults();
        let address = manager.import_wallet(TEST_KEY, "filler-1").expect("import");

        assert!(manager.has_wallet(&address));
        assert_eq!(manager.wallet_count(), 1);
    }

    #[test]
    fn import_wallet_with_0x_prefix() {
        let manager = WalletManager::with_defaults();
        let key = format!("0x{TEST_KEY}");
        let address = manager.import_wallet(&key, "filler-1").expect("import");

        assert!(manager.has_wallet(&address));
    }

    #[test]
    fn import_invalid_key() {
        let manager = WalletManager::with_defaults();
        let result = manager.import_wallet("not-a-valid-key", "bad");

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid private key"));
    }

    #[test]
    fn import_multiple_wallets() {
        let manager = WalletManager::with_defaults();
        let addr1 = manager.import_wallet(TEST_KEY, "filler-1").expect("import");
        let addr2 = manager
            .import_wallet(TEST_KEY_2, "filler-2")
            .expect("import");

        assert_ne!(addr1, addr2);
        assert_eq!(manager.wallet_count(), 2);
    }

    #[test]
    fn import_exceeds_max_wallets() {
        let config = WalletConfig { max_wallets: 1 };
        let manager = WalletManager::new(config);

        manager.import_wallet(TEST_KEY, "filler-1").expect("import");
        let result = manager.import_wallet(TEST_KEY_2, "filler-2");

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("wallet limit"));
    }

    #[test]
    fn get_signer() {
        let manager = WalletManager::with_defaults();
        let address = manager.import_wallet(TEST_KEY, "filler-1").expect("import");

        let signer = manager.get_signer(&address).expect("signer");
        assert_eq!(signer.address(), address);
    }

    #[test]
    fn get_signer_unknown() {
        let manager = WalletManager::with_defaults();
        let result = manager.get_signer(&Address::ZERO);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("wallet not found"));
    }

    #[test]
    fn get_ethereum_wallet() {
        let manager = WalletManager::with_defaults();
        let address = manager.import_wallet(TEST_KEY, "filler-1").expect("import");

        let wallet = manager.get_ethereum_wallet(&address);
        assert!(wallet.is_ok());
    }

    #[test]
    fn remove_wallet() {
        let manager = WalletManager::with_defaults();
        let address = manager.import_wallet(TEST_KEY, "filler-1").expect("import");

        assert!(manager.remove_wallet(&address));
        assert!(!manager.has_wallet(&address));
        assert_eq!(manager.wallet_count(), 0);
    }

    #[test]
    fn remove_unknown_wallet() {
        let manager = WalletManager::with_defaults();
        assert!(!manager.remove_wallet(&Address::ZERO));
    }

    #[test]
    fn list_wallets() {
        let manager = WalletManager::with_defaults();
        manager.import_wallet(TEST_KEY, "filler-1").expect("import");
        manager
            .import_wallet(TEST_KEY_2, "filler-2")
            .expect("import");

        let wallets = manager.list_wallets();
        assert_eq!(wallets.len(), 2);

        let labels: Vec<_> = wallets.iter().map(|w| w.label.clone()).collect();
        assert!(labels.contains(&"filler-1".to_string()));
        assert!(labels.contains(&"filler-2".to_string()));
    }

    #[test]
    fn wallet_info_serde() {
        let info = WalletInfo {
            address: Address::ZERO,
            label: "test".into(),
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let deserialized: WalletInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.label, "test");
    }

    #[test]
    fn wallet_config_default() {
        let config = WalletConfig::default();
        assert_eq!(config.max_wallets, 10);
    }

    #[test]
    fn default_manager() {
        let manager = WalletManager::default();
        assert!(manager.is_empty());
        assert_eq!(manager.wallet_count(), 0);
    }

    #[tokio::test]
    async fn signing_provider_with_anvil() {
        let anvil = match alloy::node_bindings::Anvil::new().try_spawn() {
            Ok(a) => a,
            Err(_) => {
                eprintln!("Anvil not available, skipping");
                return;
            }
        };

        let manager = WalletManager::with_defaults();
        // Anvil default key 0
        let key = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let address = manager.import_wallet(key, "anvil-0").expect("import");

        let (wallet, url) = manager
            .prepare_signing(&address, &anvil.endpoint())
            .expect("prepare");

        let provider = alloy::providers::ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url);

        // Verify the provider works by getting block number.
        use alloy::providers::Provider;
        let block = provider.get_block_number().await.expect("block number");
        assert!(block < 100);
    }
}
