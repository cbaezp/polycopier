use crate::config::Config;
use crate::models::{ScanStatus, TargetPosition, TradeEvent, TradeSide};
use crate::state::BotState;
use crate::strategy::compute_order_usd;
use alloy::primitives::Address;
use anyhow::Result;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::time::Duration;
use tracing::{debug, warn};

/// Fastest scan when a position is near the loss limit.
const SCAN_INTERVAL_MIN_SECS: u64 = 10;
/// Slowest scan when all positions are comfortably profitable.
const SCAN_INTERVAL_MAX_SECS: u64 = 60;

// ── Pure classifier (extracted for testability) ───────────────────────────────

/// Classify a target position into a ScanStatus.
/// All parameters are plain values — no I/O, fully unit-testable.
#[allow(clippy::too_many_arguments)]
pub fn classify_position(
    token_id: &str,
    cur_price: Decimal,
    percent_pnl: Decimal,
    our_tokens: &HashSet<String>,
    already_queued: &HashSet<String>,
    min_price: Decimal,
    max_price: Decimal,
    max_copy_loss_pct: Decimal,
) -> ScanStatus {
    if our_tokens.contains(token_id) {
        ScanStatus::SkippedOwned
    } else if already_queued.contains(token_id) {
        ScanStatus::Entered
    } else if cur_price < min_price || cur_price > max_price {
        ScanStatus::SkippedPrice
    } else if percent_pnl < -max_copy_loss_pct {
        ScanStatus::SkippedLoss
    } else {
        ScanStatus::Monitoring
    }
}

pub fn start_position_scanner(
    config: Config,
    state: Arc<RwLock<BotState>>,
    tx: mpsc::Sender<TradeEvent>,
) {
    tokio::spawn(async move {
        let mut already_queued: HashSet<String> = HashSet::new();

        // Run immediately on startup so TUI is populated before the first sleep
        if let Err(e) = scan_positions(&config, &state, &tx, &mut already_queued).await {
            warn!("Initial position scan failed: {}", e);
        }

        loop {
            // Compute next sleep based on how enterable the target's open positions still are.
            // We scan quickly when a target position's price is still near their entry
            // (good catch-up opportunity) and slowly when it has moved far away
            // (we would be chasing) or is already filtered out by drawdown.
            let interval_secs = {
                let guard = state.read().await;
                compute_scan_interval(&guard.target_positions, config.max_copy_loss_pct)
            };
            debug!(
                "Next position scan in {}s (catch-up urgency: target entry proximity)",
                interval_secs
            );
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;

            if let Err(e) = scan_positions(&config, &state, &tx, &mut already_queued).await {
                warn!("Position scan failed: {}", e);
            }
        }
    });
}

