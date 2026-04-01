//! Integration tests for polycopier.
//!
//! All tests run without network access. Business logic is tested via
//! pure functions extracted from each module, plus async strategy-engine
//! tests using a mock OrderSubmitter.

use polycopier::config::SizingMode;
use polycopier::models::{
    EvaluatedTrade, OrderRequest, Position, ScanStatus, TargetPosition, TradeEvent, TradeSide,
};
use polycopier::position_scanner::classify_position;
use polycopier::risk::RiskEngine;
use polycopier::state::BotState;
use polycopier::strategy::{calculate_entry_size, calculate_limit_price};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashSet;

// -- Shared test helpers -------------------------------------------------------

fn test_config() -> polycopier::config::Config {
    polycopier::config::Config {
        private_key: "0xdeadbeef".to_string(),
        funder_address: "0x1111111111111111111111111111111111111111".to_string(),
        chain_id: 137,
        target_wallets: vec!["0xabc".to_string()],
        max_slippage_pct: dec!(0.02),
        max_trade_size_usd: dec!(10.00),
        max_delay_seconds: 2,
        max_copy_loss_pct: dec!(0.40),
        max_copy_gain_pct: dec!(0.05),
        min_entry_price: dec!(0.02),
        max_entry_price: dec!(0.999),
        sizing_mode: SizingMode::Fixed,
        copy_size_pct: None,
    }
}

fn make_trade(taker: &str, price: Decimal, size: Decimal, side: TradeSide) -> TradeEvent {
    make_trade_for_token(taker, "99999", price, size, side)
}

fn make_trade_for_token(
    taker: &str,
    token_id: &str,
    price: Decimal,
    size: Decimal,
    side: TradeSide,
) -> TradeEvent {
    TradeEvent {
        transaction_hash: "0xtest".to_string(),
        maker_address: taker.to_string(),
        taker_address: taker.to_string(),
        token_id: token_id.to_string(),
        price,
        size,
        side,
        timestamp: chrono::Utc::now().timestamp(),
    }
}

fn make_eval(validated: bool) -> EvaluatedTrade {
    EvaluatedTrade {
        original_event: make_trade("0xabc", dec!(0.50), dec!(10), TradeSide::BUY),
        validated,
        reason: if validated {
            None
        } else {
            Some("test skip".to_string())
        },
    }
}

fn empty_set() -> HashSet<String> {
    HashSet::new()
}

fn token_set(tokens: &[&str]) -> HashSet<String> {
    tokens.iter().map(|s| s.to_string()).collect()
}

// -- Config: placeholder detection ---------------------------------------------

mod config_tests {
    use polycopier::config::is_placeholder;

    #[test]
    fn placeholder_dot() {
        assert!(is_placeholder("."));
    }

    #[test]
    fn placeholder_your_prefix() {
        assert!(is_placeholder("your-private-key-here"));
    }

    #[test]
    fn placeholder_0x_your() {
        assert!(is_placeholder("0xYourWalletAddressHere"));
    }

    #[test]
    fn placeholder_short_numeric_is_valid() {
        // Short numeric values like "2" or "10" must NOT be flagged as placeholders
        // (the old v.len() < 3 check incorrectly blocked these).
        assert!(!is_placeholder("2"));
        assert!(!is_placeholder("10"));
        assert!(!is_placeholder("ab")); // ambiguous short string - now accepted
    }

    #[test]
    fn placeholder_empty_string() {
        assert!(is_placeholder(""));
    }

    #[test]
    fn real_private_key_not_placeholder() {
        assert!(!is_placeholder(
            "0x3621b4d29ca05a9e4a670eb069c7df1113917f95280cd14041f8f783f4ab233d"
        ));
    }

    #[test]
    fn real_address_not_placeholder() {
        assert!(!is_placeholder(
            "0x5239883317CF1dd037a61D6A3b3F6A7Dd85c8dC9"
        ));
    }

    #[test]
    fn wallet_address_not_placeholder() {
        assert!(!is_placeholder(
            "0xfcbecc7e5186e88e03445b81f593685d62828f44"
        ));
    }
}

// -- Models: ScanStatus helpers ------------------------------------------------

mod model_tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn scan_status_sort_key_order_is_correct() {
        assert!(ScanStatus::Monitoring.sort_key() < ScanStatus::Entered.sort_key());
        assert!(ScanStatus::Entered.sort_key() < ScanStatus::SkippedOwned.sort_key());
        assert!(ScanStatus::SkippedOwned.sort_key() < ScanStatus::SkippedLoss.sort_key());
        assert!(ScanStatus::SkippedLoss.sort_key() < ScanStatus::SkippedPrice.sort_key());
    }

    #[test]
    fn all_scan_statuses_have_non_empty_labels() {
        let statuses: &[ScanStatus] = &[
            ScanStatus::Monitoring,
            ScanStatus::Entered,
            ScanStatus::SkippedOwned,
            ScanStatus::SkippedLoss,
            ScanStatus::SkippedPrice,
        ];
        for s in statuses {
            assert!(!s.label().is_empty(), "Empty label for {:?}", s);
        }
    }

    #[test]
    fn monitoring_color_is_green() {
        assert_eq!(ScanStatus::Monitoring.color(), Color::Green);
    }

    #[test]
    fn skipped_loss_color_is_red() {
        assert_eq!(ScanStatus::SkippedLoss.color(), Color::Red);
    }

    #[test]
    fn entered_color_is_cyan() {
        assert_eq!(ScanStatus::Entered.color(), Color::Cyan);
    }

    #[test]
    fn skipped_owned_color_is_magenta() {
        assert_eq!(ScanStatus::SkippedOwned.color(), Color::Magenta);
    }

    #[test]
    fn skipped_price_color_is_dark_gray() {
        assert_eq!(ScanStatus::SkippedPrice.color(), Color::DarkGray);
    }
}

// -- State: BotState operations ------------------------------------------------

mod state_tests {
    use super::*;

