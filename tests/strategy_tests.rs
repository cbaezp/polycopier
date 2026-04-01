//! Tests for strategy engine helpers: limit price calculation, entry sizing,
//! and the compute_order_usd sizing function.

use polycopier::models::TradeSide;
use polycopier::strategy::{calculate_entry_size, calculate_limit_price};
use rust_decimal_macros::dec;

#[test]
fn buy_limit_price_adds_slippage() {
    let price = dec!(0.50);
    let slippage = dec!(0.02);
    let result = calculate_limit_price(price, TradeSide::BUY, slippage);
    assert_eq!(result, dec!(0.51));
}

#[test]
fn sell_limit_price_subtracts_slippage() {
    let price = dec!(0.50);
    let slippage = dec!(0.02);
    let result = calculate_limit_price(price, TradeSide::SELL, slippage);
    assert_eq!(result, dec!(0.49));
}

#[test]
fn entry_size_within_budget_unchanged() {
    // 10 shares at $0.40 = $4 -- under $10 max
    let result = calculate_entry_size(dec!(10), dec!(0.40), dec!(10));
    assert_eq!(result, dec!(10));
}

#[test]
fn entry_size_capped_to_max_usd() {
    // 100 shares at $0.40 = $40 -- over $10 max => 10/0.40 = 25 shares
    let result = calculate_entry_size(dec!(100), dec!(0.40), dec!(10));
    assert_eq!(result, dec!(25));
}

#[test]
fn zero_slippage_keeps_price_unchanged() {
    let price = dec!(0.77);
    let result = calculate_limit_price(price, TradeSide::BUY, dec!(0));
    assert_eq!(result, price);
}
