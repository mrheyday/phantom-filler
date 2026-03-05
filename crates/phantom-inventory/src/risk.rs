//! Position limits and risk parameter enforcement.
//!
//! Validates proposed fills against configurable risk limits including
//! per-token position caps, single fill size limits, daily loss limits,
//! and concurrent pending fill counts.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

use alloy::primitives::U256;
use phantom_common::error::InventoryResult;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Configuration for risk management parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Maximum balance of any single token (in token's smallest unit).
    pub max_position_per_token: U256,
    /// Maximum value of a single fill transaction (in wei).
    pub max_single_fill_value: U256,
    /// Daily realized loss limit in wei. Fills are rejected if the
    /// cumulative daily loss exceeds this threshold.
    pub daily_loss_limit_wei: u64,
    /// Maximum number of concurrent pending (unconfirmed) fills.
    pub max_pending_fills: u32,
    /// Whether risk checks are enabled. When false, all checks pass.
    pub enabled: bool,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_per_token: U256::from(1_000_000_000_000_000_000_000u128), // 1000 tokens
            max_single_fill_value: U256::from(10_000_000_000_000_000_000u128),     // 10 ETH
            daily_loss_limit_wei: 1_000_000_000_000_000_000,                       // 1 ETH
            max_pending_fills: 16,
            enabled: true,
        }
    }
}

/// Outcome of a risk check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskCheckOutcome {
    /// Fill passes all risk checks.
    Passed,
    /// Fill was rejected by risk controls.
    Rejected,
}

/// Result of a risk evaluation for a proposed fill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskCheckResult {
    /// Whether the fill passed or was rejected.
    pub outcome: RiskCheckOutcome,
    /// Human-readable reason for the outcome.
    pub reason: String,
    /// Current daily loss in wei at time of check.
    pub daily_loss_wei: u64,
    /// Current pending fill count at time of check.
    pub pending_fills: u32,
}

impl RiskCheckResult {
    /// Returns true if the check passed.
    pub fn is_passed(&self) -> bool {
        self.outcome == RiskCheckOutcome::Passed
    }

    /// Returns true if the check was rejected.
    pub fn is_rejected(&self) -> bool {
        self.outcome == RiskCheckOutcome::Rejected
    }
}

/// Manages risk parameters and enforces position limits.
///
/// Uses atomic counters for lock-free concurrent access to daily loss
/// and pending fill tracking.
pub struct RiskManager {
    config: RiskConfig,
    /// Cumulative daily realized loss in wei (can be negative = profit).
    /// Stored as signed integer: positive = loss, negative = profit.
    daily_pnl_wei: AtomicI64,
    /// Number of currently pending (unconfirmed) fills.
    pending_fills: AtomicU32,
}

