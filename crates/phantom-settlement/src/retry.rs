//! Revert handling and retry logic for failed fill transactions.
//!
//! Provides configurable retry policies with exponential backoff,
//! revert reason classification, and retry budget tracking.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use phantom_common::error::{SettlementError, SettlementResult};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Configuration for retry behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retry attempts per transaction.
    pub max_retries: u32,
    /// Initial delay before first retry in milliseconds.
    pub initial_delay_ms: u64,
    /// Maximum delay between retries in milliseconds.
    pub max_delay_ms: u64,
    /// Backoff multiplier (e.g., 2.0 for doubling).
    pub backoff_multiplier: f64,
    /// Global retry budget — max concurrent retries across all transactions.
    pub global_retry_budget: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 1_000,
            max_delay_ms: 30_000,
            backoff_multiplier: 2.0,
            global_retry_budget: 10,
        }
    }
}

/// Classification of revert reasons to guide retry decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevertReason {
    /// Transaction ran out of gas.
    OutOfGas,
    /// Nonce was already used (nonce too low).
    NonceTooLow,
    /// Nonce is ahead of the expected value.
    NonceTooHigh,
    /// Insufficient funds for gas or value.
    InsufficientFunds,
    /// The order was already filled by another filler.
    AlreadyFilled,
    /// The order has expired (deadline passed).
    OrderExpired,
    /// Slippage exceeded — price moved unfavorably.
    SlippageExceeded,
    /// Generic execution revert with custom message.
    ExecutionReverted(String),
    /// Unknown or unclassifiable revert.
    Unknown(String),
}

impl RevertReason {
    /// Returns true if this revert reason is retryable.
    ///
    /// Some reverts (like already filled or expired) are permanent
    /// and should not be retried.
    pub fn is_retryable(&self) -> bool {
        match self {
            RevertReason::OutOfGas => true,
            RevertReason::NonceTooLow => true,
            RevertReason::NonceTooHigh => true,
            RevertReason::InsufficientFunds => false,
            RevertReason::AlreadyFilled => false,
            RevertReason::OrderExpired => false,
            RevertReason::SlippageExceeded => true,
            RevertReason::ExecutionReverted(_) => true,
            RevertReason::Unknown(_) => true,
        }
    }

    /// Returns a human-readable category for metrics/logging.
    pub fn category(&self) -> &str {
        match self {
            RevertReason::OutOfGas => "out_of_gas",
            RevertReason::NonceTooLow => "nonce_too_low",
            RevertReason::NonceTooHigh => "nonce_too_high",
            RevertReason::InsufficientFunds => "insufficient_funds",
            RevertReason::AlreadyFilled => "already_filled",
            RevertReason::OrderExpired => "order_expired",
            RevertReason::SlippageExceeded => "slippage_exceeded",
            RevertReason::ExecutionReverted(_) => "execution_reverted",
            RevertReason::Unknown(_) => "unknown",
        }
    }
}

/// Classifies a revert error message into a structured reason.
pub fn classify_revert(error_message: &str) -> RevertReason {
    let lower = error_message.to_lowercase();

    if lower.contains("out of gas") || lower.contains("gas required exceeds") {
        RevertReason::OutOfGas
    } else if lower.contains("nonce too low") {
        RevertReason::NonceTooLow
    } else if lower.contains("nonce too high") {
        RevertReason::NonceTooHigh
    } else if lower.contains("insufficient funds") || lower.contains("insufficient balance") {
        RevertReason::InsufficientFunds
    } else if lower.contains("already filled") || lower.contains("order already") {
        RevertReason::AlreadyFilled
    } else if lower.contains("expired") || lower.contains("deadline") {
        RevertReason::OrderExpired
    } else if lower.contains("slippage") || lower.contains("price moved") {
        RevertReason::SlippageExceeded
    } else if lower.contains("revert") || lower.contains("execution reverted") {
        RevertReason::ExecutionReverted(error_message.to_string())
    } else {
        RevertReason::Unknown(error_message.to_string())
    }
}

