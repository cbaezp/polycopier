//! Tests for `src/risk.rs` — Gap 12.
//! Run: `cargo test --test risk_tests`

use polycopier::config::Config;
use polycopier::models::{TradeEvent, TradeSide};
use polycopier::risk::RiskEngine;
use rust_decimal_macros::dec;

fn test_config() -> Config {
    Config {
        private_key: "0xdeadbeef".to_string(),
        funder_address: "0x1111111111111111111111111111111111111111".to_string(),
        chain_id: 137,
        target_wallets: vec!["0xabc".to_string()],
        target_scalars: std::collections::HashMap::new(),
        max_slippage_pct: dec!(0.02),
        max_trade_size_usd: dec!(50),
        max_delay_seconds: 2,
        max_copy_loss_pct: dec!(0.40),
        max_copy_gain_pct: dec!(0.05),
        min_entry_price: dec!(0.02),
        max_entry_price: dec!(0.999),
        sizing_mode: polycopier::config::SizingMode::Fixed,
        copy_size_pct: None,
        scan_max_entries_per_cycle: 1,
        sell_fee_buffer: dec!(0.97),
        ledger_retention_days: 90,
        max_daily_volume_usd: dec!(0), // disabled by default
        max_consecutive_losses: 0,     // disabled by default
        loss_cooldown_secs: 300,
        is_sim: false,
        sim_balance: None,
    }
}

fn make_buy(
    token_id: &str,
    price: rust_decimal::Decimal,
    size: rust_decimal::Decimal,
) -> TradeEvent {
    TradeEvent {
        transaction_hash: "0xtest".to_string(),
        maker_address: "0xabc".to_string(),
        taker_address: "0xabc".to_string(),
        token_id: token_id.to_string(),
        price,
        size,
        side: TradeSide::BUY,
        timestamp: chrono::Utc::now().timestamp(),
    }
}

fn make_sell(
    token_id: &str,
    price: rust_decimal::Decimal,
    size: rust_decimal::Decimal,
) -> TradeEvent {
    TradeEvent {
        transaction_hash: "0xtest".to_string(),
        maker_address: "0xabc".to_string(),
        taker_address: "0xabc".to_string(),
        token_id: token_id.to_string(),
        price,
        size,
        side: TradeSide::SELL,
        timestamp: chrono::Utc::now().timestamp(),
    }
}

// ---------------------------------------------------------------------------
// Micro-trade anti-spoofing (original check)
// ---------------------------------------------------------------------------

#[test]
fn risk_rejects_micro_trade_below_one_dollar() {
    let mut engine = RiskEngine::new(test_config());
    let trade = make_buy("tok", dec!(0.50), dec!(0.10)); // $0.05 notional
    assert!(engine.check_trade(&trade).is_err());
    assert!(engine.check_trade(&trade).unwrap_err().contains("spoofing"));
}

#[test]
fn risk_allows_one_dollar_trade() {
    let mut engine = RiskEngine::new(test_config());
    let trade = make_buy("tok", dec!(0.50), dec!(2.00)); // $1.00 notional
    assert!(engine.check_trade(&trade).is_ok());
}

#[test]
fn risk_allows_normal_trade() {
    let mut engine = RiskEngine::new(test_config());
    let trade = make_buy("tok", dec!(0.60), dec!(10.00)); // $6.00 notional
    assert!(engine.check_trade(&trade).is_ok());
}

// ---------------------------------------------------------------------------
// Daily volume limit (Gap 12)
// ---------------------------------------------------------------------------

#[test]
fn risk_rejects_when_daily_volume_exceeded() {
    let mut cfg = test_config();
    cfg.max_daily_volume_usd = dec!(10); // $10 daily limit
    let mut engine = RiskEngine::new(cfg);

    // First trade: $9 — should pass
    let t1 = make_buy("tok_a", dec!(0.90), dec!(10)); // $9
    assert!(engine.check_trade(&t1).is_ok());

    // Second trade: $2 — $9 + $2 = $11 > $10 → should be rejected
    let t2 = make_buy("tok_b", dec!(0.50), dec!(4)); // $2
    let err = engine.check_trade(&t2).unwrap_err();
    assert!(err.contains("Daily volume limit"), "error was: {err}");
}

#[test]
fn risk_allows_trades_when_daily_limit_is_zero_disabled() {
    let mut cfg = test_config();
    cfg.max_daily_volume_usd = dec!(0); // disabled
    let mut engine = RiskEngine::new(cfg);

    // Each trade uses a distinct token to avoid the rapid-flip 60s guard.
    for i in 0..10 {
        let trade = make_buy(&format!("tok_{}", i), dec!(0.90), dec!(1000)); // $900 each
        assert!(
            engine.check_trade(&trade).is_ok(),
            "trade {i} should succeed"
        );
    }
}