    #[test]
    fn new_state_has_zero_balance() {
        let state = BotState::new();
        assert_eq!(state.total_balance, Decimal::ZERO);
    }

    #[test]
    fn new_state_has_empty_feed() {
        let state = BotState::new();
        assert!(state.live_feed.is_empty());
    }

    #[test]
    fn validated_trade_increments_copies_not_skips() {
        let mut state = BotState::new();
        state.push_evaluated_trade(make_eval(true));
        assert_eq!(state.copies_executed, 1);
        assert_eq!(state.trades_skipped, 0);
    }

    #[test]
    fn invalid_trade_increments_skips_not_copies() {
        let mut state = BotState::new();
        state.push_evaluated_trade(make_eval(false));
        assert_eq!(state.copies_executed, 0);
        assert_eq!(state.trades_skipped, 1);
    }

    #[test]
    fn feed_most_recent_is_at_front() {
        let mut state = BotState::new();
        state.push_evaluated_trade(make_eval(true));
        state.push_evaluated_trade(make_eval(false)); // most recent = skipped
        assert!(!state.live_feed.front().unwrap().validated);
    }

    #[test]
    fn feed_capped_at_100_entries() {
        let mut state = BotState::new();
        for _ in 0..110 {
            state.push_evaluated_trade(make_eval(true));
        }
        assert_eq!(state.live_feed.len(), 100);
    }

    #[test]
    fn counters_accumulate_correctly() {
        let mut state = BotState::new();
        for _ in 0..5 {
            state.push_evaluated_trade(make_eval(true));
        }
        for _ in 0..3 {
            state.push_evaluated_trade(make_eval(false));
        }
        assert_eq!(state.copies_executed, 5);
        assert_eq!(state.trades_skipped, 3);
    }

    #[test]
    fn target_positions_default_is_empty() {
        let state = BotState::new();
        assert!(state.target_positions.is_empty());
    }

    // -- positions HashMap (new: SELL position lookup) --------------------------

    #[test]
    fn position_can_be_inserted_and_retrieved() {
        let mut state = BotState::new();
        state.positions.insert(
            "tok1".to_string(),
            Position {
                token_id: "tok1".to_string(),
                size: dec!(20),
                average_entry_price: dec!(0.50),
            },
        );
        let pos = state.positions.get("tok1").expect("position should exist");
        assert_eq!(pos.size, dec!(20));
        assert_eq!(pos.average_entry_price, dec!(0.50));
    }

    #[test]
    fn position_size_can_be_updated_in_place() {
        let mut state = BotState::new();
        state.positions.insert(
            "tok2".to_string(),
            Position {
                token_id: "tok2".to_string(),
                size: dec!(10),
                average_entry_price: dec!(0.60),
            },
        );
        state.positions.get_mut("tok2").unwrap().size = dec!(8);
        assert_eq!(state.positions.get("tok2").unwrap().size, dec!(8));
    }

    #[test]
    fn missing_token_returns_none() {
        let state = BotState::new();
        assert!(!state.positions.contains_key("does_not_exist"));
    }

    #[test]
    fn multiple_positions_are_independent() {
        let mut state = BotState::new();
        state.positions.insert(
            "tokA".to_string(),
            Position {
                token_id: "tokA".to_string(),
                size: dec!(5),
                average_entry_price: dec!(0.20),
            },
        );
        state.positions.insert(
            "tokB".to_string(),
            Position {
                token_id: "tokB".to_string(),
                size: dec!(50),
                average_entry_price: dec!(0.75),
            },
        );
        assert_eq!(state.positions.get("tokA").unwrap().size, dec!(5));
        assert_eq!(state.positions.get("tokB").unwrap().size, dec!(50));
        assert_eq!(state.positions.len(), 2);
    }
}

// -- Risk engine ---------------------------------------------------------------

mod risk_tests {
    use super::*;

    fn engine() -> RiskEngine {
        RiskEngine::new(test_config())
    }

    #[test]
    fn valid_trade_passes() {
        let mut e = engine();
        assert!(e
            .check_trade(&make_trade("0xabc", dec!(0.50), dec!(10), TradeSide::BUY))
            .is_ok());
    }

    #[test]
    fn micro_trade_below_1_usd_is_rejected() {
        let mut e = engine();
        // 0.01 * $0.05 = $0.0005 - below $1 minimum
        let result = e.check_trade(&make_trade("0xabc", dec!(0.05), dec!(0.01), TradeSide::BUY));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_lowercase().contains("spoofing"));
    }

    #[test]
    fn trade_exactly_at_1_usd_passes() {
        let mut e = engine();
        // 2 * $0.50 = $1.00
        assert!(e
            .check_trade(&make_trade("0xabc", dec!(0.50), dec!(2), TradeSide::BUY))
            .is_ok());
    }

    #[test]
    fn sell_trade_also_subject_to_min_check() {
        let mut e = engine();
        // $0.01 * 0.5 = $0.005
        assert!(e
            .check_trade(&make_trade("0xabc", dec!(0.01), dec!(0.5), TradeSide::SELL))
            .is_err());
    }

    #[test]
    fn large_buy_passes_risk_check() {
        let mut e = engine();
        // Size-capping occurs in strategy.rs, not risk.rs
        assert!(e
            .check_trade(&make_trade("0xabc", dec!(0.80), dec!(1000), TradeSide::BUY))
            .is_ok());
    }

    #[test]
    fn trade_just_above_1_usd_passes() {
        let mut e = engine();
        // 3 shares * $0.34 = $1.02 - just above the $1 threshold
        assert!(e
            .check_trade(&make_trade("0xabc", dec!(0.34), dec!(3), TradeSide::BUY))
            .is_ok());
    }

    #[test]
    fn trade_just_below_1_usd_is_rejected() {
        let mut e = engine();
        // 1.9 shares * $0.50 = $0.95 - just under $1
        let result = e.check_trade(&make_trade("0xabc", dec!(0.50), dec!(1.9), TradeSide::BUY));
        assert!(result.is_err());
    }
}

// -- Strategy: pure price/size helpers ----------------------------------------