/// Outcome of a retry decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDecision {
    /// Should retry after the specified delay.
    Retry,
    /// Should not retry (permanent failure or budget exhausted).
    Abandon,
}

/// Result of evaluating whether to retry a failed transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryEvaluation {
    /// Whether to retry or abandon.
    pub decision: RetryDecision,
    /// Delay before retrying (zero if abandoning).
    pub delay: Duration,
    /// Current attempt number (1-based).
    pub attempt: u32,
    /// Maximum attempts allowed.
    pub max_attempts: u32,
    /// The classified revert reason.
    pub reason: RevertReason,
}

impl RetryEvaluation {
    /// Returns true if the decision is to retry.
    pub fn should_retry(&self) -> bool {
        self.decision == RetryDecision::Retry
    }
}

/// Manages retry logic for failed fill transactions.
///
/// Tracks a global retry budget to prevent retry storms and computes
/// exponential backoff delays.
pub struct RetryManager {
    config: RetryConfig,
    /// Active retries consuming the global budget.
    active_retries: AtomicU32,
}

impl RetryManager {
    /// Creates a new retry manager with the given configuration.
    pub fn new(config: RetryConfig) -> Self {
        Self {
            config,
            active_retries: AtomicU32::new(0),
        }
    }

    /// Creates a retry manager with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(RetryConfig::default())
    }

    /// Returns a reference to the configuration.
    pub fn config(&self) -> &RetryConfig {
        &self.config
    }

    /// Evaluates whether a failed transaction should be retried.
    ///
    /// Considers:
    /// - Whether the revert reason is retryable
    /// - Current attempt vs max retries
    /// - Global retry budget availability
    pub fn evaluate_retry(&self, error_message: &str, attempt: u32) -> RetryEvaluation {
        let reason = classify_revert(error_message);

        // Non-retryable reverts are immediately abandoned.
        if !reason.is_retryable() {
            info!(
                reason = reason.category(),
                attempt, "non-retryable revert, abandoning"
            );
            return RetryEvaluation {
                decision: RetryDecision::Abandon,
                delay: Duration::ZERO,
                attempt,
                max_attempts: self.config.max_retries,
                reason,
            };
        }

        // Check attempt limit.
        if attempt >= self.config.max_retries {
            warn!(
                attempt,
                max = self.config.max_retries,
                reason = reason.category(),
                "max retries reached, abandoning"
            );
            return RetryEvaluation {
                decision: RetryDecision::Abandon,
                delay: Duration::ZERO,
                attempt,
                max_attempts: self.config.max_retries,
                reason,
            };
        }

        // Check global budget.
        let active = self.active_retries.load(Ordering::SeqCst);
        if active >= self.config.global_retry_budget {
            warn!(
                active,
                budget = self.config.global_retry_budget,
                "global retry budget exhausted, abandoning"
            );
            return RetryEvaluation {
                decision: RetryDecision::Abandon,
                delay: Duration::ZERO,
                attempt,
                max_attempts: self.config.max_retries,
                reason,
            };
        }

        // Compute backoff delay.
        let delay = self.compute_delay(attempt);

        debug!(
            attempt,
            delay_ms = delay.as_millis() as u64,
            reason = reason.category(),
            "scheduling retry"
        );

        RetryEvaluation {
            decision: RetryDecision::Retry,
            delay,
            attempt,
            max_attempts: self.config.max_retries,
            reason,
        }
    }

    /// Acquires a retry slot from the global budget.
    ///
    /// Call before starting a retry. Returns an error if the budget
    /// is exhausted.
    pub fn acquire_retry_slot(&self) -> SettlementResult<()> {
        let current = self.active_retries.load(Ordering::SeqCst);
        if current >= self.config.global_retry_budget {
            return Err(SettlementError::ConfirmationFailed(
                "global retry budget exhausted".into(),
            ));
        }
        self.active_retries.fetch_add(1, Ordering::SeqCst);
        debug!(
            active = current + 1,
            budget = self.config.global_retry_budget,
            "acquired retry slot"
        );
        Ok(())
    }

    /// Releases a retry slot back to the global budget.
    ///
    /// Call after a retry completes (success or final failure).
    pub fn release_retry_slot(&self) {
        let prev =
            self.active_retries
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                    Some(current.saturating_sub(1))
                });
        if let Ok(prev_val) = prev {
            debug!(active = prev_val.saturating_sub(1), "released retry slot");
        }
    }

    /// Returns the number of active retries.
    pub fn active_retries(&self) -> u32 {
        self.active_retries.load(Ordering::SeqCst)
    }

    /// Computes the backoff delay for the given attempt number.
    fn compute_delay(&self, attempt: u32) -> Duration {
        let base = self.config.initial_delay_ms as f64;
        let multiplier = self.config.backoff_multiplier.powi(attempt as i32);
        let delay_ms = (base * multiplier) as u64;
        let capped = delay_ms.min(self.config.max_delay_ms);
        Duration::from_millis(capped)
    }
}