#[test]
fn risk_accumulates_sell_side_in_daily_volume() {
    let mut cfg = test_config();
    cfg.max_daily_volume_usd = dec!(5);
    let mut engine = RiskEngine::new(cfg);

    // SELLs contribute to daily volume too (outgoing flow)
    let sell = make_sell("tok", dec!(0.80), dec!(5)); // $4 — passes
    assert!(engine.check_trade(&sell).is_ok());

    // Now $4 used. Another $2 buy → $6 > $5 → rejected.
    let buy = make_buy("tok2", dec!(0.50), dec!(4)); // $2
    assert!(engine.check_trade(&buy).is_err());
}

// ---------------------------------------------------------------------------
// Consecutive-loss circuit breaker (Gap 12)
// ---------------------------------------------------------------------------

#[test]
fn risk_consecutive_loss_triggers_cooldown_immediately() {
    let mut cfg = test_config();
    cfg.max_consecutive_losses = 2; // trigger after 2 losses
    cfg.loss_cooldown_secs = 9999; // long enough to assert within the test
    let mut engine = RiskEngine::new(cfg);

    // Record 2 losses
    engine.record_loss();
    engine.record_loss(); // threshold hit → cooldown active

    // Next trade should be blocked
    let trade = make_buy("tok", dec!(0.50), dec!(10)); // $5
    let err = engine.check_trade(&trade).unwrap_err();
    assert!(err.contains("cooldown"), "error was: {err}");
}

#[test]
fn risk_consecutive_losses_disabled_when_max_is_zero() {
    let mut cfg = test_config();
    cfg.max_consecutive_losses = 0; // disabled
    let mut engine = RiskEngine::new(cfg);

    // Record many losses — should never trigger cooldown
    for _ in 0..100 {
        engine.record_loss();
    }

    let trade = make_buy("tok", dec!(0.50), dec!(10));
    assert!(engine.check_trade(&trade).is_ok());
}

#[test]
fn risk_successful_trade_resets_consecutive_loss_counter() {
    let mut cfg = test_config();
    cfg.max_consecutive_losses = 3;
    cfg.loss_cooldown_secs = 9999;
    let mut engine = RiskEngine::new(cfg);

    engine.record_loss();
    engine.record_loss(); // 2 losses

    // A successful trade resets the counter
    let trade = make_buy("tok", dec!(0.50), dec!(10));
    assert!(engine.check_trade(&trade).is_ok()); // counter → 0

    engine.record_loss();
    engine.record_loss(); // 2 losses again; 3rd would trigger
                          // Another successful trade keeps us safe
    assert!(engine
        .check_trade(&make_buy("tok2", dec!(0.60), dec!(10)))
        .is_ok());
}

// ---------------------------------------------------------------------------
// Rapid-flip guard (Gap 12)
// ---------------------------------------------------------------------------

#[test]
fn risk_rapid_flip_rejected_on_same_token_within_60s() {
    let mut engine = RiskEngine::new(test_config());

    // First BUY — approved and recorded
    let buy1 = make_buy("tok_a", dec!(0.50), dec!(10));
    assert!(engine.check_trade(&buy1).is_ok());

    // Immediate second BUY for the same token — rejected as rapid flip
    let buy2 = make_buy("tok_a", dec!(0.50), dec!(10));
    let err = engine.check_trade(&buy2).unwrap_err();
    assert!(err.contains("Rapid-flip"), "error was: {err}");
}

#[test]
fn risk_rapid_flip_allows_different_tokens() {
    let mut engine = RiskEngine::new(test_config());

    let buy_a = make_buy("tok_a", dec!(0.50), dec!(10));
    let buy_b = make_buy("tok_b", dec!(0.60), dec!(10));

    assert!(engine.check_trade(&buy_a).is_ok());
    assert!(engine.check_trade(&buy_b).is_ok()); // different token → allowed
}

#[test]
fn risk_rapid_flip_allows_sell_after_buy() {
    let mut engine = RiskEngine::new(test_config());

    // BUY passes
    let buy = make_buy("tok_a", dec!(0.50), dec!(10));
    assert!(engine.check_trade(&buy).is_ok());

    // SELL of the same token is NOT subject to rapid-flip guard (only BUYs are)
    let sell = make_sell("tok_a", dec!(0.70), dec!(10));
    assert!(engine.check_trade(&sell).is_ok());
}