mod strategy_tests {
    use super::*;

    #[test]
    fn buy_price_adds_slippage() {
        assert_eq!(
            calculate_limit_price(dec!(0.50), TradeSide::BUY, dec!(0.02)),
            dec!(0.51)
        );
    }

    #[test]
    fn sell_price_subtracts_slippage() {
        assert_eq!(
            calculate_limit_price(dec!(0.50), TradeSide::SELL, dec!(0.02)),
            dec!(0.49)
        );
    }

    #[test]
    fn zero_slippage_leaves_price_unchanged_buy() {
        let price = dec!(0.73);
        assert_eq!(calculate_limit_price(price, TradeSide::BUY, dec!(0)), price);
    }

    #[test]
    fn zero_slippage_leaves_price_unchanged_sell() {
        let price = dec!(0.73);
        assert_eq!(
            calculate_limit_price(price, TradeSide::SELL, dec!(0)),
            price
        );
    }

    #[test]
    fn entry_size_within_budget_is_unchanged() {
        // 10 * $0.40 = $4 - under $10
        assert_eq!(
            calculate_entry_size(dec!(10), dec!(0.40), dec!(10)),
            dec!(10)
        );
    }

    #[test]
    fn entry_size_over_budget_is_capped() {
        // 100 * $0.40 = $40 - over $10 => 10/0.40 = 25
        assert_eq!(
            calculate_entry_size(dec!(100), dec!(0.40), dec!(10)),
            dec!(25)
        );
    }

    #[test]
    fn entry_size_at_exact_budget_passes_unchanged() {
        // 20 * $0.50 = $10 exactly
        assert_eq!(
            calculate_entry_size(dec!(20), dec!(0.50), dec!(10)),
            dec!(20)
        );
    }

    #[test]
    fn slippage_applied_to_high_price() {
        // $0.90 + 2% = $0.918, rounded to 2dp = $0.92
        assert_eq!(
            calculate_limit_price(dec!(0.90), TradeSide::BUY, dec!(0.02)),
            dec!(0.92)
        );
    }

    // -- BUY size minimum floor (new: smoke test $1 fix) -----------------------

    #[test]
    fn entry_size_targeting_1_10_stays_above_1_usd() {
        // $1.10 target / $0.52 = 2.115 -> rounds to 2.11, total = 2.11 * $0.52 = $1.0972 > $1 +
        let size = calculate_entry_size(dec!(2.115), dec!(0.52), dec!(10));
        let total = size * dec!(0.52);
        assert!(total >= dec!(1.00), "total ${total} should be >= $1.00");
    }

    #[test]
    fn entry_size_targeting_1_00_can_round_below_1_usd() {
        // Demonstrates WHY $1.05/$1.10 target is needed:
        // $1.00 / $0.52 = 1.923 -> rounds to 1.92 -> 1.92 * $0.52 = $0.9984 < $1
        // This is the bug we fixed - now we target $1.10 in the smoke test.
        let raw = dec!(1.00) / dec!(0.52); // = 1.923...
        let rounded = (raw * dec!(1)).round_dp(2); // = 1.92
        let total = rounded * dec!(0.52); // = 0.9984
        assert!(total < dec!(1.00), "demonstrates the rounding-below-$1 bug");
    }

    #[test]
    fn sell_size_fee_buffer_97_pct() {
        // Verifies that the 97% factor applied to SELL sizes gives the expected result
        let held_size = dec!(20);
        let fee_factor = Decimal::new(97, 2);
        let sell_size = (held_size * fee_factor).round_dp(2);
        assert_eq!(sell_size, dec!(19.40));
    }

    #[test]
    fn sell_size_fee_buffer_never_exceeds_held() {
        // 97% of held size is always < held size (can't oversell)
        let held_sizes = [dec!(5), dec!(10), dec!(20.5), dec!(100)];
        for held in &held_sizes {
            let sell_size = (held * Decimal::new(97, 2)).round_dp(2);
            assert!(
                sell_size < *held,
                "sell_size {sell_size} should be < held {held}"
            );
        }
    }
}

// -- Position scanner: classify_position --------------------------------------

mod scanner_tests {
    use super::*;

    const MIN: &str = "0.02";
    const MAX: &str = "0.95";
    const LOSS: &str = "0.40";
    const GAIN: &str = "0.05";

    fn min() -> Decimal {
        MIN.parse().unwrap()
    }
    fn max() -> Decimal {
        MAX.parse().unwrap()
    }
    fn loss() -> Decimal {
        LOSS.parse().unwrap()
    }
    fn gain() -> Decimal {
        GAIN.parse().unwrap()
    }