impl Default for RetryManager {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_delay_ms, 1_000);
        assert_eq!(config.max_delay_ms, 30_000);
        assert_eq!(config.global_retry_budget, 10);
    }

    #[test]
    fn retry_config_serde_roundtrip() {
        let config = RetryConfig {
            max_retries: 5,
            initial_delay_ms: 500,
            max_delay_ms: 10_000,
            backoff_multiplier: 1.5,
            global_retry_budget: 20,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: RetryConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.max_retries, 5);
        assert_eq!(deserialized.global_retry_budget, 20);
    }

    #[test]
    fn classify_out_of_gas() {
        let reason = classify_revert("Transaction out of gas");
        assert_eq!(reason, RevertReason::OutOfGas);
        assert!(reason.is_retryable());
        assert_eq!(reason.category(), "out_of_gas");
    }

    #[test]
    fn classify_nonce_too_low() {
        let reason = classify_revert("nonce too low: next nonce 5, got 3");
        assert_eq!(reason, RevertReason::NonceTooLow);
        assert!(reason.is_retryable());
    }

    #[test]
    fn classify_insufficient_funds() {
        let reason = classify_revert("insufficient funds for transfer");
        assert_eq!(reason, RevertReason::InsufficientFunds);
        assert!(!reason.is_retryable());
    }

    #[test]
    fn classify_already_filled() {
        let reason = classify_revert("order already filled");
        assert_eq!(reason, RevertReason::AlreadyFilled);
        assert!(!reason.is_retryable());
    }

    #[test]
    fn classify_order_expired() {
        let reason = classify_revert("order expired: deadline passed");
        assert_eq!(reason, RevertReason::OrderExpired);
        assert!(!reason.is_retryable());
    }

    #[test]
    fn classify_slippage() {
        let reason = classify_revert("slippage tolerance exceeded");
        assert_eq!(reason, RevertReason::SlippageExceeded);
        assert!(reason.is_retryable());
    }

    #[test]
    fn classify_execution_reverted() {
        let reason = classify_revert("execution reverted: custom error");
        assert!(matches!(reason, RevertReason::ExecutionReverted(_)));
        assert!(reason.is_retryable());
    }

    #[test]
    fn classify_unknown() {
        let reason = classify_revert("something completely unexpected");
        assert!(matches!(reason, RevertReason::Unknown(_)));
        assert!(reason.is_retryable());
    }

    #[test]
    fn evaluate_retry_retryable() {
        let manager = RetryManager::with_defaults();
        let eval = manager.evaluate_retry("out of gas", 0);
        assert!(eval.should_retry());
        assert!(eval.delay.as_millis() > 0);
        assert_eq!(eval.attempt, 0);
    }

    #[test]
    fn evaluate_retry_non_retryable() {
        let manager = RetryManager::with_defaults();
        let eval = manager.evaluate_retry("order already filled", 0);
        assert!(!eval.should_retry());
        assert_eq!(eval.decision, RetryDecision::Abandon);
    }

    #[test]
    fn evaluate_retry_max_attempts() {
        let manager = RetryManager::with_defaults();
        // max_retries is 3, attempt 3 should be abandoned.
        let eval = manager.evaluate_retry("out of gas", 3);
        assert!(!eval.should_retry());
    }

    #[test]
    fn evaluate_retry_budget_exhausted() {
        let config = RetryConfig {
            global_retry_budget: 1,
            ..RetryConfig::default()
        };
        let manager = RetryManager::new(config);

        // Fill the budget.
        manager.acquire_retry_slot().expect("acquire");
        assert_eq!(manager.active_retries(), 1);

        let eval = manager.evaluate_retry("out of gas", 0);
        assert!(!eval.should_retry());
    }

    #[test]
    fn acquire_and_release_slots() {
        let config = RetryConfig {
            global_retry_budget: 2,
            ..RetryConfig::default()
        };
        let manager = RetryManager::new(config);

        manager.acquire_retry_slot().expect("first");
        manager.acquire_retry_slot().expect("second");
        assert_eq!(manager.active_retries(), 2);

        // Third should fail.
        assert!(manager.acquire_retry_slot().is_err());

        manager.release_retry_slot();
        assert_eq!(manager.active_retries(), 1);

        // Now should succeed.
        manager.acquire_retry_slot().expect("third after release");
    }

    #[test]
    fn release_slot_saturates_at_zero() {
        let manager = RetryManager::with_defaults();
        // Releasing when none are active should not underflow.
        manager.release_retry_slot();
        assert_eq!(manager.active_retries(), 0);
    }

    #[test]
    fn exponential_backoff() {
        let config = RetryConfig {
            initial_delay_ms: 1_000,
            backoff_multiplier: 2.0,
            max_delay_ms: 30_000,
            ..RetryConfig::default()
        };
        let manager = RetryManager::new(config);

        let d0 = manager.compute_delay(0);
        let d1 = manager.compute_delay(1);
        let d2 = manager.compute_delay(2);
        let d3 = manager.compute_delay(3);

        assert_eq!(d0.as_millis(), 1_000);
        assert_eq!(d1.as_millis(), 2_000);
        assert_eq!(d2.as_millis(), 4_000);
        assert_eq!(d3.as_millis(), 8_000);
    }

    #[test]
    fn backoff_capped_at_max() {
        let config = RetryConfig {
            initial_delay_ms: 10_000,
            backoff_multiplier: 10.0,
            max_delay_ms: 30_000,
            ..RetryConfig::default()
        };
        let manager = RetryManager::new(config);

        let d1 = manager.compute_delay(1); // 10_000 * 10 = 100_000 → capped to 30_000
        assert_eq!(d1.as_millis(), 30_000);
    }

    #[test]
    fn revert_reason_serde_roundtrip() {
        let reasons = [
            RevertReason::OutOfGas,
            RevertReason::NonceTooLow,
            RevertReason::InsufficientFunds,
            RevertReason::AlreadyFilled,
            RevertReason::OrderExpired,
            RevertReason::SlippageExceeded,
            RevertReason::ExecutionReverted("custom".into()),
            RevertReason::Unknown("mystery".into()),
        ];
        for reason in &reasons {
            let json = serde_json::to_string(reason).expect("serialize");
            let deserialized: RevertReason = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*reason, deserialized);
        }
    }

    #[test]
    fn retry_decision_serde() {
        let decisions = [RetryDecision::Retry, RetryDecision::Abandon];
        for decision in &decisions {
            let json = serde_json::to_string(decision).expect("serialize");
            let deserialized: RetryDecision = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*decision, deserialized);
        }
    }

    #[test]
    fn retry_evaluation_serde_roundtrip() {
        let eval = RetryEvaluation {
            decision: RetryDecision::Retry,
            delay: Duration::from_millis(2000),
            attempt: 1,
            max_attempts: 3,
            reason: RevertReason::OutOfGas,
        };
        let json = serde_json::to_string(&eval).expect("serialize");
        let deserialized: RetryEvaluation = serde_json::from_str(&json).expect("deserialize");
        assert!(deserialized.should_retry());
        assert_eq!(deserialized.attempt, 1);
    }

    #[test]
    fn default_retry_manager() {
        let manager = RetryManager::default();
        assert_eq!(manager.active_retries(), 0);
        assert_eq!(manager.config().max_retries, 3);
    }
}
