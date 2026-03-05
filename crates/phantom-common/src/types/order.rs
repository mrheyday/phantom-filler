//! Order types for Dutch auction intents.

use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

use super::chain::ChainId;

/// Unique identifier for an order, derived from its hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(pub FixedBytes<32>);

impl OrderId {
    /// Creates a new OrderId from a 32-byte hash.
    pub fn new(hash: FixedBytes<32>) -> Self {
        Self(hash)
    }

    /// Returns the inner bytes.
    pub fn as_bytes(&self) -> &FixedBytes<32> {
        &self.0
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

/// Lifecycle status of an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// Order discovered, not yet validated.
    Pending,
    /// Order validated and available for filling.
    Active,
    /// Order has been fully filled.
    Filled,
    /// Order has expired past its deadline.
    Expired,
    /// Order was cancelled by the signer.
    Cancelled,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Active => write!(f, "Active"),
            Self::Filled => write!(f, "Filled"),
            Self::Expired => write!(f, "Expired"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// Input token specification for an order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderInput {
    /// Input token contract address.
    pub token: Address,
    /// Maximum amount the swapper is willing to spend.
    pub amount: U256,
}

/// Output token specification with Dutch auction decay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderOutput {
    /// Output token contract address.
    pub token: Address,
    /// Starting (maximum) amount at decay start.
    pub start_amount: U256,
    /// Ending (minimum) amount at decay end.
    pub end_amount: U256,
    /// Recipient address for the output tokens.
    pub recipient: Address,
}

/// A Dutch auction order with decay parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DutchAuctionOrder {
    /// Unique order identifier.
    pub id: OrderId,
    /// Chain the order is on.
    pub chain_id: ChainId,
    /// Address of the reactor contract.
    pub reactor: Address,
    /// Address of the order signer (swapper).
    pub signer: Address,
    /// Order nonce for replay protection.
    pub nonce: U256,
    /// Timestamp when the decay begins.
    pub decay_start_time: DateTime<Utc>,
    /// Timestamp when the decay ends.
    pub decay_end_time: DateTime<Utc>,
    /// Deadline after which the order cannot be filled.
    pub deadline: DateTime<Utc>,
    /// Input token and amount.
    pub input: OrderInput,
    /// Output tokens with decay parameters.
    pub outputs: Vec<OrderOutput>,
}

impl DutchAuctionOrder {
    /// Returns the current required output amount based on decay progress.
    ///
    /// At `decay_start_time`, returns `start_amount`.
    /// At `decay_end_time`, returns `end_amount`.
    /// Between them, linearly interpolates.
    pub fn current_output_amount(&self, output_index: usize, now: DateTime<Utc>) -> Option<U256> {
        let output = self.outputs.get(output_index)?;

        if now <= self.decay_start_time {
            return Some(output.start_amount);
        }
        if now >= self.decay_end_time {
            return Some(output.end_amount);
        }

        let total_decay_duration = (self.decay_end_time - self.decay_start_time)
            .num_seconds()
            .unsigned_abs();
        let elapsed = (now - self.decay_start_time).num_seconds().unsigned_abs();

        if total_decay_duration == 0 {
            return Some(output.end_amount);
        }

        // Linear interpolation: start - (start - end) * elapsed / duration
        let diff = output.start_amount.saturating_sub(output.end_amount);
        let decay_amount = diff * U256::from(elapsed) / U256::from(total_decay_duration);
        Some(output.start_amount.saturating_sub(decay_amount))
    }

    /// Returns true if the order has expired.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now > self.deadline
    }
}

/// A signed order containing the order data and its signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOrder {
    /// The underlying Dutch auction order.
    pub order: DutchAuctionOrder,
    /// EIP-712 signature bytes.
    pub signature: Bytes,
    /// Current lifecycle status.
    pub status: OrderStatus,
    /// Timestamp when the order was first discovered.
    pub discovered_at: DateTime<Utc>,
}

impl SignedOrder {
    /// Creates a new signed order in Pending status.
    pub fn new(order: DutchAuctionOrder, signature: Bytes) -> Self {
        Self {
            order,
            signature,
            status: OrderStatus::Pending,
            discovered_at: Utc::now(),
        }
    }