    fn classify(
        token: &str,
        price: Decimal,
        pnl: Decimal,
        owned: &HashSet<String>,
        queued: &HashSet<String>,
    ) -> ScanStatus {
        let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1)).date_naive();
        classify_position(
            token,
            price,
            pnl,
            false,
            Some(tomorrow),
            owned,
            queued,
            min(),
            max(),
            loss(),
            gain(),
        )
    }

    #[test]
    fn valid_position_is_monitoring() {
        assert_eq!(
            classify("t1", dec!(0.50), dec!(-0.10), &empty_set(), &empty_set()),
            ScanStatus::Monitoring
        );
    }

    #[test]
    fn already_owned_is_skipped_owned() {
        assert_eq!(
            classify(
                "t1",
                dec!(0.50),
                dec!(-0.10),
                &token_set(&["t1"]),
                &empty_set()
            ),
            ScanStatus::SkippedOwned
        );
    }

    #[test]
    fn already_queued_is_entered() {
        assert_eq!(
            classify(
                "t1",
                dec!(0.50),
                dec!(-0.10),
                &empty_set(),
                &token_set(&["t1"])
            ),
            ScanStatus::Entered
        );
    }

    #[test]
    fn price_below_min_is_range_skipped() {
        assert_eq!(
            classify("t1", dec!(0.01), dec!(-0.10), &empty_set(), &empty_set()),
            ScanStatus::SkippedPrice
        );
    }

    #[test]
    fn price_above_max_is_range_skipped() {
        assert_eq!(
            classify("t1", dec!(0.97), dec!(0.05), &empty_set(), &empty_set()),
            ScanStatus::SkippedPrice
        );
    }

    #[test]
    fn price_at_min_boundary_is_monitoring() {
        assert_eq!(
            classify("t1", dec!(0.02), dec!(-0.10), &empty_set(), &empty_set()),
            ScanStatus::Monitoring
        );
    }

    #[test]
    fn price_at_max_boundary_is_monitoring() {
        assert_eq!(
            classify("t1", dec!(0.95), dec!(-0.10), &empty_set(), &empty_set()),
            ScanStatus::Monitoring
        );
    }

    #[test]
    fn pnl_exactly_at_loss_threshold_is_monitoring() {
        // percent_pnl == -threshold: NOT less than, so passes
        assert_eq!(
            classify("t1", dec!(0.50), dec!(-0.40), &empty_set(), &empty_set()),
            ScanStatus::Monitoring
        );
    }

    #[test]
    fn pnl_one_tick_below_threshold_is_loss_skipped() {
        assert_eq!(
            classify("t1", dec!(0.50), dec!(-0.41), &empty_set(), &empty_set()),
            ScanStatus::SkippedLoss
        );
    }

    #[test]
    fn deeply_underwater_is_loss_skipped() {
        assert_eq!(
            classify("t1", dec!(0.10), dec!(-0.85), &empty_set(), &empty_set()),
            ScanStatus::SkippedLoss
        );
    }

    #[test]
    fn owned_takes_priority_over_price_range() {
        // Price is below min AND we own it - owned takes priority
        assert_eq!(
            classify(
                "t1",
                dec!(0.01),
                dec!(-0.10),
                &token_set(&["t1"]),
                &empty_set()
            ),
            ScanStatus::SkippedOwned
        );
    }

    #[test]
    fn queued_takes_priority_over_loss() {
        // Already queued AND deeply underwater - Entered wins
        assert_eq!(
            classify(
                "t1",
                dec!(0.50),
                dec!(-0.99),
                &empty_set(),
                &token_set(&["t1"])
            ),
            ScanStatus::Entered
        );
    }

    #[test]
    fn positive_pnl_below_gain_threshold_is_monitoring() {
        // +3% < 5% threshold -> Monitoring (already covered by positive_pnl_below_threshold above,
        // but kept as a direct replacement for the old positive_pnl_is_monitoring test)
        assert_eq!(
            classify("t1", dec!(0.60), dec!(0.03), &empty_set(), &empty_set()),
            ScanStatus::Monitoring
        );
    }

    #[test]
    fn positive_pnl_above_gain_threshold_is_skipped() {
        // +15% >> 5% threshold -> SkippedGain (old test used 0.15 and expected Monitoring -- now it's caught)
        assert_eq!(
            classify("t1", dec!(0.60), dec!(0.15), &empty_set(), &empty_set()),
            ScanStatus::SkippedGain
        );
    }

    #[test]
    fn zero_loss_threshold_rejects_any_negative_pnl() {
        let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1)).date_naive();
        let status = classify_position(
            "t1",
            dec!(0.50),
            dec!(-0.01),
            false,
            Some(tomorrow),
            &empty_set(),
            &empty_set(),
            min(),
            max(),
            dec!(0),
            gain(),
        );
        assert_eq!(status, ScanStatus::SkippedLoss);
    }

    #[test]
    fn already_up_past_gain_threshold_is_skipped_gain() {
        // pnl = +6% > GAIN threshold of 5% -> SkippedGain
        assert_eq!(
            classify("t1", dec!(0.56), dec!(0.06), &empty_set(), &empty_set()),
            ScanStatus::SkippedGain
        );
    }

    #[test]
    fn exactly_at_gain_threshold_is_not_skipped() {
        // pnl = +5% == threshold -> should still be Monitoring (strict >)
        assert_eq!(
            classify("t1", dec!(0.55), dec!(0.05), &empty_set(), &empty_set()),
            ScanStatus::Monitoring
        );
    }

    #[test]
    fn positive_pnl_below_threshold_is_monitoring() {
        // +3% < 5% threshold -> Monitoring
        assert_eq!(
            classify("t1", dec!(0.53), dec!(0.03), &empty_set(), &empty_set()),
            ScanStatus::Monitoring
        );
    }

    #[test]
    fn redeemable_position_is_skipped_expired() {
        let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1)).date_naive();
        assert_eq!(
            classify_position(
                "t1",
                dec!(0.50),
                dec!(0.0),
                true,
                Some(tomorrow),
                &empty_set(),
                &empty_set(),
                min(),
                max(),
                loss(),
                gain(),
            ),
            ScanStatus::SkippedExpired
        );
    }

    #[test]
    fn past_end_date_is_skipped_expired() {
        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1)).date_naive();
        assert_eq!(
            classify_position(
                "t1",
                dec!(0.50),
                dec!(0.0),
                false,
                Some(yesterday),
                &empty_set(),
                &empty_set(),
                min(),
                max(),
                loss(),
                gain(),
            ),
            ScanStatus::SkippedExpired
        );
    }

    #[test]
    fn different_tokens_classified_independently() {
        let owned = token_set(&["owned_token"]);
        assert_eq!(
            classify("other_token", dec!(0.50), dec!(-0.10), &owned, &empty_set()),
            ScanStatus::Monitoring
        );
        assert_eq!(
            classify("owned_token", dec!(0.50), dec!(-0.10), &owned, &empty_set()),
            ScanStatus::SkippedOwned
        );
    }
}

// -- Scanner avg_price entry logic ---------------------------------------------
//
// The position scanner creates TradeEvents with price = avg_price (what the
// target paid) instead of cur_price (current market). The strategy engine then
// applies slippage: limit = avg_price * (1 + MAX_SLIPPAGE_PCT).
//
// This ensures we try to pay what the target paid rather than chasing
// a price that may have already moved significantly.

