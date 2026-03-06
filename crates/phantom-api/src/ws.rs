//! WebSocket feed for real-time event streaming.
//!
//! Provides a broadcast-based event bus and a WebSocket handler that
//! streams events to connected clients as JSON messages.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

// ─── Event Types ────────────────────────────────────────────────────

/// Events broadcast to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    /// A new fill has been recorded.
    FillRecorded {
        fill_id: String,
        chain_id: u64,
        pnl_wei: i64,
    },
    /// A fill has been confirmed on-chain.
    FillConfirmed { fill_id: String, tx_hash: String },
    /// A fill has been reverted.
    FillReverted { fill_id: String },
    /// A component health status changed.
    HealthChanged {
        component: String,
        status: String,
        message: String,
    },
    /// P&L summary update.
    PnlUpdate {
        total_pnl_wei: i64,
        total_fills: u64,
    },
    /// Server heartbeat (sent periodically to keep connections alive).
    Heartbeat {
        /// Server uptime in seconds.
        uptime_secs: u64,
        /// Number of connected clients.
        connected_clients: u64,
    },
}

// ─── Event Bus ──────────────────────────────────────────────────────

/// Configuration for the event bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBusConfig {
    /// Maximum number of events buffered in the broadcast channel.
    pub channel_capacity: usize,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 1024,
        }
    }
}

/// Central event bus for publishing and subscribing to real-time events.
///
/// Uses a `tokio::sync::broadcast` channel for efficient fan-out to
/// multiple WebSocket clients.
pub struct EventBus {
    sender: broadcast::Sender<WsEvent>,
    /// Number of currently connected WebSocket clients.
    connected_clients: AtomicU64,
}

impl EventBus {
    /// Creates a new event bus with the given capacity.
    pub fn new(config: EventBusConfig) -> Self {
        let (sender, _) = broadcast::channel(config.channel_capacity);
        Self {
            sender,
            connected_clients: AtomicU64::new(0),
        }
    }

