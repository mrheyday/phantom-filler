//! In-memory order book with lifecycle management.
//!
//! Provides a concurrent order book backed by `DashMap` for fast, lock-free
//! access. Manages order lifecycle transitions (Pending → Active → Filled/Expired/Cancelled)
//! and supports periodic expiry cleanup.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use phantom_common::error::DiscoveryError;
use phantom_common::types::{DutchAuctionOrder, OrderId, OrderStatus};

/// An entry in the order book, combining order data with lifecycle metadata.
#[derive(Debug, Clone)]
pub struct OrderEntry {
    /// The decoded Dutch auction order.
    pub order: DutchAuctionOrder,
    /// Current lifecycle status.
    pub status: OrderStatus,
    /// Timestamp when the order was inserted.
    pub discovered_at: DateTime<Utc>,
    /// Timestamp of the last status change.
    pub updated_at: DateTime<Utc>,
}

/// Concurrent in-memory order book.
///
/// Thread-safe via `DashMap`; suitable for concurrent reads/writes from
/// multiple async tasks without external locking.
pub struct OrderBook {
    orders: DashMap<OrderId, OrderEntry>,
}

impl OrderBook {
    /// Creates an empty order book.
    pub fn new() -> Self {
        Self {
            orders: DashMap::new(),
        }
    }

    /// Inserts a new order in `Pending` status.
    ///
    /// Returns an error if an order with the same ID already exists.
    pub fn insert(&self, order: DutchAuctionOrder) -> Result<(), DiscoveryError> {
        let id = order.id;
        let now = Utc::now();

        let entry = OrderEntry {
            order,
            status: OrderStatus::Pending,
            discovered_at: now,
            updated_at: now,
        };

        if self.orders.contains_key(&id) {
            return Err(DiscoveryError::OrderAlreadyExists(id.to_string()));
        }

        self.orders.insert(id, entry);
        Ok(())
    }

    /// Returns a clone of the entry for the given order ID.
    pub fn get(&self, id: &OrderId) -> Option<OrderEntry> {
        self.orders.get(id).map(|r| r.clone())
    }

    /// Removes an order from the book, returning it if it existed.
    pub fn remove(&self, id: &OrderId) -> Option<OrderEntry> {
        self.orders.remove(id).map(|(_, v)| v)
    }

    /// Transitions an order from `Pending` to `Active`.
    pub fn activate(&self, id: &OrderId) -> Result<(), DiscoveryError> {
        self.transition(id, OrderStatus::Pending, OrderStatus::Active)
    }

    /// Transitions an order from `Active` to `Filled`.
    pub fn mark_filled(&self, id: &OrderId) -> Result<(), DiscoveryError> {
        self.transition(id, OrderStatus::Active, OrderStatus::Filled)
    }

    /// Transitions an order from `Active` to `Expired`.
    pub fn mark_expired(&self, id: &OrderId) -> Result<(), DiscoveryError> {
        self.transition(id, OrderStatus::Active, OrderStatus::Expired)
    }

    /// Transitions an order to `Cancelled` (from `Pending` or `Active`).
    pub fn mark_cancelled(&self, id: &OrderId) -> Result<(), DiscoveryError> {
        let mut entry = self
            .orders
            .get_mut(id)
            .ok_or_else(|| DiscoveryError::OrderNotFound(id.to_string()))?;

        match entry.status {
            OrderStatus::Pending | OrderStatus::Active => {
                entry.status = OrderStatus::Cancelled;
                entry.updated_at = Utc::now();
                Ok(())
            }
            other => Err(DiscoveryError::InvalidTransition {
                from: other.to_string(),
                to: "Cancelled".to_string(),
            }),
        }
    }

    /// Returns all orders with `Active` status.
    pub fn get_active_orders(&self) -> Vec<OrderEntry> {
        self.get_by_status(OrderStatus::Active)
    }

    /// Returns all orders matching the given status.
    pub fn get_by_status(&self, status: OrderStatus) -> Vec<OrderEntry> {
        self.orders
            .iter()
            .filter(|r| r.status == status)
            .map(|r| r.clone())
            .collect()
    }