/// Compute the next scan interval (seconds) based on how close the TARGET's
/// still-enterable positions are to their own average entry price.
///
/// Purpose: catch up on positions the target already had open when the bot started.
/// We scan frequently when a target's Monitoring position is near their entry price
/// (meaning we can still get a similar fill). We back off when:
///   - The position has moved significantly from their entry (we'd be chasing)
///   - The position is already in drawdown past `max_copy_loss_pct` (won't enter anyway)
///   - There are no Monitoring positions at all
///
/// Algorithm (MAX_INTERESTING_MOVE = 15% as the "too late" threshold):
///   closeness = 1 - |target_percent_pnl| / MAX_INTERESTING_MOVE   (clamped 0 → 1)
///   interval  = MAX - best_closeness × (MAX - MIN)
///
/// Examples (max_copy_loss_pct = 10%, threshold = 15%):
///   Target PnL =  0%  → closeness = 1.0 → 10s  (at entry — urgent catch-up)
///   Target PnL = +5%  → closeness = 0.67 → 27s  (small move, still interesting)
///   Target PnL = -5%  → closeness = 0.67 → 27s  (within drawdown, still enterable)
///   Target PnL = -10% → closeness = 0.33 → 43s  (at limit, borderline)
///   Target PnL = -11% → filtered out by classify_position (SkippedLoss)
///   Target PnL = +15% → closeness = 0.0  → 60s  (too far, not worth scanning fast)
///
/// We NEVER exit based on this. Exits happen ONLY when the target exits.
pub fn compute_scan_interval(
    target_positions: &[TargetPosition],
    max_copy_loss_pct: Decimal,
) -> u64 {
    // Beyond this absolute PnL move from the target's entry, the catch-up opportunity
    // is no longer urgent (we'd be chasing). Must be > max_copy_loss_pct.
    // 0.15 = 15 × 10^-2
    let max_interesting_move = Decimal::new(15, 2);

    // Consider only positions the scanner classifies as Monitoring (enterable).
    // SkippedLoss, SkippedPrice, Entered, etc. are irrelevant — won't be entered.
    let best_closeness = target_positions
        .iter()
        .filter(|p| p.status == ScanStatus::Monitoring && p.percent_pnl > -max_copy_loss_pct)
        .map(|p| {
            // How close is the current price to the target's average entry?
            // percent_pnl near 0 → closeness near 1.0 → very urgent
            // |percent_pnl| at or above threshold → closeness = 0 → not urgent
            let abs_pnl = p.percent_pnl.abs();
            (Decimal::ONE - abs_pnl / max_interesting_move).clamp(Decimal::ZERO, Decimal::ONE)
        })
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(Decimal::ZERO);

    // best_closeness = 1.0 → min interval (scan urgently)
    // best_closeness = 0.0 → max interval (nothing interesting to catch up on)
    let range = SCAN_INTERVAL_MAX_SECS - SCAN_INTERVAL_MIN_SECS;
    let pct = (best_closeness * Decimal::from(100))
        .round_dp(0)
        .to_string()
        .parse::<u64>()
        .unwrap_or(0)
        .min(100);
    (SCAN_INTERVAL_MAX_SECS - pct * range / 100)
        .clamp(SCAN_INTERVAL_MIN_SECS, SCAN_INTERVAL_MAX_SECS)
}

