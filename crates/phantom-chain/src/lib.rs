//! Multi-chain connector layer for RPC/WebSocket connections and block streaming.

pub mod events;
pub mod mempool;
pub mod provider;
pub mod stream;

pub use events::{ChainEvent, EventFilterConfig, EventSubscription, EventSubscriptionManager};
pub use mempool::{MempoolConfig, MempoolMonitor, PendingTransaction};
pub use provider::ProviderManager;
pub use stream::{BlockNotification, BlockStreamer, BlockStreamerConfig};