    /// Scans active orders and marks any that have passed their deadline as `Expired`.
    ///
    /// Returns the number of orders that were expired.
    pub fn cleanup_expired(&self, now: DateTime<Utc>) -> usize {
        let mut expired_count = 0;

        for mut entry in self.orders.iter_mut() {
            if entry.status == OrderStatus::Active && entry.order.is_expired(now) {
                entry.status = OrderStatus::Expired;
                entry.updated_at = now;
                expired_count += 1;
            }
        }

        expired_count
    }

    /// Returns the total number of orders in the book.
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }

    /// Returns the number of active orders.
    pub fn active_count(&self) -> usize {
        self.orders
            .iter()
            .filter(|r| r.status == OrderStatus::Active)
            .count()
    }

    /// Returns the number of pending orders.
    pub fn pending_count(&self) -> usize {
        self.orders
            .iter()
            .filter(|r| r.status == OrderStatus::Pending)
            .count()
    }

    /// Performs a generic state transition with validation.
    fn transition(
        &self,
        id: &OrderId,
        expected_from: OrderStatus,
        to: OrderStatus,
    ) -> Result<(), DiscoveryError> {
        let mut entry = self
            .orders
            .get_mut(id)
            .ok_or_else(|| DiscoveryError::OrderNotFound(id.to_string()))?;

        if entry.status != expected_from {
            return Err(DiscoveryError::InvalidTransition {
                from: entry.status.to_string(),
                to: to.to_string(),
            });
        }

        entry.status = to;
        entry.updated_at = Utc::now();
        Ok(())
    }
}

