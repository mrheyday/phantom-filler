//! Reactor event monitoring and order decoding.
//!
//! Watches UniswapX reactor contracts for Fill events and decodes
//! Dutch auction order parameters from event logs.

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Log;
use alloy::sol_types::SolEvent;
use chrono::{DateTime, TimeZone, Utc};
use phantom_common::error::DiscoveryError;
use phantom_common::types::{ChainId, DutchAuctionOrder, OrderId, OrderInput, OrderOutput};

use crate::abi::{self, IReactor};

/// A decoded Fill event from a reactor contract.
#[derive(Debug, Clone)]
pub struct DecodedFillEvent {
    /// The order hash.
    pub order_hash: B256,
    /// The filler address.
    pub filler: Address,
    /// The swapper address.
    pub swapper: Address,
    /// The order nonce.
    pub nonce: U256,
    /// Block number where the event was emitted.
    pub block_number: u64,
    /// Transaction hash that emitted the event.
    pub tx_hash: Option<B256>,
    /// Chain this event occurred on.
    pub chain_id: ChainId,
}

/// Decodes a Fill event from a raw log entry.
///
/// Returns `None` if the log doesn't match the Fill event signature.
pub fn decode_fill_event(log: &Log, chain_id: ChainId) -> Result<DecodedFillEvent, DiscoveryError> {
    // Verify we have the right event signature in topic0.
    let topics = &log.topics();
    if topics.is_empty() || topics[0] != IReactor::Fill::SIGNATURE_HASH {
        return Err(DiscoveryError::DecodingFailed {
            reason: "log does not match Fill event signature".to_string(),
        });
    }

    // Decode the event from raw log data.
    let fill = IReactor::Fill::decode_raw_log(log.topics().iter().copied(), &log.data().data)
        .map_err(|e| DiscoveryError::DecodingFailed {
            reason: format!("failed to decode Fill event: {e}"),
        })?;

    Ok(DecodedFillEvent {
        order_hash: fill.orderHash,
        filler: fill.filler,
        swapper: fill.swapper,
        nonce: fill.nonce,
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash,
        chain_id,
    })
}

/// Configuration for a reactor monitor.
#[derive(Debug, Clone)]
pub struct ReactorMonitorConfig {
    /// Reactor contract address to monitor.
    pub reactor_address: Address,
    /// Chain ID for this reactor.
    pub chain_id: ChainId,
}

impl ReactorMonitorConfig {
    /// Creates a config for a specific reactor on a chain.
    pub fn new(reactor_address: Address, chain_id: ChainId) -> Self {
        Self {
            reactor_address,
            chain_id,
        }
    }

    /// Creates a config using the well-known reactor address for a chain.
    pub fn for_chain(chain_id: ChainId) -> Option<Self> {
        abi::addresses::reactor_for_chain(chain_id.as_u64()).map(|addr| Self {
            reactor_address: addr,
            chain_id,
        })
    }
}

/// Converts a Unix timestamp (seconds) to a `DateTime<Utc>`.
fn timestamp_to_datetime(timestamp: U256) -> DateTime<Utc> {
    let secs: i64 = timestamp.try_into().unwrap_or(i64::MAX);
    Utc.timestamp_opt(secs, 0).single().unwrap_or_default()
}

/// Decodes an ExclusiveDutchOrder from raw ABI-encoded bytes into our domain type.
pub fn decode_dutch_order(
    encoded_order: &[u8],
    chain_id: ChainId,
    order_hash: B256,
) -> Result<DutchAuctionOrder, DiscoveryError> {
    let order = <abi::ExclusiveDutchOrder as alloy::sol_types::SolType>::abi_decode(encoded_order)
        .map_err(|e| DiscoveryError::DecodingFailed {
            reason: format!("failed to decode ExclusiveDutchOrder: {e}"),
        })?;

    let outputs = order
        .outputs
        .iter()
        .map(|o| OrderOutput {
            token: o.token,
            start_amount: o.startAmount,
            end_amount: o.endAmount,
            recipient: o.recipient,
        })
        .collect();

    Ok(DutchAuctionOrder {
        id: OrderId::new(order_hash),
        chain_id,
        reactor: order.info.reactor,
        signer: order.info.swapper,
        nonce: order.info.nonce,
        decay_start_time: timestamp_to_datetime(order.decayStartTime),
        decay_end_time: timestamp_to_datetime(order.decayEndTime),
        deadline: timestamp_to_datetime(order.info.deadline),
        input: OrderInput {
            token: order.input.token,
            amount: order.input.startAmount,
        },
        outputs,
    })
}

