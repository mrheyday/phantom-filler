//! Chain identifier types for supported EVM networks.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported EVM chain identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChainId {
    /// Ethereum Mainnet (chain ID 1)
    Ethereum,
    /// Arbitrum One (chain ID 42161)
    Arbitrum,
    /// Base (chain ID 8453)
    Base,
    /// Polygon PoS (chain ID 137)
    Polygon,
    /// Optimism (chain ID 10)
    Optimism,
}

impl ChainId {
    /// Returns the numeric chain ID.
    pub fn as_u64(&self) -> u64 {
        match self {
            Self::Ethereum => 1,
            Self::Arbitrum => 42161,
            Self::Base => 8453,
            Self::Polygon => 137,
            Self::Optimism => 10,
        }
    }

    /// Creates a `ChainId` from a numeric chain ID.
    ///
    /// Returns `None` if the chain ID is not supported.
    pub fn from_u64(id: u64) -> Option<Self> {
        match id {
            1 => Some(Self::Ethereum),
            42161 => Some(Self::Arbitrum),
            8453 => Some(Self::Base),
            137 => Some(Self::Polygon),
            10 => Some(Self::Optimism),
            _ => None,
        }
    }

    /// Returns all supported chain IDs.
    pub fn all() -> &'static [ChainId] {
        &[
            Self::Ethereum,
            Self::Arbitrum,
            Self::Base,
            Self::Polygon,
            Self::Optimism,
        ]
    }

    /// Returns a human-readable name for the chain.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ethereum => "Ethereum",
            Self::Arbitrum => "Arbitrum",
            Self::Base => "Base",
            Self::Polygon => "Polygon",
            Self::Optimism => "Optimism",
        }
    }
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.as_u64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_id_numeric_values() {
        assert_eq!(ChainId::Ethereum.as_u64(), 1);
        assert_eq!(ChainId::Arbitrum.as_u64(), 42161);
        assert_eq!(ChainId::Base.as_u64(), 8453);
        assert_eq!(ChainId::Polygon.as_u64(), 137);
        assert_eq!(ChainId::Optimism.as_u64(), 10);
    }

    #[test]
    fn chain_id_from_u64_roundtrip() {
        for chain in ChainId::all() {
            assert_eq!(ChainId::from_u64(chain.as_u64()), Some(*chain));
        }
    }

    #[test]
    fn chain_id_from_u64_unknown() {
        assert_eq!(ChainId::from_u64(999), None);
    }

    #[test]
    fn chain_id_serde_roundtrip() {
        for chain in ChainId::all() {
            let json = serde_json::to_string(chain).expect("serialize");
            let deserialized: ChainId = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*chain, deserialized);
        }
    }

    #[test]
    fn chain_id_display() {
        assert_eq!(ChainId::Ethereum.to_string(), "Ethereum (1)");
        assert_eq!(ChainId::Arbitrum.to_string(), "Arbitrum (42161)");
    }

    #[test]
    fn chain_id_all_returns_five() {
        assert_eq!(ChainId::all().len(), 5);
    }
}