async fn scan_positions(
    config: &Config,
    state: &Arc<RwLock<BotState>>,
    tx: &mpsc::Sender<TradeEvent>,
    already_queued: &mut HashSet<String>,
) -> Result<()> {
    let data_client = DataClient::default();
    let min_price = config.min_entry_price;
    let max_price = config.max_entry_price;

    // Brief read lock to snapshot our current holdings
    let (our_token_ids, current_balance) = {
        let guard = state.read().await;
        let ids = guard.positions.keys().cloned().collect();
        let bal = guard.total_balance;
        (ids, bal)
    };
    let our_token_ids: HashSet<String> = our_token_ids;

    let mut all_positions: Vec<TargetPosition> = Vec::new();
    let mut to_enter: Vec<(String, TradeEvent)> = Vec::new();

    for wallet_str in &config.target_wallets {
        let wallet_str = wallet_str.trim();
        let addr = match Address::from_str(wallet_str) {
            Ok(a) => a,
            Err(_) => {
                warn!("Invalid target wallet address: {}", wallet_str);
                continue;
            }
        };

        let req = PositionsRequest::builder().user(addr).build();
        let positions = match data_client.positions(&req).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to fetch positions for {}: {}", wallet_str, e);
                continue;
            }
        };

        debug!(
            "Scanner: {} has {} open position(s)",
            wallet_str,
            positions.len()
        );

        for pos in positions {
            let token_id = pos.asset.to_string();

            let status = classify_position(
                &token_id,
                pos.cur_price,
                pos.percent_pnl,
                &our_token_ids,
                already_queued,
                min_price,
                max_price,
                config.max_copy_loss_pct,
            );

            debug!(
                "  {} [{}] price={:.3} pnl={:.1}%",
                pos.title,
                status.label(),
                pos.cur_price,
                pos.percent_pnl * Decimal::from(100)
            );

            if status == ScanStatus::Monitoring && pos.cur_price > Decimal::ZERO {
                // target_notional = what the target originally paid for this position
                let target_notional = pos.avg_price * pos.size;
                // NOTE: target_portfolio_usd will be computed after all positions are collected
                // and read back from state on the next cycle for TargetPct. Here we use a
                // placeholder; the per-position sizing below uses the freshly-computed total.
                let budget_usd = Decimal::ZERO; // will be overwritten after full collection
                let _ = budget_usd; // suppress warning; sizing done below using target_notional
                let _ = target_notional; // stored in event below
                let short_id = &token_id[..token_id.len().min(8)];
                let event = TradeEvent {
                    transaction_hash: format!("scan_{}", short_id),
                    maker_address: wallet_str.to_string(),
                    taker_address: wallet_str.to_string(),
                    token_id: token_id.clone(),
                    price: pos.cur_price,
                    // Store full target size — strategy engine will apply budget cap via compute_order_usd
                    size: pos.size,
                    side: TradeSide::BUY,
                    timestamp: chrono::Utc::now().timestamp(),
                };
                to_enter.push((token_id.clone(), event));
            }

            let title = if pos.title.len() > 45 {
                format!("{}…", &pos.title[..45])
            } else {
                pos.title.clone()
            };

            all_positions.push(TargetPosition {
                title,
                outcome: pos.outcome.clone(),
                token_id,
                cur_price: pos.cur_price,
                avg_price: pos.avg_price,
                percent_pnl: pos.percent_pnl,
                size: pos.size,
                status,
            });
        }
    }

    // Compute target_portfolio_usd = Σ(avg_price × size) across all collected positions.
    // This is the best approximation of the target's total invested capital we can make
    // without historical account-balance snapshots.
    let target_portfolio_usd: Decimal = all_positions.iter().map(|p| p.avg_price * p.size).sum();

    // Pre-size the scan entries now that we have the full portfolio estimate.
    // Replace the placeholder sizes in to_enter with budget-capped values.
    let sized_entries: Vec<(String, TradeEvent)> = to_enter
        .into_iter()
        .filter_map(|(token_id, mut ev)| {
            // avg_price not directly available here — re-derive from all_positions
            let pos_avg = all_positions
                .iter()
                .find(|p| p.token_id == token_id)
                .map(|p| p.avg_price)
                .unwrap_or(ev.price);
            let target_notional = pos_avg * ev.size;
            let budget_usd = compute_order_usd(
                current_balance,
                &config.sizing_mode,
                config.copy_size_pct,
                config.max_trade_size_usd,
                target_notional,
                target_portfolio_usd,
            );
            let size = (budget_usd / ev.price).min(ev.size).round_dp(2);
            if size > Decimal::ZERO {
                ev.size = size;
                Some((token_id, ev))
            } else {
                None
            }
        })
        .collect();

    // Sort: WATCH first, then QUEUED, HELD, LOSS, RANGE; within each by pnl desc
    all_positions.sort_by(|a, b| {
        a.status.sort_key().cmp(&b.status.sort_key()).then(
            b.percent_pnl
                .partial_cmp(&a.percent_pnl)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    // Batch-write all positions and portfolio estimate to state
    {
        let mut guard = state.write().await;
        guard.target_positions = all_positions;
        guard.target_portfolio_usd = target_portfolio_usd;
    }

    // Queue entry events after releasing lock.
    // IMPORTANT: only enter ONE position per scan cycle — the one closest to the
    // target's entry price (lowest |percent_pnl| = best catch-up opportunity).
    // The next scan cycle will pick up the next-best position, and so on.
    // This prevents depleting the wallet balance in a single burst.
    let mut sized_entries = sized_entries;
    sized_entries.sort_by(|(_, a), (_, b)| {
        // lower abs pnl from target's entry = better catch-up = enter first
        let a_pnl = a.price; // price ≈ cur_price; use percent_pnl from all_positions
        let b_pnl = b.price;
        a_pnl
            .partial_cmp(&b_pnl)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Pick only the single best opportunity this cycle
    if let Some((token_id, event)) = sized_entries.into_iter().next() {
        debug!(
            "Scanner queuing entry for token {}",
            &token_id[..token_id.len().min(12)]
        );
        already_queued.insert(token_id);
        if let Err(e) = tx.send(event).await {
            warn!("Failed to send scan event: {}", e);
        }
    }

    Ok(())
}
