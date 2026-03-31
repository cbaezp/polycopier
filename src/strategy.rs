use crate::clients::OrderSubmitter;
use crate::config::Config;
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
    match side {
        TradeSide::BUY => price + (price * slippage_pct),
        TradeSide::SELL => price - (price * slippage_pct),
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

                // Determine limit price with slippage
                let limit_price = if event.side == TradeSide::BUY {
                    event.price + (event.price * config.max_slippage_pct)
                } else {
                    event.price - (event.price * config.max_slippage_pct)
                };

                let actual_size = if is_closing {
                    // Position existence was already verified above — our_held_size > 0 guaranteed.
                    let fee_factor = Decimal::new(97, 2); // 0.97 — CLOB fee buffer for SELLs
                    let our_held_size = {
                        let guard = state.read().await;
                        guard
                            .positions
                            .get(&event.token_id)
                            .map(|p| p.size)
                            .unwrap_or(Decimal::ZERO)
                    };
                    (our_held_size * fee_factor).round_dp(2)
                } else {
                    // BUY: cap to max_trade_size_usd
                    let size_cost = event.size * event.price;
                    if size_cost > config.max_trade_size_usd {
                        config.max_trade_size_usd / event.price
                    } else {
                        event.size
                    }
                };

                let order = OrderRequest {
                    token_id: event.token_id.clone(),
                    price: limit_price,
                    size: actual_size,
                    side: event.side,
                };

                let submitter_clone = submitter.clone();
                tokio::spawn(async move {
                    if let Err(e) = submitter_clone(order).await {
                        tracing::error!("Execution failed: {}", e);
                    }
                });
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
}
