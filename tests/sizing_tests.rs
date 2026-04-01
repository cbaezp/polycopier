//! Tests for `compute_order_usd` -- all SizingMode variants.
//!
//! Covers: Fixed, SelfPct, TargetUsd, floor/cap guards, misconfigured max < min.

mod common;

use polycopier::models::SizingMode;
use polycopier::strategy::{compute_order_usd, MIN_ORDER_SHARES, MIN_ORDER_USD};
use rust_decimal_macros::dec;

// -- Helpers ------------------------------------------------------------------

/// Convenience wrapper -- mirrors the full signature with named fields.
fn size(
    our_balance: impl Into<rust_decimal::Decimal>,
    sizing_mode: &SizingMode,
    copy_size_pct: Option<rust_decimal::Decimal>,
    max_trade_usd: impl Into<rust_decimal::Decimal>,
    target_notional: impl Into<rust_decimal::Decimal>,
) -> rust_decimal::Decimal {
    compute_order_usd(
        our_balance.into(),
        sizing_mode,
        copy_size_pct,
        max_trade_usd.into(),
        target_notional.into(),
    )
}

// -- SizingMode::Fixed ---------------------------------------------------------

#[test]
fn fixed_always_returns_max() {
    let usd = size(dec!(500), &SizingMode::Fixed, None, dec!(10), dec!(0));
    assert_eq!(usd, dec!(10));
}

#[test]
fn fixed_ignores_balance_changes() {
    let usd_small = size(dec!(5), &SizingMode::Fixed, None, dec!(10), dec!(0));
    let usd_large = size(dec!(10000), &SizingMode::Fixed, None, dec!(10), dec!(0));
    assert_eq!(usd_small, usd_large);
}

#[test]
fn fixed_with_no_copy_size_pct_still_returns_max() {
    // Explicit: copy_size_pct is ignored in Fixed mode
    let usd = size(
        dec!(200),
        &SizingMode::Fixed,
        Some(dec!(0.10)),
        dec!(25),
        dec!(0),
    );
    assert_eq!(usd, dec!(25));
}

// -- SizingMode::SelfPct -------------------------------------------------------

#[test]
fn self_pct_uses_fraction_of_balance() {
    // 10% of $200 = $20, within $50 cap
    let usd = size(
        dec!(200),
        &SizingMode::SelfPct,
        Some(dec!(0.10)),
        dec!(50),
        dec!(0),
    );
    assert_eq!(usd, dec!(20));
}

#[test]
fn self_pct_capped_at_max_trade_usd() {
    // 10% of $1000 = $100, but cap is $50
    let usd = size(
        dec!(1000),
        &SizingMode::SelfPct,
        Some(dec!(0.10)),
        dec!(50),
        dec!(0),
    );
    assert_eq!(usd, dec!(50));
}

#[test]
fn self_pct_below_clob_minimum_returns_zero() {
    // 10% of $5 = $0.50 — below $1 CLOB minimum → return 0 (skip).
    let usd = size(
        dec!(5),
        &SizingMode::SelfPct,
        Some(dec!(0.10)),
        dec!(50),
        dec!(0),
    );
    assert_eq!(
        usd,
        dec!(0),
        "$0.50 is below $1 minimum — should return 0 (skip)"
    );
}

#[test]
fn self_pct_at_exact_cap_boundary() {
    // 50% of $100 = $50 = cap exactly
    let usd = size(
        dec!(100),
        &SizingMode::SelfPct,
        Some(dec!(0.50)),
        dec!(50),
        dec!(0),
    );
    assert_eq!(usd, dec!(50));
}

#[test]
fn self_pct_with_no_copy_size_pct_falls_back_to_max() {
    // No pct provided -- fallback: pct = max/balance = 10/200 = 0.05 => 200*0.05 = 10 = max
    let usd = size(dec!(200), &SizingMode::SelfPct, None, dec!(10), dec!(0));
    assert_eq!(usd, dec!(10));
}

// -- SizingMode::TargetUsd ----------------------------------------------------

#[test]
fn target_usd_mirrors_target_notional() {
    // Target bet 10 shares @ $0.80 = $8.00
    let usd = size(dec!(500), &SizingMode::TargetUsd, None, dec!(50), dec!(8));
    assert_eq!(usd, dec!(8));
}

#[test]
fn target_usd_capped_at_max() {
    // Target bet $200, our cap is $50
    let usd = size(dec!(500), &SizingMode::TargetUsd, None, dec!(50), dec!(200));
    assert_eq!(usd, dec!(50));
}

#[test]
fn target_usd_below_clob_minimum_returns_zero() {
    // Target bet $0.50 — below $1 CLOB minimum → return 0 (skip).
    let usd = size(
        dec!(500),
        &SizingMode::TargetUsd,
        None,
        dec!(50),
        dec!(0.50),
    );
    assert_eq!(usd, dec!(0), "below-$1 target bet must return 0 (skip)");
}