mod scanner_avg_price_tests {
    use super::*;

    // Simulates: target bought at 89¢, market now at 94¢ (target up 5.6%).
    // With MAX_COPY_GAIN_PCT=0.05 this is right at the boundary but let's
    // test the price mechanics with a position that passes the gain filter (+4%).
    #[test]
    fn limit_is_based_on_avg_price_not_cur_price() {
        let avg_price = dec!(0.89);
        let cur_price = dec!(0.927); // +4.2% -- within 5% gain threshold

        let limit_from_avg = calculate_limit_price(avg_price, TradeSide::BUY, dec!(0.02));
        let limit_from_cur = calculate_limit_price(cur_price, TradeSide::BUY, dec!(0.02));

        // Limit based on avg_price is strictly lower than cur_price-based limit
        assert!(
            limit_from_avg < limit_from_cur,
            "avg-based limit {limit_from_avg} should be below cur-based limit {limit_from_cur}"
        );

        // Limit should be avg_price * 1.02 = 0.89 * 1.02 = 0.9078 -> rounds to 0.91
        assert_eq!(limit_from_avg, dec!(0.91));
    }

    // When target is DOWN from their entry (cur_price < avg_price):
    // limit = avg_price * (1+slippage) > cur_price
    // -> on the CLOB this fills immediately at cur_price (cheaper!)
    #[test]
    fn target_underwater_limit_above_market_fills_at_discount() {
        let avg_price = dec!(0.89);
        let cur_price = dec!(0.80); // target is down 10%

        let limit = calculate_limit_price(avg_price, TradeSide::BUY, dec!(0.02));

        // Limit = 0.89 * 1.02 = 0.9078 -> 0.91
        assert_eq!(limit, dec!(0.91));

        // The limit (0.91) is > cur_price (0.80), so the CLOB fills at 0.80
        // (the current ask), which is better than the target's avg of 0.89
        assert!(
            limit > cur_price,
            "limit {limit} should be above cur_price {cur_price} -- order fills at cur_price"
        );
    }

    // When target entry price was very high (e.g. 98¢), limit is capped at 0.99
    #[test]
    fn high_avg_price_limit_capped_at_099() {
        let avg_price = dec!(0.98);
        let limit = calculate_limit_price(avg_price, TradeSide::BUY, dec!(0.02));
        // 0.98 * 1.02 = 0.9996 -> would round to 1.00, but is capped at 0.99
        assert_eq!(limit, dec!(0.99));
    }

    // Worst-case premium over target's avg price
    // = (1 + MAX_COPY_GAIN_PCT) * (1 + MAX_SLIPPAGE_PCT) - 1
    // With GAIN=5% and SLIPPAGE=2%: 1.05 * 1.02 - 1 = 7.1%
    // With GAIN=2% and SLIPPAGE=1%: 1.02 * 1.01 - 1 = 3.02%
    #[test]
    fn worst_case_premium_over_target_entry() {
        let avg_price = dec!(0.50);
        let max_copy_gain_pct = dec!(0.05);
        let slippage = dec!(0.02);

        // Maximum cur_price that still passes the gain filter
        let max_allowed_cur = avg_price * (Decimal::ONE + max_copy_gain_pct);
        // Limit order on top of avg_price
        let our_limit = calculate_limit_price(avg_price, TradeSide::BUY, slippage);

        let premium_over_avg = (our_limit / avg_price) - Decimal::ONE;

        // Our limit is only slippage% above avg_price (not above cur_price)
        assert_eq!(
            premium_over_avg, slippage,
            "our limit should be exactly slippage% above avg_price"
        );

        // And the worst-case we'd ever pay vs target's entry is just the slippage,
        // since we price off avg_price not cur_price.
        // (cur_price up to avg*(1+gain) is just for the WATCH filter, not the order price)
        assert!(
            our_limit < max_allowed_cur,
            "our limit {our_limit} should still be under max allowed cur {max_allowed_cur}"
        );
    }
}

// -- Strategy engine: async end-to-end tests -----------------------------------

