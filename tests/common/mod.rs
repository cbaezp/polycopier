//! Shared helpers for all integration test files.
//! Each test binary includes this with `mod common;`.
#![allow(dead_code)]

use polycopier::config::Config;
use polycopier::models::{
    EvaluatedTrade, Position, ScanStatus, SizingMode, TargetPosition, TradeEvent, TradeSide,
};

use polycopier::state::BotState;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashSet;

/// Default test configuration. Uses `SizingMode::Fixed` with $10 cap so sizing tests
/// are deterministic without a live balance. Override fields as needed per test.
pub fn test_config() -> Config {
    Config {
        private_key: "0xdeadbeef".to_string(),
        funder_address: "0x1111111111111111111111111111111111111111".to_string(),
        chain_id: 137,
        target_wallets: vec!["0xabc".to_string()],
        target_scalars: std::collections::HashMap::new(),
        max_slippage_pct: dec!(0.02),
        max_trade_size_usd: dec!(10.00),
        max_delay_seconds: 2,
        max_copy_loss_pct: dec!(0.40),
        max_copy_gain_pct: dec!(0.05),
        min_entry_price: dec!(0.02),
        max_entry_price: dec!(0.999),
        sizing_mode: SizingMode::Fixed,
        copy_size_pct: None,
        scan_max_entries_per_cycle: 1,
        scan_min_amount: dec!(0),
        scan_max_amount: dec!(9999999999),
        sell_fee_buffer: dec!(0.97),
        ledger_retention_days: 90,
        ignore_closing_in_mins: None,
        max_daily_volume_usd: dec!(0),
        max_consecutive_losses: 0,
        loss_cooldown_secs: 300,
        is_sim: false,
        sim_balance: None,
    }
}

pub fn make_trade(taker: &str, price: Decimal, size: Decimal, side: TradeSide) -> TradeEvent {
    make_trade_for_token(taker, "99999", price, size, side)
}

pub fn make_trade_for_token(
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

pub fn make_eval(validated: bool) -> EvaluatedTrade {
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

pub fn empty_set() -> HashSet<String> {
    HashSet::new()
}

pub fn token_set(tokens: &[&str]) -> HashSet<String> {
    tokens.iter().map(|s| s.to_string()).collect()
}

pub fn target_pos(token_id: &str) -> TargetPosition {
    TargetPosition {
        title: "Test Market".to_string(),
        outcome: "YES".to_string(),
        token_id: token_id.to_string(),
        cur_price: dec!(0.50),
        avg_price: dec!(0.45),
        percent_pnl: dec!(0.10),
        size: dec!(20),
        status: ScanStatus::Monitoring,
        source_wallet: "0xtest..wall".to_string(),
    }
}

pub fn make_position(token_id: &str, size: Decimal) -> (String, Position) {
    (
        token_id.to_string(),
        Position {
            token_id: token_id.to_string(),
            size,
            average_entry_price: dec!(0.50),
        },
    )
}

/// Seeded BotState with total_balance set, for tests that require balance pre-check.
pub fn state_with_balance(balance: Decimal) -> BotState {
    let mut s = BotState::new(false, None);
    s.total_balance = balance;
    s
}
