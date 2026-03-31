use crate::clients::OrderSubmitter;
use crate::config::{Config, SizingMode};
use crate::models::{EvaluatedTrade, OrderRequest, TradeEvent, TradeSide};
use crate::risk::RiskEngine;
use crate::state::BotState;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

// ── Pure helpers (extracted for testability) ──────────────────────────────────

/// Applies slippage to a price to produce the limit order price.
pub fn calculate_limit_price(price: Decimal, side: TradeSide, slippage_pct: Decimal) -> Decimal {
    // Polymarket CLOB tick size is 0.001 — prices must have at most 3 decimal places.
    // Token prices are also bounded [0.001, 0.999] for an open binary market.
    let max_price = Decimal::new(999, 3); // 0.999
    let min_price = Decimal::new(1, 3); // 0.001
    match side {
        TradeSide::BUY => (price + price * slippage_pct).round_dp(3).min(max_price),
        TradeSide::SELL => (price - price * slippage_pct).round_dp(3).max(min_price),
    }
}

/// Caps entry size to max_trade_usd / price, returns the original size if within budget.
pub fn calculate_entry_size(size: Decimal, price: Decimal, max_trade_usd: Decimal) -> Decimal {
    let cost = size * price;
    if cost > max_trade_usd {
        max_trade_usd / price
    } else {
        size
    }
}

/// Minimum notional the CLOB requires (5 shares × ~$1.00 = $5.00).
pub const MIN_ORDER_USD: Decimal = Decimal::from_parts(5, 0, 0, false, 0); // 5.00

/// Determine how many USD to spend on a BUY order — BalancePct and Fixed modes.
/// For MirrorTarget, use `compute_mirror_order_usd` instead.
///
/// Rules:
/// 1. `BalancePct(p)` → desired = balance × p
/// 2. `Fixed` (or MirrorTarget fallback) → desired = max_trade_usd
/// 3. Floored at MIN_ORDER_USD ($5), capped at max_trade_usd.
pub fn compute_order_usd(
    balance: Decimal,
    copy_size_pct: Option<Decimal>, // kept for unit-test compatibility
    max_trade_usd: Decimal,
) -> Decimal {
    let desired = match copy_size_pct {
        Some(pct) => balance * pct,
        None => max_trade_usd,
    };
    let floor = MIN_ORDER_USD.min(max_trade_usd);
    desired.max(floor).min(max_trade_usd)
}

/// Mirror the target's portfolio allocation.
///
/// Formula: `ratio = target_trade_usd / target_portfolio_usd`
/// then: `our_trade_usd = our_balance × ratio`
///
/// Falls back to `max_trade_usd` (fixed mode) when `target_portfolio_usd` is zero
/// (scanner hasn't populated target positions yet).
/// Result is clamped to `[MIN_ORDER_USD, max_trade_usd]`.
pub fn compute_mirror_order_usd(
    our_balance: Decimal,
    target_trade_usd: Decimal, // event.size × event.price for live trades
    target_portfolio_usd: Decimal, // BotState::target_portfolio_usd()
    max_trade_usd: Decimal,
) -> Decimal {
    if target_portfolio_usd <= Decimal::ZERO {
        // Target data not yet available — fall back to fixed sizing
        let floor = MIN_ORDER_USD.min(max_trade_usd);
        return max_trade_usd.max(floor).min(max_trade_usd);
    }
    let ratio = target_trade_usd / target_portfolio_usd;
    let desired = our_balance * ratio;
    let floor = MIN_ORDER_USD.min(max_trade_usd);
    desired.max(floor).min(max_trade_usd)
}

/// Resolve how much USD to spend for a BUY, dispatching on the configured SizingMode.
///
/// `target_trade_usd`      = event.size × event.price (the target's notional)
/// `target_portfolio_usd`  = BotState::target_portfolio_usd() (scanner-cached)
pub fn resolve_budget_usd(
    sizing_mode: &SizingMode,
    our_balance: Decimal,
    target_trade_usd: Decimal,
    target_portfolio_usd: Decimal,
    max_trade_usd: Decimal,
) -> Decimal {
    let floor = MIN_ORDER_USD.min(max_trade_usd);
    match sizing_mode {
        SizingMode::Fixed => max_trade_usd.max(floor).min(max_trade_usd),
        SizingMode::BalancePct(pct) => compute_order_usd(our_balance, Some(*pct), max_trade_usd),
        SizingMode::MirrorPct => compute_mirror_order_usd(
            our_balance,
            target_trade_usd,
            target_portfolio_usd,
            max_trade_usd,
        ),
        SizingMode::MirrorAbsolute => {
            // Copy the target's exact notional, clamped to [MIN_ORDER_USD, max_trade_usd].
            target_trade_usd.max(floor).min(max_trade_usd)
        }
    }
}

