//! Background tasks that keep shared state in sync with the Polymarket Data API.
//!
//! All tasks here follow the same shape: spawn a loop, fetch from the API,
//! write to `BotState`. Extracting them here keeps `main.rs` as a thin wiring
//! file and makes each task independently testable.
//!
//! | Task                       | Interval | What it updates                               |
//! |----------------------------|----------|-----------------------------------------------|
//! | `seed_own_positions`       | once     | `state.positions` (boot snapshot)             |
//! | `seed_pending_orders`      | once     | `state.pending_order_tokens` (boot snapshot)  |
//! | `start_position_sync`      | 30 s     | `state.positions` (fill tracking, Gap 4)      |
//! | `start_price_refresh`      | 20 s     | `state.target_positions[*].cur_price`+PnL     |
//! | `start_balance_poll`       | 10 s     | `state.total_balance`                         |
//! | `start_position_close_sweep` | 60 s   | synthetic SELL events (Gap 2 fix)             |

use crate::backoff::next_backoff;
use crate::clients::{AuthedClobClient, BalanceFetcher};
use crate::copy_ledger::CopyLedger;
use crate::models::{Position, TradeEvent, TradeSide};
use crate::state::BotState;
use alloy::primitives::Address;
use polymarket_client_sdk::clob::types::request::OrdersRequest;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};

type State = Arc<RwLock<BotState>>;

// ---------------------------------------------------------------------------
// 1. Boot seed — AWAITABLE. Populates state.positions from the live wallet API
//    before the position scanner is started. main.rs must `.await` this so the
//    scanner's SkippedOwned check has accurate data from the first scan.
//    Without this guarantee the scanner re-enters positions on every restart.
// ---------------------------------------------------------------------------
pub async fn seed_own_positions(funder: &str, state: State) {
    let client = DataClient::default();
    let Ok(addr) = Address::from_str(funder) else {
        return;
    };
    let req = PositionsRequest::builder().user(addr).build();
    match client.positions(&req).await {
        Ok(positions) => {
            let mut guard = state.write().await;
            for p in &positions {
                let token_id = p.asset.to_string();
                guard.positions.insert(
                    token_id.clone(),
                    Position {
                        token_id,
                        size: p.size,
                        average_entry_price: p.avg_price,
                    },
                );
            }
            tracing::warn!(
                "Seeded {} existing position(s) from wallet — scanner will skip these.",
                guard.positions.len()
            );
        }
        Err(e) => tracing::warn!("Could not seed own positions on startup: {}", e),
    }
}

// ---------------------------------------------------------------------------
// 1b. Pending CLOB order seed — AWAITABLE. Fetches live GTC orders from the
//     CLOB and records their token IDs in state.pending_order_tokens so the
//     scanner's first run treats them as already-queued. Prevents the
//     "order already exists" 400 errors on bot restart.
// ---------------------------------------------------------------------------
pub async fn seed_pending_orders(clob: &AuthedClobClient, state: State) {
    let req = OrdersRequest::default();
    match clob.orders(&req, None).await {
        Ok(page) => {
            let live = page
                .data
                .into_iter()
                .filter(|o| format!("{:?}", o.status).to_uppercase().contains("LIVE"))
                .collect::<Vec<_>>();
            let count = live.len();
            if count > 0 {
                use crate::models::{QueuedOrder, TradeSide};
                let mut guard = state.write().await;
                for o in live {
                    let size = o.original_size;
                    let price = o.price;
                    let side = if format!("{:?}", o.side).to_uppercase().contains("BUY") {
                        TradeSide::BUY
                    } else {
                        TradeSide::SELL
                    };
                    guard.pending_orders.insert(
                        o.asset_id.to_string(),
                        QueuedOrder {
                            token_id: o.asset_id.to_string(),
                            price,
                            size,
                            side,
                        },
                    );
                }
                tracing::warn!(
                    "Seeded {} live GTC order(s) — scanner will treat these as already queued.",
                    count
                );
            }
        }
        Err(e) => tracing::warn!("Could not seed pending CLOB orders on startup: {}", e),
    }
}