/// Validates basic order constraints.
pub fn validate_order(order: &DutchAuctionOrder) -> Result<(), DiscoveryError> {
    // Check deadline is in the future.
    let now = Utc::now();
    if order.is_expired(now) {
        return Err(DiscoveryError::ValidationFailed {
            reason: "order has expired".to_string(),
        });
    }

    // Check decay times are ordered.
    if order.decay_end_time < order.decay_start_time {
        return Err(DiscoveryError::ValidationFailed {
            reason: "decay end time is before decay start time".to_string(),
        });
    }

    // Check there's at least one output.
    if order.outputs.is_empty() {
        return Err(DiscoveryError::ValidationFailed {
            reason: "order has no outputs".to_string(),
        });
    }

    // Check output amounts are valid (start >= end for Dutch auction).
    for (i, output) in order.outputs.iter().enumerate() {
        if output.start_amount < output.end_amount {
            return Err(DiscoveryError::ValidationFailed {
                reason: format!(
                    "output {i} start_amount < end_amount (invalid Dutch auction decay)"
                ),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, keccak256, Bytes, LogData};
    use alloy::sol_types::SolType;
    use chrono::Duration;

    fn sample_dutch_order() -> DutchAuctionOrder {
        let now = Utc::now();
        DutchAuctionOrder {
            id: OrderId::new(B256::ZERO),
            chain_id: ChainId::Ethereum,
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
    fn validate_valid_order() {
        let order = sample_dutch_order();
        assert!(validate_order(&order).is_ok());
    }

    #[test]
    fn validate_expired_order() {
        let mut order = sample_dutch_order();
        order.deadline = Utc::now() - Duration::minutes(1);
        let result = validate_order(&order);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DiscoveryError::ValidationFailed { .. }
        ));
    }

    #[test]
    fn validate_no_outputs() {
        let mut order = sample_dutch_order();
        order.outputs.clear();
        assert!(validate_order(&order).is_err());
    }

    #[test]
    fn validate_invalid_decay() {
        let mut order = sample_dutch_order();
        // start_amount < end_amount is invalid for a Dutch auction.
        order.outputs[0].start_amount = U256::from(100u64);
        order.outputs[0].end_amount = U256::from(200u64);
        assert!(validate_order(&order).is_err());
    }

    #[test]
    fn validate_reversed_decay_times() {
        let mut order = sample_dutch_order();
        std::mem::swap(&mut order.decay_start_time, &mut order.decay_end_time);
        assert!(validate_order(&order).is_err());
    }

    #[test]
    fn decode_fill_event_valid() {
        // Build a Fill event log manually.
        let order_hash = B256::with_last_byte(0x42);
        let filler = address!("0x1111111111111111111111111111111111111111");
        let swapper = address!("0x2222222222222222222222222222222222222222");
        let nonce = U256::from(99u64);

        let fill = IReactor::Fill {
            orderHash: order_hash,
            filler,
            swapper,
            nonce,
        };

        let log_data = fill.encode_log_data();
        let inner = alloy::primitives::Log {
            address: Address::ZERO,
            data: LogData::new(log_data.topics().to_vec(), log_data.data.clone())
                .expect("valid log data"),
        };
        let log = Log {
            inner,
            ..Default::default()
        };

        let decoded = decode_fill_event(&log, ChainId::Ethereum).expect("decode");
        assert_eq!(decoded.order_hash, order_hash);
        assert_eq!(decoded.filler, filler);
        assert_eq!(decoded.swapper, swapper);
        assert_eq!(decoded.nonce, nonce);
        assert_eq!(decoded.chain_id, ChainId::Ethereum);
    }

    #[test]
    fn decode_fill_event_wrong_signature() {
        let inner = alloy::primitives::Log {
            address: Address::ZERO,
            data: LogData::new(vec![B256::ZERO], Bytes::new()).expect("valid"),
        };
        let log = Log {
            inner,
            ..Default::default()
        };

        let result = decode_fill_event(&log, ChainId::Ethereum);
        assert!(result.is_err());
    }

    #[test]
    fn decode_dutch_order_roundtrip() {
        let now_ts = Utc::now().timestamp() as u64;
        let encoded_order = abi::ExclusiveDutchOrder {
            info: abi::OrderInfo {
                reactor: Address::with_last_byte(0x01),
                swapper: Address::with_last_byte(0x02),
                nonce: U256::from(1),
                deadline: U256::from(now_ts + 3600),
                additionalValidationContract: Address::ZERO,
                additionalValidationData: Bytes::new(),
            },
            decayStartTime: U256::from(now_ts),
            decayEndTime: U256::from(now_ts + 600),
            exclusiveFiller: Address::ZERO,
            exclusivityOverrideBps: U256::ZERO,
            input: abi::DutchInput {
                token: Address::with_last_byte(0xAA),
                startAmount: U256::from(1000u64),
                endAmount: U256::from(1000u64),
            },
            outputs: vec![abi::DutchOutput {
                token: Address::with_last_byte(0xBB),
                startAmount: U256::from(500u64),
                endAmount: U256::from(450u64),
                recipient: Address::with_last_byte(0x02),
            }],
        };

        let bytes = <abi::ExclusiveDutchOrder as SolType>::abi_encode(&encoded_order);
        let order_hash = keccak256(&bytes);

        let decoded = decode_dutch_order(&bytes, ChainId::Ethereum, order_hash).expect("decode");

        assert_eq!(decoded.id, OrderId::new(order_hash));
        assert_eq!(decoded.chain_id, ChainId::Ethereum);
        assert_eq!(decoded.reactor, Address::with_last_byte(0x01));
        assert_eq!(decoded.signer, Address::with_last_byte(0x02));
        assert_eq!(decoded.outputs.len(), 1);
        assert_eq!(decoded.outputs[0].start_amount, U256::from(500u64));
        assert_eq!(decoded.outputs[0].end_amount, U256::from(450u64));
    }

    #[test]
    fn reactor_monitor_config_for_chain() {
        let config = ReactorMonitorConfig::for_chain(ChainId::Ethereum);
        assert!(config.is_some());
        let config = config.unwrap();
        assert_eq!(config.chain_id, ChainId::Ethereum);
        assert_eq!(
            config.reactor_address,
            abi::addresses::EXCLUSIVE_DUTCH_ORDER_REACTOR_MAINNET
        );
    }

    #[test]
    fn reactor_monitor_config_unknown_chain() {
        let config = ReactorMonitorConfig::for_chain(ChainId::Optimism);
        assert!(config.is_none());
    }

    #[test]
    fn timestamp_to_datetime_valid() {
        let dt = timestamp_to_datetime(U256::from(1_700_000_000u64));
        assert_eq!(dt.timestamp(), 1_700_000_000);
    }
}