impl RiskManager {
    /// Creates a new risk manager with the given configuration.
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            daily_pnl_wei: AtomicI64::new(0),
            pending_fills: AtomicU32::new(0),
        }
    }

    /// Creates a risk manager with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(RiskConfig::default())
    }

    /// Returns a reference to the configuration.
    pub fn config(&self) -> &RiskConfig {
        &self.config
    }

    /// Evaluates whether a proposed fill passes risk checks.
    ///
    /// Checks (in order):
    /// 1. Risk controls enabled
    /// 2. Fill value within single fill limit
    /// 3. Token position within per-token limit
    /// 4. Daily loss within threshold
    /// 5. Pending fill count within limit
    pub fn check_fill(&self, fill_value: U256, current_position: U256) -> RiskCheckResult {
        let daily_loss = self.daily_loss_wei();
        let pending = self.pending_count();

        // If risk checks are disabled, always pass.
        if !self.config.enabled {
            return RiskCheckResult {
                outcome: RiskCheckOutcome::Passed,
                reason: "risk checks disabled".into(),
                daily_loss_wei: daily_loss,
                pending_fills: pending,
            };
        }

        // Check single fill value.
        if fill_value > self.config.max_single_fill_value {
            debug!(
                fill_value = %fill_value,
                max = %self.config.max_single_fill_value,
                "fill value exceeds single fill limit"
            );
            return RiskCheckResult {
                outcome: RiskCheckOutcome::Rejected,
                reason: format!(
                    "fill value {fill_value} exceeds max single fill {}",
                    self.config.max_single_fill_value
                ),
                daily_loss_wei: daily_loss,
                pending_fills: pending,
            };
        }

        // Check position limit: current + fill_value must stay within limit.
        let projected = current_position.saturating_add(fill_value);
        if projected > self.config.max_position_per_token {
            debug!(
                current = %current_position,
                fill = %fill_value,
                projected = %projected,
                max = %self.config.max_position_per_token,
                "projected position exceeds per-token limit"
            );
            return RiskCheckResult {
                outcome: RiskCheckOutcome::Rejected,
                reason: format!(
                    "projected position {projected} exceeds per-token limit {}",
                    self.config.max_position_per_token
                ),
                daily_loss_wei: daily_loss,
                pending_fills: pending,
            };
        }

        // Check daily loss limit.
        if daily_loss > self.config.daily_loss_limit_wei {
            warn!(
                daily_loss,
                limit = self.config.daily_loss_limit_wei,
                "daily loss limit exceeded"
            );
            return RiskCheckResult {
                outcome: RiskCheckOutcome::Rejected,
                reason: format!(
                    "daily loss {daily_loss} exceeds limit {}",
                    self.config.daily_loss_limit_wei
                ),
                daily_loss_wei: daily_loss,
                pending_fills: pending,
            };
        }

        // Check pending fills limit.
        if pending >= self.config.max_pending_fills {
            debug!(
                pending,
                max = self.config.max_pending_fills,
                "pending fill limit reached"
            );
            return RiskCheckResult {
                outcome: RiskCheckOutcome::Rejected,
                reason: format!(
                    "pending fills {pending} at limit {}",
                    self.config.max_pending_fills
                ),
                daily_loss_wei: daily_loss,
                pending_fills: pending,
            };
        }

        RiskCheckResult {
            outcome: RiskCheckOutcome::Passed,
            reason: "all risk checks passed".into(),
            daily_loss_wei: daily_loss,
            pending_fills: pending,
        }
    }

    /// Records a fill being submitted (increments pending count).
    pub fn record_fill_start(&self) -> InventoryResult<()> {
        let current = self.pending_fills.fetch_add(1, Ordering::SeqCst);
        debug!(pending = current + 1, "recorded fill start");
        Ok(())
    }

    /// Records a fill completing (decrements pending, updates P&L).
    ///
    /// `pnl_wei` is signed: positive = profit, negative = loss.
    pub fn record_fill_complete(&self, pnl_wei: i64) {
        self.pending_fills.fetch_sub(1, Ordering::SeqCst);

        // Daily PnL tracks net: subtract pnl since positive pnl = profit = less loss.
        let old = self.daily_pnl_wei.fetch_sub(pnl_wei, Ordering::SeqCst);
        let new_pnl = old - pnl_wei;

        debug!(
            pnl_wei,
            total_daily_pnl = new_pnl,
            "recorded fill completion"
        );
    }

    /// Returns the current daily loss in wei.
    ///
    /// Returns 0 if the day is net profitable (positive PnL).
    pub fn daily_loss_wei(&self) -> u64 {
        let pnl = self.daily_pnl_wei.load(Ordering::SeqCst);
        if pnl > 0 {
            pnl as u64 // positive daily_pnl means cumulative loss
        } else {
            0 // net profitable or break-even
        }
    }

    /// Returns the raw daily P&L in wei (can be negative for loss).
    pub fn daily_pnl_raw(&self) -> i64 {
        self.daily_pnl_wei.load(Ordering::SeqCst)
    }

    /// Returns the current number of pending fills.
    pub fn pending_count(&self) -> u32 {
        self.pending_fills.load(Ordering::SeqCst)
    }

    /// Resets the daily loss counter. Call at the start of each trading day.
    pub fn reset_daily(&self) {
        self.daily_pnl_wei.store(0, Ordering::SeqCst);
        info!("reset daily P&L counter");
    }

    /// Returns a snapshot of current risk metrics.
    pub fn risk_snapshot(&self) -> RiskSnapshot {
        RiskSnapshot {
            daily_loss_wei: self.daily_loss_wei(),
            daily_pnl_raw: self.daily_pnl_raw(),
            pending_fills: self.pending_count(),
            max_pending_fills: self.config.max_pending_fills,
            daily_loss_limit_wei: self.config.daily_loss_limit_wei,
            risk_enabled: self.config.enabled,
        }
    }
}