    /// Creates an event bus with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(EventBusConfig::default())
    }

    /// Publishes an event to all connected subscribers.
    ///
    /// Returns the number of subscribers that received the event.
    /// Returns 0 if there are no subscribers (events are dropped).
    pub fn publish(&self, event: WsEvent) -> usize {
        self.sender.send(event).unwrap_or_default()
    }

    /// Creates a new receiver for subscribing to events.
    pub fn subscribe(&self) -> broadcast::Receiver<WsEvent> {
        self.sender.subscribe()
    }

    /// Returns the number of currently connected clients.
    pub fn connected_clients(&self) -> u64 {
        self.connected_clients.load(Ordering::Relaxed)
    }

    /// Increments the connected client count.
    fn client_connected(&self) {
        self.connected_clients.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrements the connected client count.
    fn client_disconnected(&self) {
        self.connected_clients.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ─── WebSocket Handler ──────────────────────────────────────────────

/// State required for WebSocket connections.
#[derive(Clone)]
pub struct WsState {
    /// Shared event bus.
    pub event_bus: Arc<EventBus>,
}

/// WebSocket upgrade handler.
///
/// `GET /ws` — upgrades the HTTP connection to a WebSocket and streams
/// events from the event bus.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<WsState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state.event_bus))
}

/// Handles an individual WebSocket connection.
///
/// Subscribes to the event bus and forwards events as JSON text messages.
/// The connection is closed when the client disconnects or an error occurs.
async fn handle_socket(mut socket: WebSocket, event_bus: Arc<EventBus>) {
    event_bus.client_connected();
    let client_count = event_bus.connected_clients();
    info!(clients = client_count, "websocket client connected");

    let mut rx = event_bus.subscribe();

    loop {
        tokio::select! {
            // Forward events from the bus to the client.
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        match serde_json::to_string(&event) {
                            Ok(json) => {
                                if socket.send(Message::Text(json.into())).await.is_err() {
                                    break; // Client disconnected.
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to serialize ws event");
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!(missed = n, "websocket client lagged, skipping events");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break; // Bus shut down.
                    }
                }
            }
            // Handle incoming messages from the client (ping/pong, close).
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {} // Ignore other messages.
                    Some(Err(_)) => break, // Connection error.
                }
            }
        }
    }

    event_bus.client_disconnected();
    let client_count = event_bus.connected_clients();
    info!(clients = client_count, "websocket client disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_bus_default() {
        let bus = EventBus::with_defaults();
        assert_eq!(bus.connected_clients(), 0);
    }

    #[test]
    fn event_bus_publish_no_subscribers() {
        let bus = EventBus::with_defaults();
        let count = bus.publish(WsEvent::Heartbeat {
            uptime_secs: 0,
            connected_clients: 0,
        });
        assert_eq!(count, 0);
    }

    #[test]
    fn event_bus_publish_with_subscriber() {
        let bus = EventBus::with_defaults();
        let mut rx = bus.subscribe();
        let count = bus.publish(WsEvent::Heartbeat {
            uptime_secs: 10,
            connected_clients: 1,
        });
        assert_eq!(count, 1);

        let event = rx.try_recv().expect("should receive event");
        match event {
            WsEvent::Heartbeat { uptime_secs, .. } => assert_eq!(uptime_secs, 10),
            _ => panic!("unexpected event type"),
        }
    }

    #[test]
    fn event_bus_multiple_subscribers() {
        let bus = EventBus::with_defaults();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        let mut rx3 = bus.subscribe();

        let count = bus.publish(WsEvent::FillReverted {
            fill_id: "f1".to_string(),
        });
        assert_eq!(count, 3);

        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
        assert!(rx3.try_recv().is_ok());
    }

    #[test]
    fn event_bus_client_count() {
        let bus = EventBus::with_defaults();
        assert_eq!(bus.connected_clients(), 0);

        bus.client_connected();
        assert_eq!(bus.connected_clients(), 1);

        bus.client_connected();
        assert_eq!(bus.connected_clients(), 2);

        bus.client_disconnected();
        assert_eq!(bus.connected_clients(), 1);
    }

    #[test]
    fn ws_event_serde_fill_recorded() {
        let event = WsEvent::FillRecorded {
            fill_id: "fill-1".to_string(),
            chain_id: 1,
            pnl_wei: 1000,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"fill_recorded\""));
        assert!(json.contains("\"fill_id\":\"fill-1\""));

        let deserialized: WsEvent = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            WsEvent::FillRecorded {
                fill_id, chain_id, ..
            } => {
                assert_eq!(fill_id, "fill-1");
                assert_eq!(chain_id, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ws_event_serde_fill_confirmed() {
        let event = WsEvent::FillConfirmed {
            fill_id: "fill-2".to_string(),
            tx_hash: "0xabc".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"fill_confirmed\""));

        let deserialized: WsEvent = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            WsEvent::FillConfirmed { tx_hash, .. } => assert_eq!(tx_hash, "0xabc"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ws_event_serde_fill_reverted() {
        let event = WsEvent::FillReverted {
            fill_id: "fill-3".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"fill_reverted\""));
    }

    #[test]
    fn ws_event_serde_health_changed() {
        let event = WsEvent::HealthChanged {
            component: "rpc".to_string(),
            status: "unhealthy".to_string(),
            message: "timeout".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"health_changed\""));
        assert!(json.contains("\"component\":\"rpc\""));
    }

    #[test]
    fn ws_event_serde_pnl_update() {
        let event = WsEvent::PnlUpdate {
            total_pnl_wei: -500,
            total_fills: 42,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"pnl_update\""));
        assert!(json.contains("\"total_pnl_wei\":-500"));
    }

    #[test]
    fn ws_event_serde_heartbeat() {
        let event = WsEvent::Heartbeat {
            uptime_secs: 3600,
            connected_clients: 5,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"type\":\"heartbeat\""));
    }

    #[test]
    fn event_bus_config_default() {
        let config = EventBusConfig::default();
        assert_eq!(config.channel_capacity, 1024);
    }

    #[test]
    fn event_bus_config_serde_roundtrip() {
        let config = EventBusConfig {
            channel_capacity: 2048,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: EventBusConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.channel_capacity, 2048);
    }

    #[test]
    fn event_bus_custom_capacity() {
        let config = EventBusConfig {
            channel_capacity: 64,
        };
        let bus = EventBus::new(config);
        let _rx = bus.subscribe();

        // Fill the buffer.
        for i in 0..64 {
            bus.publish(WsEvent::Heartbeat {
                uptime_secs: i,
                connected_clients: 0,
            });
        }
    }

    #[test]
    fn ws_state_is_clone() {
        let state = WsState {
            event_bus: Arc::new(EventBus::with_defaults()),
        };
        let _cloned = state.clone();
    }

    #[tokio::test]
    async fn event_bus_subscribe_receives_async() {
        let bus = Arc::new(EventBus::with_defaults());
        let mut rx = bus.subscribe();

        let bus_clone = Arc::clone(&bus);
        tokio::spawn(async move {
            bus_clone.publish(WsEvent::FillReverted {
                fill_id: "async-fill".to_string(),
            });
        });

        let event = rx.recv().await.expect("should receive");
        match event {
            WsEvent::FillReverted { fill_id } => assert_eq!(fill_id, "async-fill"),
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn dropped_subscriber_doesnt_block() {
        let bus = EventBus::with_defaults();
        let rx = bus.subscribe();
        drop(rx);

        // Publishing should still work, just returns 0.
        let count = bus.publish(WsEvent::Heartbeat {
            uptime_secs: 0,
            connected_clients: 0,
        });
        assert_eq!(count, 0);
    }

    #[test]
    fn concurrent_publish() {
        use std::sync::Arc;
        use std::thread;

        let bus = Arc::new(EventBus::with_defaults());
        let mut rx = bus.subscribe();

        let mut handles = vec![];
        for i in 0..10 {
            let bus = Arc::clone(&bus);
            handles.push(thread::spawn(move || {
                bus.publish(WsEvent::Heartbeat {
                    uptime_secs: i,
                    connected_clients: 0,
                });
            }));
        }

        for h in handles {
            h.join().expect("thread panicked");
        }

        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 10);
    }
}