    /// Returns the order ID.
    pub fn id(&self) -> OrderId {
        self.order.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, bytes, fixed_bytes};
    use chrono::Duration;

    fn sample_order() -> DutchAuctionOrder {
        let now = Utc::now();
        DutchAuctionOrder {
            id: OrderId::new(fixed_bytes!(
                "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
            )),
            chain_id: ChainId::Ethereum,
            reactor: address!("0x6000da47483062A0D734Ba3dc7576Ce6A0B645C4"),
            signer: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            nonce: U256::from(1u64),
            decay_start_time: now,
            decay_end_time: now + Duration::minutes(10),
            deadline: now + Duration::minutes(30),
            input: OrderInput {
                token: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                amount: U256::from(1_000_000_000u64), // 1000 USDC
            },
            outputs: vec![OrderOutput {
                token: address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                start_amount: U256::from(500_000_000_000_000_000u64), // 0.5 ETH
                end_amount: U256::from(450_000_000_000_000_000u64),   // 0.45 ETH
                recipient: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            }],
        }
    }

    #[test]
    fn order_id_display() {
        let id = OrderId::new(fixed_bytes!(
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
        ));
        let display = id.to_string();
        assert!(display.starts_with("0x"));
        assert_eq!(display.len(), 66); // 0x + 64 hex chars
    }

    #[test]
    fn order_status_serde_roundtrip() {
        let statuses = [
            OrderStatus::Pending,
            OrderStatus::Active,
            OrderStatus::Filled,
            OrderStatus::Expired,
            OrderStatus::Cancelled,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).expect("serialize");
            let deserialized: OrderStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*status, deserialized);
        }
    }

    #[test]
    fn dutch_auction_decay_at_start() {
        let order = sample_order();
        let amount = order
            .current_output_amount(0, order.decay_start_time)
            .expect("valid index");
        assert_eq!(amount, order.outputs[0].start_amount);
    }

    #[test]
    fn dutch_auction_decay_at_end() {
        let order = sample_order();
        let amount = order
            .current_output_amount(0, order.decay_end_time)
            .expect("valid index");
        assert_eq!(amount, order.outputs[0].end_amount);
    }

    #[test]
    fn dutch_auction_decay_midpoint() {
        let order = sample_order();
        let midpoint = order.decay_start_time + Duration::minutes(5);
        let amount = order
            .current_output_amount(0, midpoint)
            .expect("valid index");

        // Should be roughly halfway between start and end amounts
        let start = order.outputs[0].start_amount;
        let end = order.outputs[0].end_amount;
        assert!(amount < start);
        assert!(amount > end);
    }

    #[test]
    fn dutch_auction_decay_before_start() {
        let order = sample_order();
        let before = order.decay_start_time - Duration::minutes(5);
        let amount = order.current_output_amount(0, before).expect("valid index");
        assert_eq!(amount, order.outputs[0].start_amount);
    }

    #[test]
    fn dutch_auction_invalid_index() {
        let order = sample_order();
        assert!(order.current_output_amount(99, Utc::now()).is_none());
    }

    #[test]
    fn order_not_expired_before_deadline() {
        let order = sample_order();
        assert!(!order.is_expired(order.deadline - Duration::seconds(1)));
    }

    #[test]
    fn order_expired_after_deadline() {
        let order = sample_order();
        assert!(order.is_expired(order.deadline + Duration::seconds(1)));
    }

    #[test]
    fn signed_order_creation() {
        let order = sample_order();
        let signed = SignedOrder::new(order.clone(), bytes!("deadbeef"));
        assert_eq!(signed.status, OrderStatus::Pending);
        assert_eq!(signed.id(), order.id);
    }

    #[test]
    fn dutch_auction_order_serde_roundtrip() {
        let order = sample_order();
        let json = serde_json::to_string(&order).expect("serialize");
        let deserialized: DutchAuctionOrder = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(order, deserialized);
    }

    #[test]
    fn signed_order_serde_roundtrip() {
        let order = sample_order();
        let signed = SignedOrder::new(order, bytes!("aabbccdd"));
        let json = serde_json::to_string(&signed).expect("serialize");
        let deserialized: SignedOrder = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(signed.order, deserialized.order);
        assert_eq!(signed.signature, deserialized.signature);
        assert_eq!(signed.status, deserialized.status);
    }
}
