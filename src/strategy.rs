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
use std::time::Instant;
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
    wallet_scalar: Decimal,
    mut max_trade_usd: Decimal,
    target_notional: Decimal,
) -> Decimal {
    // If the user's available balance is smaller than the hard max ceiling,
    // gracefully scale down the max ceiling to their available balance
    // minus an estimated 2% fee overhead buffer (divide by 1.02),
    // ensuring we still enter the trade with "all we have" rather than erroring out.
    let usable_balance = our_balance / Decimal::from_str("1.02").unwrap();
    if usable_balance < max_trade_usd {
        max_trade_usd = usable_balance;
    }

    let desired = match sizing_mode {
        SizingMode::Fixed => max_trade_usd,
        SizingMode::SelfPct => {
            let pct =
                copy_size_pct.unwrap_or_else(|| max_trade_usd / our_balance.max(Decimal::ONE));
            our_balance * pct
        }
        SizingMode::TargetUsd => target_notional,
        SizingMode::TargetScalar => target_notional * wallet_scalar,
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
// Debounce cache with TTL eviction (Gap 5)
// ---------------------------------------------------------------------------

/// Maximum number of entries the debounce cache may hold simultaneously.
/// At ~3 events/s per target × 4 targets, 512 covers >40 seconds of burst.
const DEBOUNCE_CACHE_CAP: usize = 512;
/// Entries older than this are evicted unconditionally.
const DEBOUNCE_STALE_SECS: u64 = 5;
/// Events for the same key within this window are coalesced (size accumulated).
const DEBOUNCE_WINDOW_SECS: i64 = 1;

struct DebounceCache {
    map: HashMap<String, (TradeEvent, Instant)>,
}

impl DebounceCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Evict all entries older than `DEBOUNCE_STALE_SECS`.
    fn purge_stale(&mut self) {
        self.map
            .retain(|_, (_, inserted)| inserted.elapsed().as_secs() < DEBOUNCE_STALE_SECS);
    }

    /// Insert or accumulate an event.  Returns `true` if the event was debounced
    /// (accumulated into an existing entry) so the caller should `continue`.
    fn insert_or_accumulate(&mut self, key: String, event: TradeEvent) -> bool {
        // Evict stale entries before checking capacity
        self.purge_stale();

        // Enforce capacity ceiling — evict oldest entry if at cap
        if self.map.len() >= DEBOUNCE_CACHE_CAP && !self.map.contains_key(&key) {
            // Remove the entry with the oldest insertion time
            if let Some(oldest_key) = self
                .map
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest_key);
            }
        }

        if let Some((existing, inserted)) = self.map.get_mut(&key) {
            let age_secs = chrono::Utc::now().timestamp() - existing.timestamp;
            if age_secs < DEBOUNCE_WINDOW_SECS {
                // Accumulate size — still within the debounce window
                existing.size += event.size;
                debug!(
                    "Debounced fragmented fill for {}. New size: {}",
                    existing.token_id, existing.size
                );
                return true; // caller should `continue`
            } else {
                // Window expired — replace with fresh event
                *existing = event;
                *inserted = Instant::now();
                return false;
            }
        }

        self.map.insert(key, (event, Instant::now()));
        false
    }
}

// ---------------------------------------------------------------------------
// Live query cache (Gap 13)
// ---------------------------------------------------------------------------

/// Cache TTL in seconds for `holds_query` results.  Fresh enough that a trade
/// arriving 3s after a prior one for the same wallet re-uses the cached result,
/// but stale enough that a fast-moving market doesn't use a 10s-old snapshot.
const LIVE_QUERY_CACHE_TTL_SECS: u64 = 3;

struct LiveQueryCache {
    inner: HashMap<String, (bool, Instant)>,
}

impl LiveQueryCache {
    fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    fn get(&self, wallet: &str, token_id: &str) -> Option<bool> {
        let key = format!("{wallet}:{token_id}");
        self.inner.get(&key).and_then(|(result, inserted)| {
            if inserted.elapsed().as_secs() < LIVE_QUERY_CACHE_TTL_SECS {
                Some(*result)
            } else {
                None
            }
        })
    }

    fn set(&mut self, wallet: &str, token_id: &str, holds: bool) {
        let key = format!("{wallet}:{token_id}");
        self.inner.insert(key, (holds, Instant::now()));
    }