mod strategy_engine_tests {
    use super::*;
    use polycopier::clients::OrderSubmitter;
    use polycopier::strategy::start_strategy_engine;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, RwLock};

    fn mock_submitter(log: Arc<Mutex<Vec<OrderRequest>>>) -> OrderSubmitter {
        Arc::new(move |order: OrderRequest| {
            let log = log.clone();
            let fut: Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>> =
                Box::pin(async move {
                    log.lock().unwrap().push(order);
                    Ok(())
                });
            fut
        })
    }

    /// Build a TargetPosition entry (what the scanner would have fetched).
    fn target_pos(token_id: &str) -> TargetPosition {
        TargetPosition {
            title: "Test Market".to_string(),
            outcome: "Yes".to_string(),
            token_id: token_id.to_string(),
            cur_price: dec!(0.50),
            avg_price: dec!(0.45),
            percent_pnl: dec!(0.10),
            size: dec!(10),
            status: ScanStatus::Monitoring,
            source_wallet: "0xtest..wall".to_string(),
        }
    }

    #[tokio::test]
    async fn valid_trade_from_target_wallet_is_executed() {
        let config = test_config();
        let state = Arc::new(RwLock::new(BotState::new()));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(
            rx,
            state.clone(),
            risk,
            mock_submitter(log.clone()),
            config.clone(),
        );

        // Seed sufficient balance so the pre-check doesn't block submission
        {
            let mut guard = state.write().await;
            guard.total_balance = dec!(100);
        }

        tx.send(make_trade("0xabc", dec!(0.50), dec!(10), TradeSide::BUY))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        let orders = log.lock().unwrap();
        assert_eq!(orders.len(), 1);
        // 0.50 + (0.50 * 0.02) = 0.51
        assert_eq!(orders[0].price, dec!(0.51));
    }

    #[tokio::test]
    async fn trade_from_unknown_wallet_is_skipped() {
        let config = test_config();
        let state = Arc::new(RwLock::new(BotState::new()));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(rx, state.clone(), risk, mock_submitter(log.clone()), config);

        tx.send(make_trade(
            "0xunknown",
            dec!(0.50),
            dec!(10),
            TradeSide::BUY,
        ))
        .await
        .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        assert!(log.lock().unwrap().is_empty());
        let guard = state.read().await;
        assert_eq!(guard.trades_skipped, 1);
        assert_eq!(guard.copies_executed, 0);
    }

    #[tokio::test]
    async fn oversized_trade_is_capped_to_max_usd() {
        let config = test_config(); // max_trade_size_usd = $10
        let state = Arc::new(RwLock::new(BotState::new()));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(rx, state.clone(), risk, mock_submitter(log.clone()), config);

        // Seed sufficient balance so the pre-check doesn't block submission
        {
            let mut guard = state.write().await;
            guard.total_balance = dec!(100);
        }

        // 100 shares * $0.40 = $40 - capped to 10/0.40 = 25 shares
        tx.send(make_trade("0xabc", dec!(0.40), dec!(100), TradeSide::BUY))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        let orders = log.lock().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].size, dec!(25));
    }

    #[tokio::test]
    async fn micro_trade_rejected_by_risk_engine() {
        let config = test_config();
        let state = Arc::new(RwLock::new(BotState::new()));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(rx, state.clone(), risk, mock_submitter(log.clone()), config);

        // $0.05 * 0.01 = $0.0005 - below $1 spoofing threshold
        tx.send(make_trade("0xabc", dec!(0.05), dec!(0.01), TradeSide::BUY))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        assert!(log.lock().unwrap().is_empty());
        assert_eq!(state.read().await.trades_skipped, 1);
    }

    #[tokio::test]
    async fn sell_trade_uses_lower_limit_price() {
        let config = test_config();
        // Must pre-populate both our position AND the target's scanner position
        let mut init_state = BotState::new();
        init_state.positions.insert(
            "99999".to_string(),
            Position {
                token_id: "99999".to_string(),
                size: dec!(10),
                average_entry_price: dec!(0.60),
            },
        );
        init_state.target_positions.push(target_pos("99999"));
        let state = Arc::new(RwLock::new(init_state));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(rx, state.clone(), risk, mock_submitter(log.clone()), config);

        // SELL: 0.60 - (0.60 * 0.02) = 0.588
        tx.send(make_trade("0xabc", dec!(0.60), dec!(5), TradeSide::SELL))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        let orders = log.lock().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].price, dec!(0.59)); // 0.60 - 2% = 0.588, rounded to 2dp = 0.59
        assert_eq!(orders[0].side, TradeSide::SELL);
    }

    // -- SELL size: position lookup (new: proportional-close fix) --------------

    #[tokio::test]
    async fn sell_uses_our_held_size_not_target_size() {
        // Target sells 500 shares; we hold only 20 -> SELL should be 20 * 0.97 = 19.40
        let config = test_config();
        let mut init_state = BotState::new();
        init_state.positions.insert(
            "99999".to_string(),
            Position {
                token_id: "99999".to_string(),
                size: dec!(20),
                average_entry_price: dec!(0.50),
            },
        );
        let state = Arc::new(RwLock::new(init_state));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(
            rx,
            state.clone(),
            risk,
            mock_submitter(log.clone()),
            config.clone(),
        );

        // Target sells 500 shares - we hold only 20
        tx.send(make_trade("0xabc", dec!(0.60), dec!(500), TradeSide::SELL))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        let orders = log.lock().unwrap();
        assert_eq!(orders.len(), 1, "expected exactly one SELL order");
        assert_eq!(
            orders[0].size,
            dec!(19.40),
            "should be 20 * 0.97, not 500 * 0.97"
        );
        assert_eq!(orders[0].side, TradeSide::SELL);
    }

    #[tokio::test]
    async fn sell_applies_97_pct_fee_buffer_to_our_position() {
        // Held: 10 shares -> SELL size should be 10 * 0.97 = 9.70
        let config = test_config();
        let mut init_state = BotState::new();
        init_state.positions.insert(
            "99999".to_string(),
            Position {
                token_id: "99999".to_string(),
                size: dec!(10),
                average_entry_price: dec!(0.60),
            },
        );
        let state = Arc::new(RwLock::new(init_state));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(
            rx,
            state.clone(),
            risk,
            mock_submitter(log.clone()),
            config.clone(),
        );

        tx.send(make_trade("0xabc", dec!(0.60), dec!(10), TradeSide::SELL))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        let orders = log.lock().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].size, dec!(9.70));
    }

    #[tokio::test]
    async fn sell_skipped_when_we_never_entered_targets_long() {
        // Scanner shows target HAS the position, but we never entered it -> skip.
        let config = test_config();
        let mut init_state = BotState::new();
        // Target holds "99999" but we don't
        init_state.target_positions.push(target_pos("99999"));
        let state = Arc::new(RwLock::new(init_state));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(
            rx,
            state.clone(),
            risk,
            mock_submitter(log.clone()),
            config.clone(),
        );

        tx.send(make_trade("0xabc", dec!(0.50), dec!(100), TradeSide::SELL))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        assert!(
            log.lock().unwrap().is_empty(),
            "should NOT SELL a position we never entered"
        );
        let guard = state.read().await;
        assert_eq!(guard.trades_skipped, 1);
        assert_eq!(guard.copies_executed, 0);
    }

    #[tokio::test]
    async fn sell_skipped_when_target_opening_short() {
        // Target has NO prior scanner position -> SELL = opening a short entry.
        // We cannot replicate shorts, so skip.
        let config = test_config();
        let mut init_state = BotState::new();
        // Seed a DIFFERENT token so scanner_ready = true
        init_state.target_positions.push(target_pos("other_token"));
        let state = Arc::new(RwLock::new(init_state));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(
            rx,
            state.clone(),
            risk,
            mock_submitter(log.clone()),
            config.clone(),
        );

        // Target sells "99999" but has no prior long position -> short entry
        tx.send(make_trade("0xabc", dec!(0.60), dec!(10), TradeSide::SELL))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        assert!(
            log.lock().unwrap().is_empty(),
            "should skip short-entry SELL"
        );
        let guard = state.read().await;
        assert_eq!(guard.trades_skipped, 1);
        assert_eq!(guard.copies_executed, 0);
    }

    #[tokio::test]
    async fn buy_skipped_when_target_closing_short_we_are_long() {
        // We hold a long. Target has NO scanner position -> target is likely
        // closing a short. Copying this BUY would add to our long incorrectly.
        let config = test_config();
        let mut init_state = BotState::new();
        init_state.positions.insert(
            "99999".to_string(),
            Position {
                token_id: "99999".to_string(),
                size: dec!(10),
                average_entry_price: dec!(0.50),
            },
        );
        // Scanner is ready (non-empty) but does NOT contain "99999"
        init_state.target_positions.push(target_pos("other_token"));
        let state = Arc::new(RwLock::new(init_state));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(
            rx,
            state.clone(),
            risk,
            mock_submitter(log.clone()),
            config.clone(),
        );

        tx.send(make_trade("0xabc", dec!(0.50), dec!(10), TradeSide::BUY))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        assert!(
            log.lock().unwrap().is_empty(),
            "should skip BUY that looks like short close"
        );
        let guard = state.read().await;
        assert_eq!(guard.trades_skipped, 1);
        assert_eq!(guard.copies_executed, 0);
    }

    #[tokio::test]
    async fn sell_size_never_exceeds_held_position() {
        // Even if the target trade is tiny, we sell exactly our full balance * 0.97
        let config = test_config();
        let mut init_state = BotState::new();
        // We hold 15 shares; target only sells 3 (partial close signal)
        init_state.positions.insert(
            "99999".to_string(),
            Position {
                token_id: "99999".to_string(),
                size: dec!(15),
                average_entry_price: dec!(0.50),
            },
        );
        let state = Arc::new(RwLock::new(init_state));
        let risk = RiskEngine::new(config.clone());
        let log: Arc<Mutex<Vec<OrderRequest>>> = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = mpsc::channel::<TradeEvent>(10);
        start_strategy_engine(rx, state.clone(), risk, mock_submitter(log.clone()), config);

        tx.send(make_trade("0xabc", dec!(0.60), dec!(3), TradeSide::SELL))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        let orders = log.lock().unwrap();
        assert_eq!(orders.len(), 1);
        // Should use OUR held size (15), not target's size (3)
        // 15 * 0.97 = 14.55
        assert_eq!(orders[0].size, dec!(14.55));
        // Crucially, sell size must never exceed what we hold
        assert!(
            orders[0].size < dec!(15),
            "sell size must be < held position"
        );
    }
}