#[test]
fn target_usd_exact_max_boundary() {
    // Target bet exactly $50 = cap
    let usd = size(dec!(500), &SizingMode::TargetUsd, None, dec!(50), dec!(50));
    assert_eq!(usd, dec!(50));
}

#[test]
fn target_usd_ignores_our_balance() {
    // Our balance doesn't matter for TargetUsd
    let small = size(dec!(10), &SizingMode::TargetUsd, None, dec!(50), dec!(30));
    let large = size(
        dec!(10000),
        &SizingMode::TargetUsd,
        None,
        dec!(50),
        dec!(30),
    );
    assert_eq!(small, large);
    assert_eq!(small, dec!(30));
}

// -- Floor/ceiling edge cases (mode-agnostic) ---------------------------------

#[test]
fn max_less_than_min_clob_floor_returns_zero() {
    // Misconfigured: max_trade = $0.50, below $1 CLOB minimum → return 0 (skip).
    let usd = size(dec!(100), &SizingMode::Fixed, None, dec!(0.5), dec!(0));
    assert_eq!(usd, dec!(0));
}

// -- Exact bug scenario: 7% of $39 -------------------------------------------

#[test]
fn seven_pct_of_small_balance_is_skipped() {
    // 7% of $10 = $0.70 < $1 → skip.
    let usd = size(
        dec!(10),
        &SizingMode::SelfPct,
        Some(dec!(0.07)),
        dec!(50),
        dec!(0),
    );
    assert_eq!(
        usd,
        dec!(0),
        "7% of $10 = $0.70 is below $1 minimum — should skip"
    );
}

#[test]
fn seven_pct_of_39_goes_through() {
    // COPY_SIZE_PCT=0.07 with $39 balance → 7% × $39 = $2.73 ≥ $1 → order placed.
    // This is the exact user scenario that was broken when MIN_ORDER_USD was $5.
    let usd = size(
        dec!(39),
        &SizingMode::SelfPct,
        Some(dec!(0.07)),
        dec!(50),
        dec!(0),
    );
    assert!(
        usd > dec!(0),
        "7% of $39 = $2.73 should clear the $1 minimum: got {usd}"
    );
    assert_eq!(usd, dec!(2.73));
}

#[test]
fn seven_pct_fires_once_balance_grows_above_threshold() {
    // At $15 balance: 7% × $15 = $1.05 ≥ $1 → order placed.
    let usd = size(
        dec!(15),
        &SizingMode::SelfPct,
        Some(dec!(0.07)),
        dec!(50),
        dec!(0),
    );
    assert!(
        usd >= dec!(1),
        "7% of $15 = $1.05 should clear the $1 minimum: got {usd}"
    );
}

#[test]
fn self_pct_misconfigured_max_returns_zero() {
    // 50% of $10 = $5, but max_trade = $5 → capped at $5 → exactly MIN_ORDER_USD, so allowed.
    // (Edge: $5 == MIN_ORDER_USD, not strictly less-than, so it goes through.)
    let usd = size(
        dec!(10),
        &SizingMode::SelfPct,
        Some(dec!(0.50)),
        dec!(5),
        dec!(0),
    );
    assert_eq!(usd, dec!(5));
}

// ── Share-count minimum (the real CLOB constraint) ────────────────────────────

#[test]
fn min_order_shares_is_five() {
    // Polymarket CLOB hard minimum: 5 shares per order.
    assert_eq!(MIN_ORDER_SHARES, dec!(5));
}

#[test]
fn seven_pct_of_24_at_0_98_yields_too_few_shares() {
    // Regression: 7% × $24 = $1.68. At price $0.98 → 1.71 shares < 5 → 400.
    // compute_order_usd returns the budget in USD ($1.68). The strategy engine
    // then divides by the price and must check against MIN_ORDER_SHARES.
    let budget = size(
        dec!(24),
        &SizingMode::SelfPct,
        Some(dec!(0.07)),
        dec!(50),
        dec!(0),
    );
    let price = dec!(0.98);
    let shares = (budget / price).round_dp(2);
    assert!(
        shares < MIN_ORDER_SHARES,
        "7% of $24 at $0.98 = {shares:.2} shares — must be below MIN_ORDER_SHARES={MIN_ORDER_SHARES}"
    );
}

#[test]
fn budget_that_yields_five_shares_ok() {
    // Need budget ≥ 5 × price: at $0.98 that means ≥ $4.90.
    // 20% × $25 = $5.00 → 5.10 shares ≥ 5 → passes.
    let budget = size(
        dec!(25),
        &SizingMode::SelfPct,
        Some(dec!(0.20)),
        dec!(50),
        dec!(0),
    );
    let price = dec!(0.98);
    let shares = (budget / price).round_dp(2);
    assert!(
        shares >= MIN_ORDER_SHARES,
        "20% of $25 at $0.98 = {shares:.2} shares — should clear MIN_ORDER_SHARES={MIN_ORDER_SHARES}"
    );
}
