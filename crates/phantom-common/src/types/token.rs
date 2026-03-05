//! Token and token amount types.

use alloy::primitives::{Address, U256};
use serde::{Deserialize, Serialize};
use std::fmt;

use super::chain::ChainId;

/// Represents an ERC-20 token on a specific chain.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Token {
    /// Contract address of the token.
    pub address: Address,
    /// Chain the token resides on.
    pub chain_id: ChainId,
    /// Number of decimal places.
    pub decimals: u8,
    /// Human-readable symbol (e.g., "USDC", "WETH").
    pub symbol: String,
}

impl Token {
    /// Creates a new token.
    pub fn new(
        address: Address,
        chain_id: ChainId,
        decimals: u8,
        symbol: impl Into<String>,
    ) -> Self {
        Self {
            address,
            chain_id,
            decimals,
            symbol: symbol.into(),
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} on {})", self.symbol, self.address, self.chain_id)
    }
}

/// A token paired with an amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAmount {
    /// The token.
    pub token: Token,
    /// Raw amount in the token's smallest unit.
    pub amount: U256,
}

impl TokenAmount {
    /// Creates a new token amount.
    pub fn new(token: Token, amount: U256) -> Self {
        Self { token, amount }
    }

    /// Returns true if the amount is zero.
    pub fn is_zero(&self) -> bool {
        self.amount.is_zero()
    }
}

impl fmt::Display for TokenAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.amount, self.token.symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn sample_token() -> Token {
        Token::new(
            address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            ChainId::Ethereum,
            6,
            "USDC",
        )
    }

    #[test]
    fn token_creation() {
        let token = sample_token();
        assert_eq!(token.symbol, "USDC");
        assert_eq!(token.decimals, 6);
        assert_eq!(token.chain_id, ChainId::Ethereum);
    }

    #[test]
    fn token_serde_roundtrip() {
        let token = sample_token();
        let json = serde_json::to_string(&token).expect("serialize");
        let deserialized: Token = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(token, deserialized);
    }

    #[test]
    fn token_amount_creation() {
        let token = sample_token();
        let amount = TokenAmount::new(token, U256::from(1_000_000u64));
        assert!(!amount.is_zero());
    }

    #[test]
    fn token_amount_zero() {
        let token = sample_token();
        let amount = TokenAmount::new(token, U256::ZERO);
        assert!(amount.is_zero());
    }

    #[test]
    fn token_amount_serde_roundtrip() {
        let token = sample_token();
        let amount = TokenAmount::new(token, U256::from(5_000_000u64));
        let json = serde_json::to_string(&amount).expect("serialize");
        let deserialized: TokenAmount = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(amount, deserialized);
    }

    #[test]
    fn token_display() {
        let token = sample_token();
        let display = token.to_string();
        assert!(display.contains("USDC"));
        assert!(display.contains("Ethereum"));
    }

    #[test]
    fn token_amount_display() {
        let token = sample_token();
        let amount = TokenAmount::new(token, U256::from(1_000_000u64));
        let display = amount.to_string();
        assert!(display.contains("USDC"));
    }
}
