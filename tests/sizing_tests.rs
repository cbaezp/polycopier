//! Tests for `compute_order_usd` -- all SizingMode variants.
//!
//! Covers: Fixed, SelfPct, TargetUsd, floor/cap guards, misconfigured max < min.

mod common;

use polycopier::models::SizingMode;
use polycopier::strategy::{compute_order_usd, MIN_ORDER_USD};
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
fn self_pct_floored_at_5_usd() {
    // 10% of $20 = $2, below CLOB $5 minimum
    let usd = size(
        dec!(20),
        &SizingMode::SelfPct,
        Some(dec!(0.10)),
        dec!(50),
        dec!(0),
    );
    assert_eq!(usd, MIN_ORDER_USD);
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
fn target_usd_floored_at_clob_minimum() {
    // Target bet $1 -- below CLOB minimum, floor to $5
    let usd = size(dec!(500), &SizingMode::TargetUsd, None, dec!(50), dec!(1));
    assert_eq!(usd, MIN_ORDER_USD);
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
fn max_less_than_min_clob_floor_uses_max_as_ceiling() {
    // Misconfigured: max_trade = $3, below $5 CLOB minimum
    // floor = min($5, $3) = $3; output is clamped to $3
    let usd = size(dec!(100), &SizingMode::Fixed, None, dec!(3), dec!(0));
    assert_eq!(usd, dec!(3));
}

#[test]
fn self_pct_misconfigured_max_uses_max_as_floor() {
    // 50% of $10 = $5 -- equal to cap at $5 (max < CLOB min)
    let usd = size(
        dec!(10),
        &SizingMode::SelfPct,
        Some(dec!(0.50)),
        dec!(5),
        dec!(0),
    );
    assert_eq!(usd, dec!(5));
}