// ---------------------------------------------------------------------------
// 2. Position sync — runs every 30 s, upserts filled positions into
//    state.positions so GTC fills that happen after boot become visible in the
//    TUI table without requiring a bot restart.
//    Gap 4: also calls ledger.update_fill() when a position size differs from
//    the recorded order size (partial fills).
// ---------------------------------------------------------------------------
pub fn start_position_sync(config: crate::config::Config, state: State, copy_ledger: Arc<Mutex<CopyLedger>>) {
    if config.is_sim {
        tracing::warn!("Simulation Mode: skipping start_position_sync (assuming instant exact fills).");
        return;
    }
    
    let funder = config.funder_address.clone();
    tokio::spawn(async move {
        let client = DataClient::default();
        // Boot seed has already completed (main.rs awaits seed_own_positions first).
        // Still wait 30s before the first diff-refresh to avoid hammering the API.
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        let mut consecutive_errors: u32 = 0;
        loop {
            if let Ok(addr) = Address::from_str(&funder) {
                let req = PositionsRequest::builder().user(addr).build();
                match client.positions(&req).await {
                    Ok(positions) => {
                        consecutive_errors = 0;
                        let mut guard = state.write().await;
                        for p in &positions {
                            if p.redeemable || p.cur_price == Decimal::ZERO {
                                // Market resolved — remove stale entry.
                                guard.positions.remove(&p.asset.to_string());
                            } else {
                                guard.positions.insert(
                                    p.asset.to_string(),
                                    Position {
                                        token_id: p.asset.to_string(),
                                        size: p.size,
                                        average_entry_price: p.avg_price,
                                    },
                                );
                            }
                        }
                        drop(guard);

                        // Gap 4: update ledger fill sizes for partial fills.
                        let mut ledger = copy_ledger.lock().await;
                        for p in &positions {
                            if p.redeemable || p.cur_price == Decimal::ZERO {
                                continue;
                            }
                            let token_id = p.asset.to_string();
                            // update_fill is a no-op if sizes already match or no entry exists.
                            ledger.update_fill(&token_id, p.size);
                        }
                    }
                    Err(e) => {
                        consecutive_errors += 1;
                        tracing::warn!(
                            "Position sync failed (consecutive={}): {}",
                            consecutive_errors,
                            e
                        );
                    }
                }
            }
            // Gap 6: exponential backoff on sustained errors; base 30s, max 300s.
            let sleep = next_backoff(consecutive_errors, 30, 300);
            tokio::time::sleep(tokio::time::Duration::from_secs(sleep)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 3. Price refresh — runs every 20 s, updates cur_price on each
//    target_position entry so the TUI scanner table shows live prices
//    independent of the scanner's adaptive interval.
//
//    Gap 7: unrealized PnL is now computed from OUR positions (state.positions)
//    keyed against the fresh price_map, with a fallback to avg_entry if a
//    price is not in the map.  This prevents tokens that closed between
//    refreshes from contributing 0 to the total.
// ---------------------------------------------------------------------------
pub fn start_price_refresh(target_wallets: Vec<String>, state: State) {
    tokio::spawn(async move {
        let client = DataClient::default();
        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
        let mut consecutive_errors: u32 = 0;
        loop {
            let mut price_map: HashMap<String, Decimal> = HashMap::new();
            let mut had_error = false;
            for wallet_str in &target_wallets {
                let wallet_str = wallet_str.trim();
                if let Ok(addr) = Address::from_str(wallet_str) {
                    let req = PositionsRequest::builder().user(addr).build();
                    match client.positions(&req).await {
                        Ok(ps) => {
                            for p in ps {
                                price_map.insert(p.asset.to_string(), p.cur_price);
                            }
                        }
                        Err(e) => {
                            had_error = true;
                            tracing::warn!(
                                "Price refresh failed for {}: {e} (consecutive={})",
                                &wallet_str[..wallet_str.len().min(10)],
                                consecutive_errors + 1
                            );
                        }
                    }
                }
            }

            if had_error {
                consecutive_errors += 1;
            } else {
                consecutive_errors = 0;
            }

            if !price_map.is_empty() || !had_error {
                let mut g = state.write().await;

                // Update cur_price on target_positions
                for tp in g.target_positions.iter_mut() {
                    if let Some(&fresh_price) = price_map.get(&tp.token_id) {
                        tp.cur_price = fresh_price;
                    }
                }
                g.last_price_refresh_at = Some(std::time::Instant::now());

                // Gap 7: recompute unrealized PnL from OUR positions, falling back
                // to avg_entry (PnL = 0) when a token is absent from price_map.
                // This is correct regardless of whether a target has already closed.
                let unrealized: Decimal = g
                    .positions
                    .values()
                    .map(|p| {
                        let cur = price_map
                            .get(&p.token_id)
                            .copied()
                            .unwrap_or(p.average_entry_price);
                        (cur - p.average_entry_price) * p.size
                    })
                    .sum();
                g.unrealized_pnl = unrealized;
            }

            // Gap 6: backoff on sustained price-refresh errors; base 20s, max 120s.
            let sleep = next_backoff(consecutive_errors, 20, 120);
            tokio::time::sleep(tokio::time::Duration::from_secs(sleep)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 4. Balance poll — runs every 10 s, keeps state.total_balance current so the
//    TUI header and sizing logic always have an up-to-date USDC balance.
// ---------------------------------------------------------------------------
pub fn start_balance_poll(balance_fetcher: BalanceFetcher, state: State) {
    tokio::spawn(async move {
        let mut consecutive_errors: u32 = 0;
        loop {
            match balance_fetcher().await {
                Ok(balance) => {
                    consecutive_errors = 0;
                    let mut guard = state.write().await;
                    guard.total_balance = balance;
                }
                Err(e) => {
                    consecutive_errors += 1;
                    tracing::warn!(
                        "Balance fetch failed (consecutive={}): {}",
                        consecutive_errors,
                        e
                    );
                }
            }
            // Gap 6: backoff on balance-poll errors; base 10s, max 60s.
            let sleep = next_backoff(consecutive_errors, 10, 60);
            tokio::time::sleep(tokio::time::Duration::from_secs(sleep)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 5. Position-close sweep — runs every 60 s, compares our held positions
//    against target wallet positions via the Data API.  For every token WE
//    hold that NO target still holds, a synthetic SELL event is emitted so
//    the strategy engine closes our position.
//
//    Gap 2 fix: the synthetic event's `taker_address` is now set to the
//    `source_wallet` recorded in the copy ledger for that token.  This ensures
//    `record_close(token_id, source_wallet)` in strategy.rs correctly finds and
//    closes the ledger entry.  Falls back to `first_target` if no ledger entry
//    exists (defensive-close path — the engine handles this gracefully).
// ---------------------------------------------------------------------------
pub fn start_position_close_sweep(
    target_wallets: Vec<String>,
    state: Arc<RwLock<BotState>>,
    tx: mpsc::Sender<TradeEvent>,
    copy_ledger: Arc<Mutex<CopyLedger>>,
) {
    let Some(first_target) = target_wallets.first().cloned() else {
        tracing::warn!("Position close sweep: no target wallets configured — sweep disabled.");
        return;
    };

    tokio::spawn(async move {
        let client = DataClient::default();
        // Wait 60 s on first run so boot-time seeding and position sync
        // have a chance to populate state before we start comparing.
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

        loop {
            // Snapshot the positions we currently hold.
            let our_positions: Vec<Position> = {
                let guard = state.read().await;
                guard.positions.values().cloned().collect()
            };

            if !our_positions.is_empty() {
                // Fetch all token IDs held by any target wallet (union).
                let mut target_tokens: HashSet<String> = HashSet::new();
                for wallet_str in &target_wallets {
                    let w = wallet_str.trim();
                    if w.is_empty() {
                        continue;
                    }
                    if let Ok(addr) = Address::from_str(w) {
                        let req = PositionsRequest::builder().user(addr).build();
                        match client.positions(&req).await {
                            Ok(ps) => {
                                for p in ps {
                                    target_tokens.insert(p.asset.to_string());
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Position sweep: failed to fetch {}: {}", w, e);
                            }
                        }
                    }
                }

                // For each position we hold, if no target still holds it → close.
                for pos in &our_positions {
                    if target_tokens.contains(&pos.token_id) {
                        continue; // Target still holds it — nothing to do.
                    }

                    let short_id = &pos.token_id[..pos.token_id.len().min(12)];
                    tracing::warn!(
                        "Position sweep: no target holds token {} (our size={:.2}) \
                         — emitting synthetic SELL to close our position.",
                        short_id,
                        pos.size,
                    );

                    // Gap 2 fix: look up the ledger's source_wallet for this token
                    // so the synthetic SELL's taker_address correctly matches the
                    // ledger entry that record_close() will look for.
                    let source_wallet = {
                        let ledger = copy_ledger.lock().await;
                        ledger
                            .find_active_for_token(&pos.token_id)
                            .map(|e| e.source_wallet.clone())
                            .unwrap_or_else(|| first_target.clone())
                    };

                    let event = TradeEvent {
                        transaction_hash: format!(
                            "sweep_{}",
                            &pos.token_id[..pos.token_id.len().min(12)]
                        ),
                        maker_address: source_wallet.clone(),
                        taker_address: source_wallet.clone(),
                        token_id: pos.token_id.clone(),
                        price: pos.average_entry_price,
                        size: pos.size,
                        side: TradeSide::SELL,
                        timestamp: chrono::Utc::now().timestamp(),
                    };

                    if let Err(e) = tx.send(event).await {
                        tracing::warn!("Position sweep: channel closed — stopping: {}", e);
                        return;
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    });
}
