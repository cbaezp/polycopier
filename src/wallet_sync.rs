//! Background tasks that keep shared state in sync with the Polymarket Data API.
//!
//! All four tasks here follow the same shape: spawn a loop, fetch from the API,
//! write to `BotState`. Extracting them here keeps `main.rs` as a thin wiring
//! file and makes each task independently testable.
//!
//! | Task                  | Interval | What it updates                         |
//! |-----------------------|----------|-----------------------------------------|
//! | `seed_own_positions`  | once     | `state.positions` (boot snapshot)       |
//! | `start_position_sync` | 30 s     | `state.positions` (fill tracking)       |
//! | `start_price_refresh` | 20 s     | `state.target_positions[*].cur_price`   |
//! | `start_balance_poll`  | 10 s     | `state.total_balance`                   |

use crate::clients::{AuthedClobClient, BalanceFetcher};
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
use tokio::sync::{mpsc, RwLock};

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
            let live: Vec<String> = page
                .data
                .into_iter()
                .filter(|o| format!("{:?}", o.status).to_uppercase().contains("LIVE"))
                .map(|o| o.asset_id.to_string())
                .collect();
            let count = live.len();
            if count > 0 {
                let mut guard = state.write().await;
                for token_id in live {
                    guard.pending_order_tokens.insert(token_id);
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
// ---------------------------------------------------------------------------
pub fn start_position_sync(funder: String, state: State) {
    tokio::spawn(async move {
        let client = DataClient::default();
        // Boot seed has already completed (main.rs awaits seed_own_positions first).
        // Still wait 30s before the first diff-refresh to avoid hammering the API.
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        loop {
            if let Ok(addr) = Address::from_str(&funder) {
                let req = PositionsRequest::builder().user(addr).build();
                match client.positions(&req).await {
                    Ok(positions) => {
                        let mut guard = state.write().await;
                        for p in positions {
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
                    }
                    Err(e) => tracing::warn!("Position sync failed: {}", e),
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 3. Price refresh — runs every 20 s, updates cur_price on each
//    target_position entry so the TUI scanner table shows live prices
//    independent of the scanner's adaptive interval.
// ---------------------------------------------------------------------------
pub fn start_price_refresh(target_wallets: Vec<String>, state: State) {
    tokio::spawn(async move {
        let client = DataClient::default();
        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
        loop {
            let mut price_map: HashMap<String, Decimal> = HashMap::new();
            for wallet_str in &target_wallets {
                let wallet_str = wallet_str.trim();
                if let Ok(addr) = Address::from_str(wallet_str) {
                    let req = PositionsRequest::builder().user(addr).build();
                    if let Ok(ps) = client.positions(&req).await {
                        for p in ps {
                            price_map.insert(p.asset.to_string(), p.cur_price);
                        }
                    }
                }
            }
            if !price_map.is_empty() {
                let mut g = state.write().await;
                for tp in g.target_positions.iter_mut() {
                    if let Some(&fresh_price) = price_map.get(&tp.token_id) {
                        tp.cur_price = fresh_price;
                    }
                }
                g.last_price_refresh_at = Some(std::time::Instant::now());
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(20)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 4. Balance poll — runs every 10 s, keeps state.total_balance current so the
//    TUI header and sizing logic always have an up-to-date USDC balance.
// ---------------------------------------------------------------------------
pub fn start_balance_poll(balance_fetcher: BalanceFetcher, state: State) {
    tokio::spawn(async move {
        loop {
            match balance_fetcher().await {
                Ok(balance) => {
                    let mut guard = state.write().await;
                    guard.total_balance = balance;
                }
                Err(e) => tracing::warn!("Balance fetch failed: {}", e),
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 5. Position-close sweep — runs every 60 s, compares our held positions
//    against target wallet positions via the Data API.  For every token WE
//    hold that NO target still holds, a synthetic SELL event is emitted so
//    the strategy engine closes our position.
//
//    This is a backstop for the two failure modes that can cause us to miss
//    a WS-delivered SELL event:
//      - Bug A (now fixed): checksum/lowercase address mismatch drops the event
//      - Bug B (now fixed): scanner race clears target_holds before event arrives
//    Even if both fixes are in place, the sweep provides defense-in-depth.
//
//    The sweep event uses the first configured target wallet as `taker_address`
//    so it clears the wallet check in strategy.rs (which is a Vec::contains on
//    config.target_wallets).  Strategy uses our HELD size (from state.positions),
//    not the event size, for the actual SELL order, so the synthetic size field
//    is only used for the risk-engine $1 minimum check.
// ---------------------------------------------------------------------------
pub fn start_position_close_sweep(
    target_wallets: Vec<String>,
    state: Arc<RwLock<BotState>>,
    tx: mpsc::Sender<TradeEvent>,
) {
    // We need at least one target wallet address to pass the wallet check in
    // strategy.rs.  If there are none the sweep cannot function safely.
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

                    // Build a synthetic SELL event.
                    // taker_address = first target wallet so the wallet check in
                    // strategy.rs (Vec::contains on config.target_wallets) passes.
                    // strategy.rs reads our ACTUAL held size from state.positions,
                    // so the event.size here is only used for the risk-engine
                    // spoofing check (must be > $1 notional).
                    let event = TradeEvent {
                        transaction_hash: format!(
                            "sweep_{}",
                            &pos.token_id[..pos.token_id.len().min(12)]
                        ),
                        maker_address: first_target.clone(),
                        taker_address: first_target.clone(),
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