impl Default for OrderBook {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, B256, U256};
    use chrono::Duration;
    use phantom_common::types::{OrderInput, OrderOutput};

    fn sample_order(id_byte: u8) -> DutchAuctionOrder {
        let now = Utc::now();
        DutchAuctionOrder {
            id: OrderId::new(B256::with_last_byte(id_byte)),
            chain_id: phantom_common::types::ChainId::Ethereum,
            reactor: address!("0x6000da47483062A0D734Ba3dc7576Ce6A0B645C4"),
            signer: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            nonce: U256::from(1u64),
            decay_start_time: now,
            decay_end_time: now + Duration::minutes(10),
            deadline: now + Duration::minutes(30),
            input: OrderInput {
                token: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                amount: U256::from(1_000_000u64),
            },
            outputs: vec![OrderOutput {
                token: address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                start_amount: U256::from(500_000u64),
                end_amount: U256::from(450_000u64),
                recipient: address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            }],
        }
    }

    #[test]
    fn insert_and_retrieve() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order.clone()).expect("insert");
        let entry = book.get(&id).expect("should exist");
        assert_eq!(entry.status, OrderStatus::Pending);
        assert_eq!(entry.order.id, id);
    }

    #[test]
    fn insert_duplicate_fails() {
        let book = OrderBook::new();
        let order = sample_order(0x01);

        book.insert(order.clone()).expect("first insert");
        let result = book.insert(order);
        assert!(result.is_err());
    }

    #[test]
    fn activate_pending_order() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order).expect("insert");
        book.activate(&id).expect("activate");

        let entry = book.get(&id).unwrap();
        assert_eq!(entry.status, OrderStatus::Active);
    }

    #[test]
    fn activate_non_pending_fails() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order).expect("insert");
        book.activate(&id).expect("activate");

        // Trying to activate an already-active order should fail.
        let result = book.activate(&id);
        assert!(result.is_err());
    }

    #[test]
    fn fill_active_order() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order).expect("insert");
        book.activate(&id).expect("activate");
        book.mark_filled(&id).expect("fill");

        let entry = book.get(&id).unwrap();
        assert_eq!(entry.status, OrderStatus::Filled);
    }

    #[test]
    fn fill_pending_order_fails() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order).expect("insert");
        let result = book.mark_filled(&id);
        assert!(result.is_err());
    }

    #[test]
    fn cancel_pending_order() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order).expect("insert");
        book.mark_cancelled(&id).expect("cancel");

        let entry = book.get(&id).unwrap();
        assert_eq!(entry.status, OrderStatus::Cancelled);
    }

    #[test]
    fn cancel_active_order() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order).expect("insert");
        book.activate(&id).expect("activate");
        book.mark_cancelled(&id).expect("cancel");

        let entry = book.get(&id).unwrap();
        assert_eq!(entry.status, OrderStatus::Cancelled);
    }

    #[test]
    fn cancel_filled_order_fails() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order).expect("insert");
        book.activate(&id).expect("activate");
        book.mark_filled(&id).expect("fill");

        let result = book.mark_cancelled(&id);
        assert!(result.is_err());
    }

    #[test]
    fn get_active_orders() {
        let book = OrderBook::new();

        for i in 0..5u8 {
            let order = sample_order(i);
            book.insert(order).expect("insert");
        }

        // Activate first 3.
        for i in 0..3u8 {
            let id = OrderId::new(B256::with_last_byte(i));
            book.activate(&id).expect("activate");
        }

        let active = book.get_active_orders();
        assert_eq!(active.len(), 3);
        assert_eq!(book.active_count(), 3);
        assert_eq!(book.pending_count(), 2);
    }

    #[test]
    fn get_by_status() {
        let book = OrderBook::new();

        let order1 = sample_order(0x01);
        let order2 = sample_order(0x02);
        let id1 = order1.id;

        book.insert(order1).expect("insert");
        book.insert(order2).expect("insert");
        book.activate(&id1).expect("activate");

        let pending = book.get_by_status(OrderStatus::Pending);
        let active = book.get_by_status(OrderStatus::Active);
        assert_eq!(pending.len(), 1);
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn cleanup_expired_orders() {
        let book = OrderBook::new();

        // Create an order that expires in 1 minute.
        let mut order = sample_order(0x01);
        order.deadline = Utc::now() + Duration::minutes(1);
        let id = order.id;

        book.insert(order).expect("insert");
        book.activate(&id).expect("activate");

        // No expiry at current time.
        let expired = book.cleanup_expired(Utc::now());
        assert_eq!(expired, 0);
        assert_eq!(book.active_count(), 1);

        // Expire by advancing time past deadline.
        let future = Utc::now() + Duration::minutes(5);
        let expired = book.cleanup_expired(future);
        assert_eq!(expired, 1);
        assert_eq!(book.active_count(), 0);

        let entry = book.get(&id).unwrap();
        assert_eq!(entry.status, OrderStatus::Expired);
    }

    #[test]
    fn cleanup_does_not_expire_pending() {
        let book = OrderBook::new();

        let mut order = sample_order(0x01);
        order.deadline = Utc::now() - Duration::minutes(1); // Already past deadline.
        book.insert(order).expect("insert");

        // Pending orders are NOT expired by cleanup (only Active orders are).
        let expired = book.cleanup_expired(Utc::now());
        assert_eq!(expired, 0);
    }

    #[test]
    fn remove_order() {
        let book = OrderBook::new();
        let order = sample_order(0x01);
        let id = order.id;

        book.insert(order).expect("insert");
        assert_eq!(book.order_count(), 1);

        let removed = book.remove(&id);
        assert!(removed.is_some());
        assert_eq!(book.order_count(), 0);
        assert!(book.get(&id).is_none());
    }

    #[test]
    fn order_not_found() {
        let book = OrderBook::new();
        let id = OrderId::new(B256::with_last_byte(0xFF));

        assert!(book.get(&id).is_none());
        assert!(book.activate(&id).is_err());
        assert!(book.mark_filled(&id).is_err());
        assert!(book.mark_cancelled(&id).is_err());
    }

    #[test]
    fn order_count_tracking() {
        let book = OrderBook::new();
        assert_eq!(book.order_count(), 0);

        for i in 0..10u8 {
            book.insert(sample_order(i)).expect("insert");
        }
        assert_eq!(book.order_count(), 10);
        assert_eq!(book.pending_count(), 10);
        assert_eq!(book.active_count(), 0);
    }

    #[test]
    fn default_creates_empty_book() {
        let book = OrderBook::default();
        assert_eq!(book.order_count(), 0);
    }
}
