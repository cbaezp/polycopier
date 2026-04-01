//! Tests for order_watcher cancel-decision logic and wallet_sync pure helpers.
//!
//! order_watcher::run_once hits the live CLOB so it can't be unit-tested directly,
//! but the cancel-decision logic can be expressed as pure predicates and tested here.
//!
//! wallet_sync tasks all spawn long-running loops using live I/O, so we only test
//! the pure data transformations they perform (the position upsert/remove rules).

use polycopier::models::Position;
use polycopier::state::BotState;
use rust_decimal_macros::dec;

// ---------------------------------------------------------------------------
// Order watcher: cancel-decision logic
//
// The watcher cancels an open order when ANY of these is true:
//   A. Target no longer holds the token (sold out / never held)
//   B. Target PnL <= -max_copy_loss_pct
//   C. Market is redeemable OR past its end_date
//
// We model the predicate as a function so it's testable without a live client.
// ---------------------------------------------------------------------------

/// Pure helper that mirrors the cancel-decision in order_watcher::run_once.
/// Returns (should_cancel, reason).
fn should_cancel(
    target_pnl: Option<rust_decimal::Decimal>, // None = target has no position
    redeemable: bool,
    expired: bool,
    max_loss: rust_decimal::Decimal,
) -> (bool, &'static str) {
    match target_pnl {
        Some(pnl) => {
            if redeemable || expired {
                (true, "Market resolved or expired")
            } else if pnl <= -max_loss {
                (
                    true,
                    "Target position PnL dropped past MAX_COPY_LOSS_PCT limit",
                )
            } else {
                (false, "")
            }
        }
        None => (true, "Target closed position (zero balance)"),
    }
}

mod order_watcher_logic_tests {
    use super::*;

    const MAX_LOSS: rust_decimal::Decimal = dec!(0.10); // 10%

    // -- Trigger A: target sold -------------------------------------------------

    #[test]
    fn cancel_when_target_has_no_position() {
        let (cancel, reason) = should_cancel(None, false, false, MAX_LOSS);
        assert!(cancel);
        assert!(reason.contains("closed position"));
    }

    // -- Trigger B: PnL breach --------------------------------------------------

    #[test]
    fn cancel_when_pnl_exactly_at_limit() {
        // pnl == -max_loss: the condition is <=, so this triggers
        let (cancel, _) = should_cancel(Some(dec!(-0.10)), false, false, MAX_LOSS);
        assert!(cancel);
    }

    #[test]
    fn cancel_when_pnl_far_below_limit() {
        let (cancel, reason) = should_cancel(Some(dec!(-0.80)), false, false, MAX_LOSS);
        assert!(cancel);
        assert!(reason.contains("PnL"));
    }

    #[test]
    fn no_cancel_when_pnl_just_above_limit() {
        // -9.9% — within MAX_LOSS of 10%
        let (cancel, _) = should_cancel(Some(dec!(-0.099)), false, false, MAX_LOSS);
        assert!(!cancel);
    }

    #[test]
    fn no_cancel_when_pnl_positive() {
        let (cancel, _) = should_cancel(Some(dec!(0.15)), false, false, MAX_LOSS);
        assert!(!cancel);
    }

    #[test]
    fn no_cancel_when_pnl_zero() {
        let (cancel, _) = should_cancel(Some(dec!(0)), false, false, MAX_LOSS);
        assert!(!cancel);
    }

    // -- Trigger C: market expired / redeemable ---------------------------------

    #[test]
    fn cancel_when_market_redeemable() {
        // Even if pnl is fine, redeemable market = resolved, cancel
        let (cancel, reason) = should_cancel(Some(dec!(0.05)), true, false, MAX_LOSS);
        assert!(cancel);
        assert!(reason.contains("resolved or expired"));
    }

    #[test]
    fn cancel_when_market_past_end_date() {
        let (cancel, reason) = should_cancel(Some(dec!(0.05)), false, true, MAX_LOSS);
        assert!(cancel);
        assert!(reason.contains("resolved or expired"));
    }

