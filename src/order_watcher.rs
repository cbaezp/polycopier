use crate::backoff::next_backoff;
use crate::clients::AuthedClobClient;
use crate::config::Config;
use crate::risk::RiskEngine;
use crate::state::BotState;
use alloy::primitives::Address;
use polymarket_client_sdk::clob::types::request::OrdersRequest;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub fn start_order_watcher(
    config: Config,
    clob: AuthedClobClient,
    state: Arc<RwLock<BotState>>,
    risk_engine: Arc<Mutex<RiskEngine>>,
) {
    tokio::spawn(async move {
        // give the clob client some time to breathe before polling
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        let data_client = DataClient::default();
        let mut consecutive_errors: u32 = 0;

        loop {
            match run_once(&config, &clob, &data_client, &state, &risk_engine).await {
                Ok(_) => {
                    consecutive_errors = 0;
                    // Stamp AFTER a successful run so TUI shows accurate "Xs ago"
                    let mut guard = state.write().await;
                    guard.last_watcher_run_at = Some(std::time::Instant::now());
                }
                Err(e) => {
                    consecutive_errors += 1;
                    tracing::warn!(
                        "Order watcher error (consecutive={}): {}",
                        consecutive_errors,
                        e
                    );
                }
            }

            // Gap E: exponential backoff on sustained errors (base 10s, max 120s).
            let sleep = next_backoff(consecutive_errors, 10, 120);
            tokio::time::sleep(tokio::time::Duration::from_secs(sleep)).await;
        }
    });
}

async fn run_once(
    config: &Config,
    clob: &AuthedClobClient,
    data_client: &DataClient,
    state: &Arc<RwLock<BotState>>,
    risk_engine: &Arc<Mutex<RiskEngine>>,
) -> anyhow::Result<()> {
    let target_wallets = &config.target_wallets;
    let max_loss = config.max_copy_loss_pct;
    // 1. Fetch our open live orders
    let req = OrdersRequest::default();
    let orders_page = clob.orders(&req, None).await?;

    // Prepare a set of tokens we actually care about, filtering for Live orders
    let mut open_tokens = std::collections::HashSet::new();
    let mut live_orders = Vec::new();
    for o in orders_page.data {
        let status_str = format!("{:?}", o.status).to_uppercase();
        if status_str.contains("LIVE") {
            open_tokens.insert(o.asset_id.to_string());
            live_orders.push(o);
        }
    }

    // --- GHOST PURGE ---
    // If an order is in our local `pending_orders` queue but DOES NOT appear
    // in Polymarket's `live_orders` (e.g. silently rejected, expired, or filled
    // without WS ping), we must purge it.
    {
        let mut guard = state.write().await;
        let mut ghosts = Vec::new();
        for tid in guard.pending_orders.keys() {
            if !open_tokens.contains(tid) {
                ghosts.push(tid.clone());
            }
        }
        for ghost in ghosts {
            tracing::info!(
                "Purging ghost order for token {}: no longer LIVE on Polymarket network.",
                ghost
            );
            guard.pending_orders.remove(&ghost);
        }
    }

    if live_orders.is_empty() {
        return Ok(());
    }

    // 2. Fetch targets' current positions
    // Map of token_id -> Best PnL across targets for this token, plus expiry flags
    struct TargetState {
        pnl: Decimal,
        redeemable: bool,
        expired: bool,
    }
    let mut target_states: HashMap<String, TargetState> = HashMap::new();

    for wallet_str in target_wallets {
        let wallet_str = wallet_str.trim();
        if wallet_str.is_empty() {
            continue;
        }
        if let Ok(addr) = Address::from_str(wallet_str) {
            let req = PositionsRequest::builder().user(addr).build();
            if let Ok(ps) = data_client.positions(&req).await {
                for p in ps {
                    let tid = p.asset.to_string();
                    if !open_tokens.contains(&tid) {
                        continue;
                    }

                    let pnl = p.percent_pnl / rust_decimal::Decimal::from(100);
                    let today = chrono::Utc::now().date_naive();
                    let redeemable = p.redeemable;
                    // Use < today (strictly past), NOT <= today.
                    // A same-day market with redeemable=false is still open and our
                    // GTC order should stay active until Polymarket flips redeemable=true.
                    let expired = p.end_date.is_some_and(|d| d < today);

                    let entry = target_states.entry(tid).or_insert(TargetState {
                        pnl: Decimal::MIN,
                        redeemable: true,
                        expired: true,
                    });

                    if pnl > entry.pnl {
                        entry.pnl = pnl;
                    }
                    // A token is not expired if at least one target's version says it's not
                    if !redeemable {
                        entry.redeemable = false;
                    }
                    if !expired {
                        entry.expired = false;
                    }
                }
            }
        }
    }

    // 3. Evaluate each order
    for order in live_orders {
        let tid_val = order.asset_id.to_string();
        let tid = &tid_val;

        let mut should_cancel = false;
        let mut reason = "";
        let mut is_loss_cancel = false;

        if let Some(tstate) = target_states.get(tid) {
            if tstate.redeemable || tstate.expired {
                should_cancel = true;
                reason = "Market resolved or expired";
            } else if tstate.pnl <= -max_loss {
                // Target is in a huge loss
                should_cancel = true;
                is_loss_cancel = true;
                reason = "Target position PnL dropped past MAX_COPY_LOSS_PCT limit";
            }
        } else {
            // Target(s) no longer hold this position! They sold.
            should_cancel = true;
            reason = "Target closed position (zero balance)";
        }

        // Feature: Ignore markets closing in less than X minutes
        if !should_cancel {
            if let Some(skip_mins) = config.ignore_closing_in_mins {
                let guard = state.read().await;
                if let Some(pending) = guard.pending_orders.get(tid) {
                    if let Some(ed) = pending.event_end_date {
                        let cutoff =
                            chrono::Utc::now() + chrono::Duration::minutes(skip_mins as i64);
                        if ed <= cutoff {
                            should_cancel = true;
                            reason = "Market is closing soon (threshold reached)";
                        }
                    }
                }
            }
        }

        if should_cancel {
            tracing::warn!("Order Watcher cancelling order {}: {}", order.id, reason);
            if let Err(e) = clob.cancel_order(&order.id).await {
                tracing::error!("Failed to cancel order {}: {}", order.id, e);
            } else {
                // Unblock the scanner so the market can be re-entered if needed
                let mut guard = state.write().await;
                guard.pending_orders.remove(tid);
                drop(guard);

                // Gap C: notify risk engine when a loss-triggered cancel fires.
                // This increments the consecutive-loss counter and may trigger cooldown.
                if is_loss_cancel {
                    let mut risk = risk_engine.lock().await;
                    risk.record_loss();
                }
            }
        }
    }

    Ok(())
}