    /// Evict expired entries to keep memory bounded.
    fn evict_expired(&mut self) {
        self.inner
            .retain(|_, (_, t)| t.elapsed().as_secs() < LIVE_QUERY_CACHE_TTL_SECS * 4);
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

// ---------------------------------------------------------------------------
// Injectable market end-date checker
// ---------------------------------------------------------------------------

pub type EndDateQuery = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<chrono::DateTime<chrono::Utc>>> + Send>>
        + Send
        + Sync,
>;

pub fn make_live_end_date_query() -> EndDateQuery {
    Arc::new(|token_id: String| {
        Box::pin(async move {
            let client = polymarket_client_sdk::gamma::Client::default();
            let Ok(u) = polymarket_client_sdk::types::U256::from_str(&token_id) else {
                return None;
            };
            let req = polymarket_client_sdk::gamma::types::request::MarketsRequest::builder()
                .clob_token_ids(vec![u])
                .build();
            match tokio::time::timeout(Duration::from_secs(5), client.markets(&req)).await {
                Ok(Ok(markets)) => markets.into_iter().next().and_then(|m| m.end_date),
                Ok(Err(e)) => {
                    warn!("Live end_date query failed for {}: {}", &token_id[..10], e);
                    None
                }
                Err(_) => {
                    warn!("Live end_date query timed out for {}", &token_id[..10]);
                    None
                }
            }
        })
    })
}

pub fn make_no_op_end_date_query() -> EndDateQuery {
    Arc::new(|_token_id: String| Box::pin(async { None }))
}

// ---------------------------------------------------------------------------
// End Date Cache
// ---------------------------------------------------------------------------

struct EndDateCache {
    inner: HashMap<String, chrono::DateTime<chrono::Utc>>,
}

impl EndDateCache {
    fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    async fn get_or_fetch(
        &mut self,
        token_id: &str,
        query: &EndDateQuery,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        if let Some(ed) = self.inner.get(token_id) {
            return Some(*ed);
        }
        if let Some(ed) = query(token_id.to_string()).await {
            self.inner.insert(token_id.to_string(), ed);
            return Some(ed);
        }
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub fn start_strategy_engine(
    mut rx: mpsc::Receiver<TradeEvent>,
    state: Arc<RwLock<BotState>>,
    mut risk_engine: RiskEngine,
    submitter: OrderSubmitter,
    config: Config,
    copy_ledger: Arc<Mutex<CopyLedger>>,
    holds_query: HoldsQuery,
    end_date_query: EndDateQuery,
) {
    tokio::spawn(async move {
        info!("Strategy Engine Started. Monitoring edge cases (debouncing, closures...)");

        let mut debounce = DebounceCache::new();
        let mut live_cache = LiveQueryCache::new();
        let mut end_date_cache = EndDateCache::new();
        // Periodic cache maintenance counter
        let mut event_count: u32 = 0;

        while let Some(event) = rx.recv().await {
            event_count += 1;
            // Evict expired live-query cache entries every 50 events (Gap 13)
            if event_count.is_multiple_of(50) {
                live_cache.evict_expired();
            }

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

            // 2. Fragmented fill debounce (Gap 5 — bounded cache with TTL eviction)
            let cache_key = format!(
                "{}_{}_{:?}",
                event.taker_address, event.token_id, event.side
            );
            if eval.validated && debounce.insert_or_accumulate(cache_key, event.clone()) {
                continue; // accumulated — skip this event
            }

            // 3. Risk bounds
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
            //   LIVE VERIFICATION (Gap 13 — cached): for BUY events we query the
            //   target's wallet live; for SELL events we query OUR wallet live.
            //   Results are cached for LIVE_QUERY_CACHE_TTL_SECS seconds to reduce
            //   API calls and latency for burst activity.

            let mut resolved_end_date = None;

            if eval.validated {
                // Check market closing soon (before resolving holdings to save time if skipped)
                if let Some(skip_mins) = config.ignore_closing_in_mins {
                    let ed = end_date_cache
                        .get_or_fetch(&event.token_id, &end_date_query)
                        .await;
                    resolved_end_date = ed;
                    if let Some(end_date) = ed {
                        let cutoff =
                            chrono::Utc::now() + chrono::Duration::minutes(skip_mins as i64);
                        if end_date <= cutoff {
                            eval.validated = false;
                            eval.reason = Some(format!(
                                "Market closes in < {} mins (at {})",
                                skip_mins,
                                end_date.format("%H:%M UTC")
                            ));
                            warn!("Trade skipped: {}", eval.reason.as_ref().unwrap());
                        }
                    }
                }
            }

            if eval.validated {
                // --- Resolve live state for this token ---

                // Our position: check cache first, then live API
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

                // Check live-query cache before making API calls (Gap 13)
                let our_cached = live_cache.get(&config.funder_address, &event.token_id);
                let target_cached = live_cache.get(&event.taker_address, &event.token_id);

                let (live_we_hold, live_target_holds) = if our_cached.is_some()
                    && target_cached.is_some()
                {
                    // Both are cached — no API call needed
                    (
                        our_cached.unwrap_or(cache_we_hold),
                        target_cached.unwrap_or(cache_target_holds),
                    )
                } else {
                    // Run whichever queries are needed in parallel
                    let (live_we_hold_opt, live_target_holds_opt) = tokio::join!(
                        async {
                            if let Some(cached) = our_cached {
                                Some(cached)
                            } else {
                                holds_query(config.funder_address.clone(), event.token_id.clone())
                                    .await
                            }
                        },
                        async {
                            if let Some(cached) = target_cached {
                                Some(cached)
                            } else {
                                holds_query(event.taker_address.clone(), event.token_id.clone())
                                    .await
                            }
                        },
                    );

                    let we_hold = live_we_hold_opt.unwrap_or(cache_we_hold);
                    let target_holds = live_target_holds_opt.unwrap_or(cache_target_holds);

                    // Populate cache with fresh results
                    live_cache.set(&config.funder_address, &event.token_id, we_hold);
                    live_cache.set(&event.taker_address, &event.token_id, target_holds);

                    (we_hold, target_holds)
                };

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
                            match &ledger_entry {
                                Some(entry) if entry.source_wallet == event.taker_address => {
                                    None // Correct source is selling → close
                                }
                                Some(entry) => Some(format!(
                                    "SELL skipped: {} sold but we copied from {} — \
                                     keeping position",
                                    &event.taker_address[..event.taker_address.len().min(10)],
                                    &entry.source_wallet[..entry.source_wallet.len().min(10)],
                                )),
                                None => {
                                    warn!(
                                        "SELL: we hold token {} with no ledger entry — \
                                         closing defensively.",
                                        &event.token_id[..event.token_id.len().min(12)]
                                    );
                                    None
                                }
                            }
                        } else if let Some(entry) = &ledger_entry {
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

                // Determine limit price: rounded to 2dp (CLOB tick), capped to [0.01, 0.99]
                let limit_price =
                    calculate_limit_price(event.price, event.side, config.max_slippage_pct);

                let order: Option<(OrderRequest, Decimal)> = if is_closing {
                    // -- SELL: close our position using our 100% held size --
                    // (Polymarket handles the CTF fee by deducting from the USDC payout,
                    // we no longer reduce the share count to avoid "dust" shares).
                    let our_held_size = {
                        let guard = state.read().await;
                        guard
                            .positions
                            .get(&event.token_id)
                            .map(|p| p.size)
                            .unwrap_or(Decimal::ZERO)
                    };

                    Some((
                        OrderRequest {
                            token_id: event.token_id.clone(),
                            price: limit_price,
                            size: our_held_size,
                            side: event.side,
                        },
                        Decimal::ZERO,
                    ))
                } else {
                    // -- BUY: size according to active SizingMode, capped and $5 floored --
                    let current_balance = {
                        let guard = state.read().await;
                        guard.total_balance
                    };
                    // target_notional = what the target just bet in dollar terms
                    let target_notional = event.size * event.price;
                    let wallet_scalar = config
                        .target_scalars
                        .get(&event.maker_address)
                        .cloned()
                        .unwrap_or(Decimal::ONE);
                    let budget_usd = compute_order_usd(
                        current_balance,
                        &config.sizing_mode,
                        config.copy_size_pct,
                        wallet_scalar,
                        config.max_trade_size_usd,
                        target_notional,
                    );
                    // Budget exactly reflects the sizing engine's mathematical intent.
                    // (TargetUSD mirrors natively, Scalars scale natively, Fixed/SelfPct override natively)
                    let raw_size = budget_usd / event.price;
                    let buy_size = raw_size.round_dp(2); // CLOB requires 2dp lot size

                    // CLOB hard minimum: 5 shares. Orders below this always 400.
                    if buy_size < MIN_ORDER_SHARES {
                        warn!(
                            "BUY skipped: computed {:.2} shares is below CLOB minimum of {} shares \
                             (budget=${:.2} at price ${:.3}). Increase COPY_SIZE_PCT or wait for higher balance.",
                            buy_size, MIN_ORDER_SHARES, budget_usd, limit_price
                        );
                        None
                    } else {
                        // Pre-check balance taking the maximum CTF fee overhead into account
                        // fee = C * feeRate * p * (1 - p). We assume a max 200bps (0.02) feeRate to be safe.
                        let p = limit_price;
                        let max_ctf_fee =
                            buy_size * Decimal::from_str("0.02").unwrap() * p * (Decimal::ONE - p);
                        let order_cost = buy_size * limit_price;
                        let total_cost = order_cost + max_ctf_fee;

                        if current_balance < total_cost {
                            warn!(
                                "Insufficient balance (have ${:.2}, need ${:.2} including fee) -- skipping entry",
                                current_balance, total_cost
                            );
                            None
                        } else {
                            // Check whether we already have a pending GTC order for this token.
                            let already_pending = {
                                let guard = state.read().await;
                                guard.pending_orders.contains_key(&event.token_id)
                            };
                            if already_pending {
                                warn!(
                                    "BUY skipped: live GTC order already exists for token {}",
                                    event.token_id
                                );
                                None
                            } else {
                                Some((
                                    OrderRequest {
                                        token_id: event.token_id.clone(),
                                        price: limit_price,
                                        size: buy_size,
                                        side: event.side,
                                    },
                                    total_cost,
                                ))
                            }
                        }
                    }
                };

                if let Some((order, cost_to_deduct)) = order {
                    // Register token as pending BEFORE spawning so any concurrent
                    // events for the same token are blocked immediately.
                    {
                        let mut guard = state.write().await;
                        guard.pending_orders.insert(
                            order.token_id.clone(),
                            crate::models::QueuedOrder {
                                token_id: order.token_id.clone(),
                                price: order.price,
                                size: order.size,
                                side: order.side,
                                event_end_date: resolved_end_date,
                            },
                        );

                        // Eagerly secure the margin requirement from our balance!
                        // This prevents rapid-fire sequential trades from overlapping
                        // against the same stale balance and throwing CLOB 400 bounds!
                        if cost_to_deduct > Decimal::ZERO {
                            guard.total_balance -= cost_to_deduct;
                        }
                    }

                    let submitter_clone = submitter.clone();
                    let state_clone = state.clone();
                    let token_id_clone = order.token_id.clone();
                    let source_wallet_clone = event.taker_address.clone();
                    let is_closing = event.side == TradeSide::SELL;
                    let order_size = order.size;
                    let order_price = order.price;
                    let ledger_clone = copy_ledger.clone();

                    // Capture entry_price from ledger BEFORE spawning for realized PnL.
                    let entry_price = if is_closing {
                        let ledger = copy_ledger.lock().await;
                        ledger
                            .find_active_for_token(&event.token_id)
                            .map(|e| e.entry_price)
                    } else {
                        None
                    };

                    let is_sim = config.is_sim;

                    tokio::spawn(async move {
                        match submitter_clone(order).await {
                            Ok(()) => {
                                let mut ledger = ledger_clone.lock().await;
                                if is_closing {
                                    ledger.record_close(&token_id_clone, &source_wallet_clone);
                                    info!(
                                        "Ledger: closed {} from {}",
                                        &token_id_clone[..token_id_clone.len().min(12)],
                                        &source_wallet_clone[..source_wallet_clone.len().min(10)],
                                    );
                                    // Accumulate realized PnL
                                    if let Some(avg_entry) = entry_price {
                                        let pnl = (order_price - avg_entry) * order_size;
                                        let mut guard = state_clone.write().await;
                                        guard.realized_pnl += pnl;
                                        // Subtract this token's contribution from unrealized;
                                        // price refresh corrects the full sum within 20s.
                                        let old_unrealized = (order_price - avg_entry) * order_size;
                                        guard.unrealized_pnl -= old_unrealized;
                                    }
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

                                if is_sim {
                                    let mut guard = state_clone.write().await;
                                    let fee_rate = rust_decimal::Decimal::from_str("0.02").unwrap();
                                    let max_ctf_fee = order_size
                                        * fee_rate
                                        * order_price
                                        * (rust_decimal::Decimal::ONE - order_price);

                                    if is_closing {
                                        guard.positions.remove(&token_id_clone);
                                        guard.total_balance +=
                                            (order_size * order_price) - max_ctf_fee;
                                    } else {
                                        // Auto-fill mock position
                                        guard.positions.insert(
                                            token_id_clone.clone(),
                                            crate::models::Position {
                                                token_id: token_id_clone.clone(),
                                                size: order_size,
                                                average_entry_price: order_price,
                                            },
                                        );
                                        // We purposefully do NOT deduct the cost here!
                                        // It was already eagerly deducted right before `tokio::spawn`!
                                    }
                                    // Remove from pending logic to match real world
                                    guard.pending_orders.remove(&token_id_clone);
                                }
                            }
                            Err(e) => {
                                // Remove from pending on failure so the order can be retried.
                                let mut guard = state_clone.write().await;
                                guard.pending_orders.remove(&token_id_clone);

                                // RESTORE the unused eagerly-deducted margin cap!
                                if cost_to_deduct > Decimal::ZERO {
                                    guard.total_balance += cost_to_deduct;
                                }

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
