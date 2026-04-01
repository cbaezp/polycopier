use crate::clients::OrderSubmitter;
use crate::config::Config;
use crate::copy_ledger::CopyLedger;
use crate::models::{EvaluatedTrade, OrderRequest, SizingMode, TradeEvent, TradeSide};
use crate::risk::RiskEngine;
use crate::state::BotState;
use alloy::primitives::Address;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::Duration;
use tracing::{debug, info, warn};

// -- Pure helpers (extracted for testability) ----------------------------------

/// Applies slippage to a price to produce the limit order price.
pub fn calculate_limit_price(price: Decimal, side: TradeSide, slippage_pct: Decimal) -> Decimal {
    // Most Polymarket markets use tick size 0.01 (2 decimal places).
    // A small number of high-liquidity markets use 0.001 (3 dp).
    // Rounding to 2 dp works for all markets: the CLOB accepts 2 dp
    // even when tick size is 0.001, but rejects 3 dp on 0.01-tick markets.
    let max_price = Decimal::new(99, 2); // 0.99
    let min_price = Decimal::new(1, 2); // 0.01
    match side {
        TradeSide::BUY => (price + price * slippage_pct).round_dp(2).min(max_price),
        TradeSide::SELL => (price - price * slippage_pct).round_dp(2).max(min_price),
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

/// Minimum share count the Polymarket CLOB enforces per order.
/// Any order with size < 5 shares gets a 400: "Size (X) lower than the minimum: 5"
pub const MIN_ORDER_SHARES: Decimal = Decimal::from_parts(5, 0, 0, false, 0);

/// Dollar floor (secondary sanity guard — the real constraint is MIN_ORDER_SHARES).
pub const MIN_ORDER_USD: Decimal = Decimal::from_parts(1, 0, 0, false, 0);

/// Compute the USD budget for a single BUY order according to the active [`SizingMode`].
///
/// | Mode | Formula |
/// |---|---|
/// | `Fixed` | `max_trade_usd` (constant) |
/// | `SelfPct` | `our_balance * copy_size_pct`, capped at `max_trade_usd` |
/// | `TargetUsd` | `target_notional` (exact $ the target bet), capped at `max_trade_usd` |
///
/// Returns **`Decimal::ZERO`** when the computed budget is below `MIN_ORDER_USD` ($1).
/// Callers treat zero as "skip this order". This respects the user's configured
/// percentage exactly: 7% of $39 = $2.73 ≥ $1 → order placed correctly.
pub fn compute_order_usd(
    our_balance: Decimal,
    sizing_mode: &SizingMode,
    copy_size_pct: Option<Decimal>,
    max_trade_usd: Decimal,
    target_notional: Decimal,
) -> Decimal {
    let desired = match sizing_mode {
        SizingMode::Fixed => max_trade_usd,
        SizingMode::SelfPct => {
            let pct =
                copy_size_pct.unwrap_or_else(|| max_trade_usd / our_balance.max(Decimal::ONE));
            our_balance * pct
        }
        SizingMode::TargetUsd => target_notional,
    };
    let capped = desired.min(max_trade_usd);
    // Return ZERO to signal "skip" when below the CLOB minimum.
    // Never floor UP — that silently overrides the user's sizing config.
    if capped < MIN_ORDER_USD {
        Decimal::ZERO
    } else {
        capped
    }
}

// ---------------------------------------------------------------------------
// Injectable live position checker
// ---------------------------------------------------------------------------

/// Returns `Some(true)` if `wallet` holds `token_id`, `Some(false)` if not,
/// `None` on error or timeout.
///
/// Injected via [`start_strategy_engine`] so tests can provide a no-op
/// without touching the real network.
pub type HoldsQuery =
    Arc<dyn Fn(String, String) -> Pin<Box<dyn Future<Output = Option<bool>> + Send>> + Send + Sync>;

/// Production implementation — makes a live Polymarket Data API call with a
/// 5-second timeout.
pub fn make_live_holds_query() -> HoldsQuery {
    Arc::new(|wallet: String, token_id: String| {
        Box::pin(async move {
            let client = DataClient::default();
            let Ok(addr) = Address::from_str(&wallet) else {
                return None;
            };
            let req = PositionsRequest::builder().user(addr).build();
            match tokio::time::timeout(Duration::from_secs(5), client.positions(&req)).await {
                Ok(Ok(positions)) => {
                    Some(positions.iter().any(|p| p.asset.to_string() == token_id))
                }
                Ok(Err(e)) => {
                    warn!(
                        "Live position query failed for {}: {e}",
                        &wallet[..wallet.len().min(10)]
                    );
                    None
                }
                Err(_) => {
                    warn!(
                        "Live position query timed out for {}",
                        &wallet[..wallet.len().min(10)]
                    );
                    None
                }
            }
        })
    })
}

/// No-op implementation for use in integration tests.  Returns `None`
/// immediately, triggering the scanner-cache fallback in the engine.
pub fn make_no_op_holds_query() -> HoldsQuery {
    Arc::new(|_wallet: String, _token_id: String| Box::pin(async { None::<bool> }))
}

pub fn start_strategy_engine(
    mut rx: mpsc::Receiver<TradeEvent>,
    state: Arc<RwLock<BotState>>,
    mut risk_engine: RiskEngine,
    submitter: OrderSubmitter,
    config: Config,
    copy_ledger: Arc<Mutex<CopyLedger>>,
    holds_query: HoldsQuery,
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

            // -- Intent classification: live API + copy ledger ------------------
            //
            // Rules:
            //   ONE-POSITION-PER-TOKEN: once we hold a token (from any target),
            //   ignore BUY events from all other targets for that token.
            //
            //   SELL gating: only the target we originally copied from can trigger
            //   a close.  A SELL from a different target is irrelevant to our
            //   position.
            //
            //   LIVE VERIFICATION: for BUY events we query the target's wallet live
            //   (they should show the position after buying); for SELL events we
            //   query OUR wallet live (authoritative — did we actually hold it?).
            //   Both calls fall back to the scanner cache on API timeout/error.
            if eval.validated {
                // --- Resolve live state for this token ---

                // Our position: query live (both calls run in parallel),
                // falling back to the scanner cache on API timeout/error.
                let cache_we_hold = {
                    let guard = state.read().await;
                    guard.positions.contains_key(&event.token_id)
                };
                let cache_target_holds = {
                    let guard = state.read().await;
                    guard.target_positions.iter().any(|p| {
                        p.token_id == event.token_id && p.source_wallet == event.taker_address
                    })
                };

                // Run both live queries in parallel to minimise latency.
                let (live_we_hold_opt, live_target_holds_opt) = tokio::join!(
                    holds_query(config.funder_address.clone(), event.token_id.clone()),
                    holds_query(event.taker_address.clone(), event.token_id.clone()),
                );
                let live_we_hold = live_we_hold_opt.unwrap_or(cache_we_hold);
                let live_target_holds = live_target_holds_opt.unwrap_or(cache_target_holds);

                // Ledger lookup for this token (who we copied it from, if anyone).
                let ledger_entry = {
                    let ledger = copy_ledger.lock().await;
                    ledger.find_active_for_token(&event.token_id).cloned()
                };
                let already_in_token = ledger_entry.is_some();

                let skip_reason: Option<String> = match event.side {
                    // ---- BUY -----------------------------------------------
                    TradeSide::BUY => {
                        if already_in_token {
                            // ONE-POSITION-PER-TOKEN: we already hold this via
                            // another target (or via this same target's prior entry).
                            let from = ledger_entry
                                .as_ref()
                                .map(|e| &e.source_wallet[..e.source_wallet.len().min(10)])
                                .unwrap_or("unknown");
                            Some(format!(
                                "BUY skipped: already holding token {} (entered from {})",
                                &event.token_id[..event.token_id.len().min(12)],
                                from
                            ))
                        } else if !live_target_holds && live_we_hold {
                            // Target has no position but we're long → target closing
                            // their short; copying this BUY would incorrectly add to
                            // our long.
                            Some(
                                "BUY skipped: we hold long but target has no position \
                                 (likely closing their short)"
                                    .to_string(),
                            )
                        } else {
                            None // Fresh long entry → copy
                        }
                    }
                    // ---- SELL ----------------------------------------------
                    TradeSide::SELL => {
                        if live_we_hold {
                            // We hold this position.  Determine if the seller is the
                            // target we copied it from.
                            match &ledger_entry {
                                Some(entry) if entry.source_wallet == event.taker_address => {
                                    None // Correct source is selling → close
                                }
                                Some(entry) => {
                                    // A *different* target is selling — their action
                                    // is irrelevant to the position we copied from
                                    // entry.source_wallet.  Keep holding.
                                    Some(format!(
                                        "SELL skipped: {} sold but we copied from {} — \
                                         keeping position",
                                        &event.taker_address[..event.taker_address.len().min(10)],
                                        &entry.source_wallet[..entry.source_wallet.len().min(10)],
                                    ))
                                }
                                None => {
                                    // We hold it but have no ledger entry — either the
                                    // ledger was lost or the position was entered manually.
                                    // Defensive: close it when ANY target sells.
                                    warn!(
                                        "SELL: we hold token {} with no ledger entry — \
                                         closing defensively.",
                                        &event.token_id[..event.token_id.len().min(12)]
                                    );
                                    None
                                }
                            }
                        } else if let Some(entry) = &ledger_entry {
                            // Ledger says we had a copy but live API shows we don't hold it.
                            // Position was closed while bot was down — sync ledger and skip.
                            warn!(
                                "SELL: ledger shows active copy of {} from {} but we no longer \
                                 hold it — marking closed.",
                                &event.token_id[..event.token_id.len().min(12)],
                                &entry.source_wallet[..entry.source_wallet.len().min(10)],
                            );
                            let mut ledger = copy_ledger.lock().await;
                            ledger.record_close(&event.token_id, &entry.source_wallet.clone());
                            Some(
                                "SELL skipped: position already closed (ledger synced)".to_string(),
                            )
                        } else {
                            // We don't hold it and there's no ledger entry.
                            // Target is either closing a long we never entered (bot
                            // was down during entry) or opening a short.
                            if live_target_holds {
                                Some(
                                    "SELL skipped: target closing long we never entered"
                                        .to_string(),
                                )
                            } else {
                                Some(
                                    "SELL skipped: target opening short (not supported)"
                                        .to_string(),
                                )
                            }
                        }
                    }
                };

                if let Some(reason) = skip_reason {
                    warn!("{}", reason);
                    eval.validated = false;
                    eval.reason = Some(reason);
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
                    // -- SELL: close our position using our held size (not the target's size) --
                    let fee_factor = Decimal::new(97, 2); // 0.97 -- CLOB fee buffer for SELLs
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
                    // -- BUY: size according to active SizingMode, capped and $5 floored --
                    let current_balance = {
                        let guard = state.read().await;
                        guard.total_balance
                    };
                    // target_notional = what the target just bet in dollar terms
                    let target_notional = event.size * event.price;
                    let budget_usd = compute_order_usd(
                        current_balance,
                        &config.sizing_mode,
                        config.copy_size_pct,
                        config.max_trade_size_usd,
                        target_notional,
                    );
                    let raw_size = if target_notional > budget_usd {
                        budget_usd / event.price
                    } else {
                        event.size
                    };
                    let buy_size = raw_size.round_dp(2); // CLOB requires 2dp lot size

                    // CLOB hard minimum: 5 shares. Orders below this always 400.
                    // With e.g. 7% of $24 = $1.68 at price $0.98 → 1.71 shares < 5 → skip.
                    if buy_size < MIN_ORDER_SHARES {
                        warn!(
                            "BUY skipped: computed {:.2} shares is below CLOB minimum of {} shares \
                             (budget=${:.2} at price ${:.3}). Increase COPY_SIZE_PCT or wait for higher balance.",
                            buy_size, MIN_ORDER_SHARES, budget_usd, limit_price
                        );
                        None
                    } else {
                        // Pre-check balance -- avoids noisy 400 errors from CLOB
                        let order_cost = buy_size * limit_price;
                        if current_balance < order_cost {
                            warn!(
                                "Insufficient balance (have ${:.2}, need ${:.2}) -- skipping entry",
                                current_balance, order_cost
                            );
                            None
                        } else {
                            // Check whether we already have a pending GTC order for this token.
                            // Catches WS-triggered events for markets with an existing live GTC order.
                            let already_pending = {
                                let guard = state.read().await;
                                guard.pending_order_tokens.contains(&event.token_id)
                            };
                            if already_pending {
                                warn!(
                                    "BUY skipped: live GTC order already exists for token {}",
                                    event.token_id
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
                        }
                    } // end MIN_ORDER_SHARES check
                };

                if let Some(order) = order {
                    // Register token as pending BEFORE spawning so any concurrent
                    // events for the same token are blocked immediately.
                    {
                        let mut guard = state.write().await;
                        guard.pending_order_tokens.insert(order.token_id.clone());
                    }

                    let submitter_clone = submitter.clone();
                    let state_clone = state.clone();
                    let token_id_clone = order.token_id.clone();
                    let source_wallet_clone = event.taker_address.clone();
                    let is_closing = event.side == TradeSide::SELL;
                    let order_size = order.size;
                    let order_price = order.price;
                    let ledger_clone = copy_ledger.clone();

                    tokio::spawn(async move {
                        match submitter_clone(order).await {
                            Ok(()) => {
                                // Record in the copy ledger so subsequent events for this
                                // token are classified correctly (source wallet tracking,
                                // one-position-per-token rule, SELL gating).
                                let mut ledger = ledger_clone.lock().await;
                                if is_closing {
                                    ledger.record_close(&token_id_clone, &source_wallet_clone);
                                    info!(
                                        "Ledger: closed {} from {}",
                                        &token_id_clone[..token_id_clone.len().min(12)],
                                        &source_wallet_clone[..source_wallet_clone.len().min(10)],
                                    );
                                } else {
                                    ledger.record_copy(
                                        token_id_clone.clone(),
                                        source_wallet_clone.clone(),
                                        order_size,
                                        order_price,
                                    );
                                    info!(
                                        "Ledger: recorded copy of {} from {}",
                                        &token_id_clone[..token_id_clone.len().min(12)],
                                        &source_wallet_clone[..source_wallet_clone.len().min(10)],
                                    );
                                }
                            }
                            Err(e) => {
                                // Remove from pending on failure so the order can be retried.
                                let mut guard = state_clone.write().await;
                                guard.pending_order_tokens.remove(&token_id_clone);
                                tracing::error!("Execution failed: {}", e);
                            }
                        }
                    });
                }
            } else {
                warn!("Skipped trade: {}", eval.reason.unwrap_or_default());
            }
        }
    });
}
