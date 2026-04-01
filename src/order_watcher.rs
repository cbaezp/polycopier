use crate::clients::AuthedClobClient;
use crate::config::Config;
use crate::state::BotState;
use alloy::primitives::Address;
use polymarket_client_sdk::clob::types::request::OrdersRequest;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

pub fn start_order_watcher(config: Config, clob: AuthedClobClient, state: Arc<RwLock<BotState>>) {
    let target_wallets = config.target_wallets.clone();
    let max_loss = config.max_copy_loss_pct;

    tokio::spawn(async move {
        // give the clob client some time to breathe before polling
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        let data_client = DataClient::default();

        loop {
            match run_once(&clob, &data_client, &target_wallets, max_loss, &state).await {
                Ok(_) => {
                    // Stamp AFTER a successful run so TUI shows accurate "Xs ago"
                    let mut guard = state.write().await;
                    guard.last_watcher_run_at = Some(std::time::Instant::now());
                }
                Err(e) => {
                    tracing::warn!("Order watcher encountered an error: {}", e);
                }
            }

            // Sleep 10 seconds before next check
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        }
    });
}

async fn run_once(
    clob: &AuthedClobClient,
    data_client: &DataClient,
    target_wallets: &[String],
    max_loss: Decimal,
    state: &Arc<RwLock<BotState>>,
) -> anyhow::Result<()> {
    // 1. Fetch our open live orders
    let req = OrdersRequest::default();
    let orders_page = clob.orders(&req, None).await?;
    if orders_page.data.is_empty() {
        return Ok(()); // Nothing to monitor
    }

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

                    let pnl = p.percent_pnl;
                    let today = chrono::Utc::now().date_naive();
                    let redeemable = p.redeemable;
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

        if let Some(tstate) = target_states.get(tid) {
            if tstate.redeemable || tstate.expired {
                should_cancel = true;
                reason = "Market resolved or expired";
            } else if tstate.pnl <= -max_loss {
                // Target is in a huge loss?
                should_cancel = true;
                reason = "Target position PnL dropped past MAX_COPY_LOSS_PCT limit";
            }
        } else {
            // Target(s) no longer hold this position! They sold.
            should_cancel = true;
            reason = "Target closed position (zero balance)";
        }

        if should_cancel {
            tracing::warn!("Order Watcher cancelling order {}: {}", order.id, reason);
            if let Err(e) = clob.cancel_order(&order.id).await {
                tracing::error!("Failed to cancel order {}: {}", order.id, e);
            } else {
                // Unblock the scanner so the market can be re-entered if needed
                let mut guard = state.write().await;
                guard.pending_order_tokens.remove(tid);
            }
        }
    }

    Ok(())
}