pub fn start_strategy_engine(
    mut rx: mpsc::Receiver<TradeEvent>,
    state: Arc<RwLock<BotState>>,
    mut risk_engine: RiskEngine,
    submitter: OrderSubmitter,
    config: Config,
) {
    tokio::spawn(async move {
        info!("Strategy Engine Started. Monitoring edge cases (debouncing, closures...)");

        // Target -> AssetID -> Token Info/Debounce Context
        let mut debounce_cache: HashMap<String, TradeEvent> = HashMap::new();

        while let Some(event) = rx.recv().await {
            let mut eval = EvaluatedTrade {
                original_event: event.clone(),
                validated: true,
                reason: None,
            };

            // 1. Is it a filled trade from the target wallet list?
            if !config.target_wallets.contains(&event.taker_address) {
                eval.validated = false;
                eval.reason = Some("Wallet mismatch".to_string());
            }

            // 4. Fragmented Fill Edge Case (Debounce 200ms)
            // A simplified debounce: Just track timestamp diff. If same token < 1 sec, accumulate sizes.
            let cache_key = format!(
                "{}_{}_{:?}",
                event.taker_address, event.token_id, event.side
            );
            if eval.validated {
                if let Some(existing) = debounce_cache.get_mut(&cache_key) {
                    if (chrono::Utc::now().timestamp() - existing.timestamp) < 1 {
                        existing.size += event.size;
                        debug!(
                            "Debounced fragmented fill for {}. New size: {}",
                            existing.token_id, existing.size
                        );
                        continue;
                    } else {
                        // Expired, flush it out
                        debounce_cache.insert(cache_key.clone(), event.clone());
                    }
                } else {
                    debounce_cache.insert(cache_key.clone(), event.clone());
                }
            }

            // Risk bounds
            if eval.validated {
                if let Err(reason) = risk_engine.check_trade(&event) {
                    eval.validated = false;
                    eval.reason = Some(reason);
                }
            }

            // ── Intent classification using target's scanner positions ──────────
            // The position scanner refreshes target_positions every 60 seconds.
            // We use it to distinguish fresh entries from closures, and to detect
            // short positions (SELL to open, BUY to close) that we cannot replicate.
            if eval.validated {
                let (target_holds, we_hold) = {
                    let guard = state.read().await;
                    let target = guard
                        .target_positions
                        .iter()
                        .any(|p| p.token_id == event.token_id);
                    let ours = guard.positions.contains_key(&event.token_id);
                    (target, ours)
                };

                // Only apply intent classification when scanner has populated data.
                // If target_positions is empty the scanner hasn't run yet — fall back
                // to side-based logic (safe: SELLs still require us to hold the token).
                let scanner_ready = {
                    let guard = state.read().await;
                    !guard.target_positions.is_empty()
                };

                let skip_reason: Option<&str> = if scanner_ready {
                    match event.side {
                        TradeSide::BUY => {
                            if !target_holds && we_hold {
                                // We're already long, target has no position →
                                // target is likely closing a short we never entered.
                                Some("BUY skipped: we hold long but target has no position (short close)")
                            } else {
                                None // Fresh long entry or adding to long → copy
                            }
                        }
                        TradeSide::SELL => {
                            if target_holds && we_hold {
                                None // Target closing their long, we hold → copy
                            } else if target_holds && !we_hold {
                                Some("SELL skipped: target closing long we never entered")
                            } else {
                                // !target_holds → target opening a short (no prior long position)
                                Some("SELL skipped: target opening short position (not supported)")
                            }
                        }
                    }
                } else {
                    // Scanner not yet populated — fall back to: only skip SELLs we don't hold
                    if event.side == TradeSide::SELL && !we_hold {
                        Some("SELL skipped: position not held (scanner warming up)")
                    } else {
                        None
                    }
                };

                if let Some(reason) = skip_reason {
                    warn!("{}", reason);
                    eval.validated = false;
                    eval.reason = Some(reason.to_string());
                }
            }

            // Update TUI feed (single push, correct validated state)
            {
                let mut guard = state.write().await;
                guard.push_evaluated_trade(eval.clone());
            }

            if eval.validated {
                info!("Trade Validated: {:?}", eval.original_event);

                let is_closing = event.side == TradeSide::SELL;

                // Determine limit price: rounded to 3dp (CLOB tick), capped to [0.001, 0.999]
                let limit_price =
                    calculate_limit_price(event.price, event.side, config.max_slippage_pct);

                let order = if is_closing {
                    // ── SELL: close our position using our held size (not the target's size) ──
                    let fee_factor = Decimal::new(97, 2); // 0.97 — CLOB fee buffer for SELLs
                    let our_held_size = {
                        let guard = state.read().await;
                        guard
                            .positions
                            .get(&event.token_id)
                            .map(|p| p.size)
                            .unwrap_or(Decimal::ZERO)
                    };
                    let sell_size = (our_held_size * fee_factor).round_dp(2);
                    Some(OrderRequest {
                        token_id: event.token_id.clone(),
                        price: limit_price,
                        size: sell_size,
                        side: event.side,
                    })
                } else {
                    // ── BUY: sizing dispatched on SizingMode ──
                    let (current_balance, target_portfolio_usd) = {
                        let guard = state.read().await;
                        (guard.total_balance, guard.target_portfolio_usd())
                    };
                    let target_trade_usd = event.size * event.price;
                    let budget_usd = resolve_budget_usd(
                        &config.sizing_mode,
                        current_balance,
                        target_trade_usd,
                        target_portfolio_usd,
                        config.max_trade_size_usd,
                    );
                    let size_cost = event.size * event.price;
                    let raw_size = if size_cost > budget_usd {
                        budget_usd / event.price
                    } else {
                        event.size
                    };
                    let buy_size = raw_size.round_dp(2); // CLOB requires 2dp lot size

                    // Pre-check balance — avoids noisy 400 errors from CLOB
                    let order_cost = buy_size * limit_price;
                    if current_balance < order_cost {
                        warn!(
                            "Insufficient balance (have ${:.2}, need ${:.2}) — skipping entry",
                            current_balance, order_cost
                        );
                        None
                    } else {
                        Some(OrderRequest {
                            token_id: event.token_id.clone(),
                            price: limit_price,
                            size: buy_size,
                            side: event.side,
                        })
                    }
                };

                if let Some(order) = order {
                    let submitter_clone = submitter.clone();
                    tokio::spawn(async move {
                        if let Err(e) = submitter_clone(order).await {
                            tracing::error!("Execution failed: {}", e);
                        }
                    });
                }
            } else {
                warn!("Skipped trade: {}", eval.reason.unwrap_or_default());
            }
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
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
        // 10 shares at $0.40 = $4 — under $10 max
        let result = calculate_entry_size(dec!(10), dec!(0.40), dec!(10));
        assert_eq!(result, dec!(10));
    }

    #[test]
    fn entry_size_capped_to_max_usd() {
        // 100 shares at $0.40 = $40 — over $10 max => 10/0.40 = 25 shares
        let result = calculate_entry_size(dec!(100), dec!(0.40), dec!(10));
        assert_eq!(result, dec!(25));
    }

    #[test]
    fn zero_slippage_keeps_price_unchanged() {
        let price = dec!(0.77);
        let result = calculate_limit_price(price, TradeSide::BUY, dec!(0));
        assert_eq!(result, price);
    }

    // ── compute_order_usd tests ─────────────────────────────────────────────

    #[test]
    fn proportional_sizing_uses_pct_of_balance() {
        // 10% of $200 = $20, within $50 cap
        let usd = compute_order_usd(dec!(200), Some(dec!(0.10)), dec!(50));
        assert_eq!(usd, dec!(20));
    }

    #[test]
    fn proportional_sizing_capped_at_max() {
        // 10% of $1000 = $100, but cap is $50
        let usd = compute_order_usd(dec!(1000), Some(dec!(0.10)), dec!(50));
        assert_eq!(usd, dec!(50));
    }

    #[test]
    fn proportional_sizing_floored_at_min() {
        // 10% of $20 = $2, below $5 minimum → should be $5
        let usd = compute_order_usd(dec!(20), Some(dec!(0.10)), dec!(50));
        assert_eq!(usd, dec!(5));
    }

    #[test]
    fn fixed_sizing_uses_max_trade_usd_when_no_pct() {
        let usd = compute_order_usd(dec!(500), None, dec!(10));
        assert_eq!(usd, dec!(10));
    }

    #[test]
    fn floor_applied_when_balance_very_low() {
        // 10% of $10 = $1, below $5 floor → should try $5 (still within $50 cap)
        let usd = compute_order_usd(dec!(10), Some(dec!(0.10)), dec!(50));
        assert_eq!(usd, dec!(5));
    }

    #[test]
    fn max_less_than_min_uses_max_as_floor() {
        // Misconfigured: max_trade = $3, below MIN_ORDER_USD $5.
        // floor = min($5, $3) = $3; result clamped to $3.
        let usd = compute_order_usd(dec!(100), Some(dec!(0.10)), dec!(3));
        assert_eq!(usd, dec!(3));
    }

    #[test]
    fn pct_exactly_at_max_returns_max() {
        // 50% of $100 = $50 = cap exactly
        let usd = compute_order_usd(dec!(100), Some(dec!(0.50)), dec!(50));
        assert_eq!(usd, dec!(50));
    }

    // ── compute_mirror_order_usd tests ─────────────────────────────────────────

    #[test]
    fn mirror_normal_case() {
        // Target portfolio: $1000. Target trade: $100 (10%).
        // Our balance: $500. Expected: $500 × 10% = $50.
        let usd = compute_mirror_order_usd(dec!(500), dec!(100), dec!(1000), dec!(200));
        assert_eq!(usd, dec!(50));
    }

    #[test]
    fn mirror_falls_back_to_fixed_when_portfolio_is_zero() {
        // No scanner data yet — should return max_trade_usd.
        let usd = compute_mirror_order_usd(dec!(500), dec!(100), dec!(0), dec!(25));
        assert_eq!(usd, dec!(25));
    }

    #[test]
    fn mirror_floored_at_min_order_usd() {
        // Target portfolio: $10000. Target trade: $10 (0.1%).
        // Our balance: $50. Expected desired = $50 × 0.001 = $0.05 → floored to $5.
        let usd = compute_mirror_order_usd(dec!(50), dec!(10), dec!(10000), dec!(50));
        assert_eq!(usd, dec!(5));
    }

    #[test]
    fn mirror_capped_at_max_trade_usd() {
        // Target portfolio: $1000. Target trade: $900 (90%).
        // Our balance: $500. Desired = $450. Cap = $50.
        let usd = compute_mirror_order_usd(dec!(500), dec!(900), dec!(1000), dec!(50));
        assert_eq!(usd, dec!(50));
    }

    #[test]
    fn mirror_small_ratio_floored() {
        // Target commits 0.5% of her $200k portfolio = $1000 per trade.
        // We have $30. Desired = $30 × 0.005 = $0.15 → floored to $5.
        let usd = compute_mirror_order_usd(dec!(30), dec!(1000), dec!(200000), dec!(50));
        assert_eq!(usd, dec!(5));
    }

    #[test]
    fn mirror_exact_parity() {
        // Same portfolio value — same absolute trade size → ratio 1:1 but capped.
        // Target portfolio: $100, trade: $10 (10%). Our balance: $100.
        // Desired = $100 × 10% = $10. Cap $50 → $10.
        let usd = compute_mirror_order_usd(dec!(100), dec!(10), dec!(100), dec!(50));
        assert_eq!(usd, dec!(10));
    }

    // ── MirrorAbsolute (via resolve_budget_usd) ────────────────────────────────

    #[test]
    fn mirror_absolute_copies_exact_target_amount() {
        // Target bet $18 → we bet $18 (within cap of $50).
        let usd = resolve_budget_usd(
            &SizingMode::MirrorAbsolute,
            dec!(500),  // our balance (ignored for absolute)
            dec!(18),   // target_trade_usd
            dec!(2000), // target_portfolio_usd (ignored for absolute)
            dec!(50),
        );
        assert_eq!(usd, dec!(18));
    }

    #[test]
    fn mirror_absolute_floored_when_target_bet_tiny() {
        // Target bet $1 (below CLOB minimum) → floored to $5.
        let usd = resolve_budget_usd(
            &SizingMode::MirrorAbsolute,
            dec!(500),
            dec!(1),
            dec!(2000),
            dec!(50),
        );
        assert_eq!(usd, dec!(5));
    }

    #[test]
    fn mirror_absolute_capped_when_target_bet_large() {
        // Target bet $200 but our cap is $50 → capped to $50.
        let usd = resolve_budget_usd(
            &SizingMode::MirrorAbsolute,
            dec!(500),
            dec!(200),
            dec!(2000),
            dec!(50),
        );
        assert_eq!(usd, dec!(50));
    }
}