// -- Balance conversion (math sanity check) ------------------------------------

#[test]
fn micro_usdc_converts_to_usdc_correctly() {
    // The CLOB API returns balance in micro-USDC (6 decimal places)
    let raw = dec!(38_900_858);
    let usdc = raw / dec!(1_000_000);
    assert_eq!(usdc, dec!(38.900858));
}

#[test]
fn zero_micro_usdc_is_zero_usdc() {
    let raw = dec!(0);
    let usdc = raw / dec!(1_000_000);
    assert_eq!(usdc, Decimal::ZERO);
}

// -- Adaptive scan interval tests ----------------------------------------------

mod scan_interval_tests {
    use super::*;
    use polycopier::position_scanner::compute_scan_interval;

    const MAX_LOSS: Decimal = dec!(0.10); // 10%
    const MIN_S: u64 = 10;
    const MAX_S: u64 = 60;

    /// Build a target position at a given percent_pnl with Monitoring status.
    fn monitoring(pnl: Decimal) -> TargetPosition {
        TargetPosition {
            title: "T".to_string(),
            outcome: "Yes".to_string(),
            token_id: "tok".to_string(),
            cur_price: dec!(0.50),
            avg_price: dec!(0.50),
            percent_pnl: pnl,
            size: dec!(10),
            status: ScanStatus::Monitoring,
            source_wallet: "0xtest..wall".to_string(),
        }
    }

    /// Build a target position with a non-Monitoring status (should be ignored).
    fn skipped(pnl: Decimal, status: ScanStatus) -> TargetPosition {
        TargetPosition {
            status,
            percent_pnl: pnl,
            ..monitoring(pnl)
        }
    }

    #[test]
    fn no_monitoring_positions_returns_max_interval() {
        // Nothing enterable -> no urgency -> scan at slowest rate
        let interval = compute_scan_interval(&[], MAX_LOSS);
        assert_eq!(interval, MAX_S);
    }

    #[test]
    fn target_at_entry_price_is_most_urgent() {
        // pnl = 0% -> closeness = 1.0 -> MIN interval (scan every 10s)
        let positions = [monitoring(dec!(0))];
        let interval = compute_scan_interval(&positions, MAX_LOSS);
        assert_eq!(
            interval, MIN_S,
            "price at entry should trigger fastest scan"
        );
    }

    #[test]
    fn target_with_small_move_is_still_urgent() {
        // pnl = +5% -> closeness = 1 - 0.05/0.15 ~ 0.67 -> ~27s
        let positions = [monitoring(dec!(0.05))];
        let interval = compute_scan_interval(&positions, MAX_LOSS);
        assert!(
            interval < 35,
            "small move should still be urgent, got {}s",
            interval
        );
    }

    #[test]
    fn target_in_small_drawdown_still_scanned_urgently() {
        // pnl = -5% (within allowed drawdown) -> closeness = 1 - 0.05/0.15 ~ 0.67 -> ~27s
        // We should still scan frequently - it's enterable and near the target's entry
        let positions = [monitoring(dec!(-0.05))];
        let interval = compute_scan_interval(&positions, MAX_LOSS);
        assert!(
            interval < 40,
            "position within drawdown should still scan frequently, got {}s",
            interval
        );
    }

