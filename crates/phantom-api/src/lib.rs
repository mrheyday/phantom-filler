//! REST API server and WebSocket feeds for the Phantom Filler engine.
//!
//! Provides a configurable HTTP server with health checks, P&L reporting,
//! fill history, risk exposure, and system status endpoints.

pub mod config;
pub mod error;
pub mod handlers;
pub mod server;
pub mod state;
pub mod ws;