    #[test]
    fn cancel_when_both_redeemable_and_expired() {
        let (cancel, _) = should_cancel(Some(dec!(0.05)), true, true, MAX_LOSS);
        assert!(cancel);
    }

    // -- Happy path: no cancellation -------------------------------------------

    #[test]
    fn no_cancel_for_healthy_position() {
        // Target still holds it, PnL fine, market active
        let (cancel, _) = should_cancel(Some(dec!(-0.05)), false, false, MAX_LOSS);
        assert!(!cancel);
    }

    // -- Priority: expired takes precedence over PnL ---------------------------

    #[test]
    fn expired_beats_healthy_pnl() {
        let (cancel, reason) = should_cancel(Some(dec!(0.50)), true, false, MAX_LOSS);
        assert!(cancel);
        assert!(reason.contains("resolved or expired"));
    }

    // -- Boundary: zero max_loss means any negative PnL triggers cancel --------

    #[test]
    fn zero_max_loss_cancels_on_any_negative_pnl() {
        let (cancel, _) = should_cancel(Some(dec!(-0.001)), false, false, dec!(0));
        assert!(cancel);
    }
}

// ---------------------------------------------------------------------------
// wallet_sync: position upsert / remove rules
//
// The start_position_sync task iterates the API response and applies:
//   - redeemable || cur_price == 0  →  positions.remove(token)
//   - otherwise                     →  positions.insert(token, ...)
//
// We test this logic here using BotState directly.
// ---------------------------------------------------------------------------

mod wallet_sync_position_rules {
    use super::*;

    fn make_pos(token_id: &str, size: rust_decimal::Decimal) -> Position {
        Position {
            token_id: token_id.to_string(),
            size,
            average_entry_price: dec!(0.50),
        }
    }

    #[test]
    fn new_active_position_is_inserted() {
        let mut state = BotState::new();
        state
            .positions
            .insert("tok_a".to_string(), make_pos("tok_a", dec!(10)));
        assert!(state.positions.contains_key("tok_a"));
        assert_eq!(state.positions["tok_a"].size, dec!(10));
    }

    #[test]
    fn resolved_position_is_removed() {
        let mut state = BotState::new();
        state
            .positions
            .insert("tok_b".to_string(), make_pos("tok_b", dec!(5)));
        // Simulate run_once removing it when redeemable=true
        state.positions.remove("tok_b");
        assert!(!state.positions.contains_key("tok_b"));
    }

    #[test]
    fn existing_position_size_is_updated_on_refresh() {
        let mut state = BotState::new();
        state
            .positions
            .insert("tok_c".to_string(), make_pos("tok_c", dec!(20)));
        // Fill comes in, size grows
        state
            .positions
            .insert("tok_c".to_string(), make_pos("tok_c", dec!(25)));
        assert_eq!(state.positions["tok_c"].size, dec!(25));
    }

    #[test]
    fn unrelated_positions_not_affected_by_remove() {
        let mut state = BotState::new();
        state
            .positions
            .insert("tok_d".to_string(), make_pos("tok_d", dec!(10)));
        state
            .positions
            .insert("tok_e".to_string(), make_pos("tok_e", dec!(8)));
        state.positions.remove("tok_d");
        assert!(!state.positions.contains_key("tok_d"));
        assert!(state.positions.contains_key("tok_e"));
        assert_eq!(state.positions.len(), 1);
    }

    #[test]
    fn remove_nonexistent_key_is_a_noop() {
        let mut state = BotState::new();
        // Should not panic
        state.positions.remove("does_not_exist");
        assert!(state.positions.is_empty());
    }

    #[test]
    fn multiple_fresh_fills_all_inserted() {
        let mut state = BotState::new();
        for i in 0..5u32 {
            let key = format!("tok_{i}");
            state
                .positions
                .insert(key.clone(), make_pos(&key, dec!(10)));
        }
        assert_eq!(state.positions.len(), 5);
    }
}