    #[test]
    fn target_far_from_entry_scans_slowly() {
        // pnl = +15% -> closeness = 0 -> MAX interval, we'd be chasing
        let positions = [monitoring(dec!(0.15))];
        let interval = compute_scan_interval(&positions, MAX_LOSS);
        assert_eq!(interval, MAX_S, "price moved 15%+ from entry - no urgency");
    }

    #[test]
    fn target_beyond_drawdown_is_excluded() {
        // pnl = -11% which is below max_copy_loss_pct=10% -> classify_position filters this
        // compute_scan_interval also excludes it -> MAX interval
        let positions = [monitoring(dec!(-0.11))];
        let interval = compute_scan_interval(&positions, MAX_LOSS);
        assert_eq!(
            interval, MAX_S,
            "position past drawdown limit should not create urgency"
        );
    }

    #[test]
    fn non_monitoring_positions_are_ignored() {
        // Entered, SkippedLoss, etc. are not candidates - should not affect interval
        let positions = [
            skipped(dec!(0.0), ScanStatus::Entered), // would be urgent if counted
            skipped(dec!(-0.11), ScanStatus::SkippedLoss), // past drawdown
        ];
        let interval = compute_scan_interval(&positions, MAX_LOSS);
        assert_eq!(
            interval, MAX_S,
            "non-Monitoring positions should not drive urgency"
        );
    }

    #[test]
    fn best_opportunity_drives_urgency() {
        // Three positions: one far from entry, one at entry, one past drawdown
        // The one at entry should dominate -> MIN interval
        let positions = [
            monitoring(dec!(0.20)),  // too far -> closeness 0
            monitoring(dec!(0.0)),   // at entry -> closeness 1.0 -> urgent
            monitoring(dec!(-0.11)), // past drawdown -> filtered
        ];
        let interval = compute_scan_interval(&positions, MAX_LOSS);
        assert_eq!(interval, MIN_S, "best opportunity should dominate");
    }
}
// -- Regression: duplicate-entry prevention ------------------------------------
//
// Bug: on every bot restart the position scanner re-entered markets we already
// held because `already_queued` was reset to an empty HashSet. The fix:
//
//   A. `seed_own_positions` is now awaited in main.rs BEFORE the scanner starts,
//      so state.positions is guaranteed populated by the time the first scan runs.
//
//   B. The scanner pre-populates `already_queued` from state.positions, so even
//      tokens we hold are treated as already queued from the very first scan.
//
// These tests verify the classify_position behaviour that underpins both layers.

mod reentry_prevention_tests {
    use super::*;

    fn tomorrow() -> chrono::NaiveDate {
        (chrono::Utc::now() + chrono::Duration::days(1)).date_naive()
    }

    fn classify(token: &str, our_tokens: &HashSet<String>, queued: &HashSet<String>) -> ScanStatus {
        classify_position(
            token,
            dec!(0.50),
            dec!(0.0),
            false,
            Some(tomorrow()),
            our_tokens,
            queued,
            dec!(0.02),
            dec!(0.95),
            dec!(0.40),
            dec!(0.05),
        )
    }

    // Layer A: state.positions provides SkippedOwned ------------------------

    #[test]
    fn held_position_returns_skipped_owned() {
        // Token is in state.positions (seeded from wallet) → must not be entered.
        let owned = token_set(&["btc_56k_token"]);
        assert_eq!(
            classify("btc_56k_token", &owned, &HashSet::new()),
            ScanStatus::SkippedOwned,
            "token we hold must be SkippedOwned, preventing re-entry"
        );
    }

    #[test]
    fn unrelated_token_not_blocked_by_other_owned() {
        // Holding token A must not block entry into token B.
        let owned = token_set(&["token_a"]);
        assert_eq!(
            classify("token_b", &owned, &HashSet::new()),
            ScanStatus::Monitoring
        );
    }

    // Layer B: already_queued pre-populated from state.positions ----------

    #[test]
    fn pre_queued_token_returns_entered_not_monitoring() {
        // When already_queued is seeded from state.positions at scanner start,
        // any token we hold gets Entered status — scanner won't re-send event.
        let queued = token_set(&["btc_56k_token"]);
        assert_eq!(
            classify("btc_56k_token", &HashSet::new(), &queued),
            ScanStatus::Entered,
            "pre-queued token must not be submitted again"
        );
    }

    #[test]
    fn owned_takes_priority_over_pre_queued() {
        // Both layers active: both should prevent re-entry; SkippedOwned wins.
        let owned = token_set(&["tok"]);
        let queued = token_set(&["tok"]);
        assert_eq!(classify("tok", &owned, &queued), ScanStatus::SkippedOwned);
    }

    // The exact scenario that caused the triple-entry bug ------------------

    #[test]
    fn restart_scenario_no_reentry() {
        // Simulate: scanner restarts, state.positions has "btc_56k" from wallet seed.
        // already_queued is pre-populated from state.positions.
        // Both checks prevent re-entry — scanner must not Monitoring this token.
        let previously_held = token_set(&["btc_56k"]);

        // Layer A: SkippedOwned via state.positions
        let status_a = classify("btc_56k", &previously_held, &HashSet::new());
        assert_ne!(
            status_a,
            ScanStatus::Monitoring,
            "layer A: owned position must not be Monitoring on restart"
        );

        // Layer B: Entered via already_queued seeded from state.positions
        let status_b = classify("btc_56k", &HashSet::new(), &previously_held);
        assert_ne!(
            status_b,
            ScanStatus::Monitoring,
            "layer B: pre-queued position must not be Monitoring on restart"
        );
    }

    #[test]
    fn fresh_unowned_token_still_enters_after_fix() {
        // The fix must not block NEW entries.
        // A token we don't hold and haven't queued should still be Monitoring.
        let owned = token_set(&["other_token"]);
        let queued = token_set(&["another_token"]);
        assert_eq!(
            classify("new_opportunity", &owned, &queued),
            ScanStatus::Monitoring,
            "unrelated new token must still be eligible for entry"
        );
    }
}