impl Default for RiskManager {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Point-in-time snapshot of risk metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSnapshot {
    /// Current daily loss in wei (0 if net profitable).
    pub daily_loss_wei: u64,
    /// Raw daily P&L (positive = loss, negative = profit).
    pub daily_pnl_raw: i64,
    /// Number of pending fills.
    pub pending_fills: u32,
    /// Maximum allowed pending fills.
    pub max_pending_fills: u32,
    /// Daily loss limit in wei.
    pub daily_loss_limit_wei: u64,
    /// Whether risk checks are enabled.
    pub risk_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> RiskConfig {
        RiskConfig {
            max_position_per_token: U256::from(1000u64),
            max_single_fill_value: U256::from(100u64),
            daily_loss_limit_wei: 500,
            max_pending_fills: 3,
            enabled: true,
        }
    }

    #[test]
    fn risk_config_default() {
        let config = RiskConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_pending_fills, 16);
        assert_eq!(config.daily_loss_limit_wei, 1_000_000_000_000_000_000);
    }

    #[test]
    fn risk_config_serde_roundtrip() {
        let config = default_config();
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: RiskConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.max_pending_fills, 3);
        assert_eq!(deserialized.daily_loss_limit_wei, 500);
    }

    #[test]
    fn check_fill_passes() {
        let manager = RiskManager::new(default_config());
        let result = manager.check_fill(U256::from(50u64), U256::from(100u64));
        assert!(result.is_passed());
        assert!(!result.is_rejected());
    }

    #[test]
    fn check_fill_value_too_large() {
        let manager = RiskManager::new(default_config());
        // max_single_fill_value is 100
        let result = manager.check_fill(U256::from(200u64), U256::ZERO);
        assert!(result.is_rejected());
        assert!(result.reason.contains("single fill"));
    }

    #[test]
    fn check_fill_position_limit() {
        let manager = RiskManager::new(default_config());
        // current_position=900, fill=200 → 1100 > max_position_per_token=1000
        let result = manager.check_fill(U256::from(90u64), U256::from(950u64));
        assert!(result.is_rejected());
        assert!(result.reason.contains("per-token limit"));
    }

    #[test]
    fn check_fill_daily_loss_limit() {
        let manager = RiskManager::new(default_config());
        // Simulate losses exceeding daily limit.
        manager.record_fill_start().expect("start");
        manager.record_fill_complete(-600); // loss of 600 > limit of 500

        let result = manager.check_fill(U256::from(10u64), U256::ZERO);
        assert!(result.is_rejected());
        assert!(result.reason.contains("daily loss"));
    }

    #[test]
    fn check_fill_pending_limit() {
        let manager = RiskManager::new(default_config());
        // Fill up pending slots (max 3).
        for _ in 0..3 {
            manager.record_fill_start().expect("start");
        }

        let result = manager.check_fill(U256::from(10u64), U256::ZERO);
        assert!(result.is_rejected());
        assert!(result.reason.contains("pending fills"));
    }

    #[test]
    fn check_fill_disabled() {
        let config = RiskConfig {
            enabled: false,
            ..default_config()
        };
        let manager = RiskManager::new(config);

        // Even with huge fill value, should pass when disabled.
        let result = manager.check_fill(U256::MAX, U256::MAX);
        assert!(result.is_passed());
        assert!(result.reason.contains("disabled"));
    }

    #[test]
    fn record_fill_lifecycle() {
        let manager = RiskManager::new(default_config());

        assert_eq!(manager.pending_count(), 0);

        manager.record_fill_start().expect("start");
        assert_eq!(manager.pending_count(), 1);

        manager.record_fill_start().expect("start");
        assert_eq!(manager.pending_count(), 2);

        manager.record_fill_complete(100); // profit of 100
        assert_eq!(manager.pending_count(), 1);

        manager.record_fill_complete(-50); // loss of 50
        assert_eq!(manager.pending_count(), 0);
    }

    #[test]
    fn daily_pnl_tracking() {
        let manager = RiskManager::new(default_config());

        // Profit of 100.
        manager.record_fill_start().expect("start");
        manager.record_fill_complete(100);
        assert_eq!(manager.daily_pnl_raw(), -100); // stored as negative = profit
        assert_eq!(manager.daily_loss_wei(), 0); // net profitable

        // Loss of 200 → net loss of 100.
        manager.record_fill_start().expect("start");
        manager.record_fill_complete(-200);
        assert_eq!(manager.daily_pnl_raw(), 100); // net loss of 100
        assert_eq!(manager.daily_loss_wei(), 100);
    }

    #[test]
    fn reset_daily() {
        let manager = RiskManager::new(default_config());

        manager.record_fill_start().expect("start");
        manager.record_fill_complete(-300);
        assert!(manager.daily_loss_wei() > 0);

        manager.reset_daily();
        assert_eq!(manager.daily_loss_wei(), 0);
        assert_eq!(manager.daily_pnl_raw(), 0);
    }

    #[test]
    fn risk_snapshot() {
        let manager = RiskManager::new(default_config());
        manager.record_fill_start().expect("start");

        let snapshot = manager.risk_snapshot();
        assert_eq!(snapshot.pending_fills, 1);
        assert_eq!(snapshot.max_pending_fills, 3);
        assert_eq!(snapshot.daily_loss_limit_wei, 500);
        assert!(snapshot.risk_enabled);
    }

    #[test]
    fn risk_snapshot_serde_roundtrip() {
        let snapshot = RiskSnapshot {
            daily_loss_wei: 100,
            daily_pnl_raw: 100,
            pending_fills: 2,
            max_pending_fills: 16,
            daily_loss_limit_wei: 1000,
            risk_enabled: true,
        };
        let json = serde_json::to_string(&snapshot).expect("serialize");
        let deserialized: RiskSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.daily_loss_wei, 100);
        assert_eq!(deserialized.pending_fills, 2);
    }

    #[test]
    fn risk_check_outcome_serde() {
        let outcomes = [RiskCheckOutcome::Passed, RiskCheckOutcome::Rejected];
        for outcome in &outcomes {
            let json = serde_json::to_string(outcome).expect("serialize");
            let deserialized: RiskCheckOutcome = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*outcome, deserialized);
        }
    }

    #[test]
    fn risk_check_result_serde_roundtrip() {
        let result = RiskCheckResult {
            outcome: RiskCheckOutcome::Passed,
            reason: "all checks passed".into(),
            daily_loss_wei: 0,
            pending_fills: 1,
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let deserialized: RiskCheckResult = serde_json::from_str(&json).expect("deserialize");
        assert!(deserialized.is_passed());
    }

    #[test]
    fn fill_exactly_at_position_limit() {
        let manager = RiskManager::new(default_config());
        // current=900, fill=100 → exactly 1000 = limit. Should pass.
        let result = manager.check_fill(U256::from(100u64), U256::from(900u64));
        assert!(result.is_passed());
    }

    #[test]
    fn fill_exactly_at_single_fill_limit() {
        let manager = RiskManager::new(default_config());
        // max_single_fill_value = 100. Fill of exactly 100 should pass.
        let result = manager.check_fill(U256::from(100u64), U256::ZERO);
        assert!(result.is_passed());
    }

    #[test]
    fn default_risk_manager() {
        let manager = RiskManager::default();
        assert!(manager.config().enabled);
        assert_eq!(manager.pending_count(), 0);
        assert_eq!(manager.daily_loss_wei(), 0);
    }
}
