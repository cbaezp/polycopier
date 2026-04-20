use crate::backoff::next_backoff;
use crate::config::Config;
use crate::models::{ScanStatus, TargetPosition, TradeEvent, TradeSide};
use crate::state::BotState;
use crate::strategy::compute_order_usd;
use alloy::primitives::Address;
use anyhow::Result;
use chrono::NaiveDate;
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

// -- Pure classifier (extracted for testability) -------------------------------

/// Classify a target position into a ScanStatus.
/// All parameters are plain values - no I/O, fully unit-testable.
#[allow(clippy::too_many_arguments)]
pub fn classify_position(
    token_id: &str,
    cur_price: Decimal,
    percent_pnl: Decimal,
    redeemable: bool,
    end_date: Option<NaiveDate>,
    our_tokens: &HashSet<String>,
    already_queued: &HashSet<String>,
    min_price: Decimal,
    max_price: Decimal,
    max_copy_loss_pct: Decimal,
    max_copy_gain_pct: Decimal,
    position_size: Decimal,
    min_amount: Decimal,
    max_amount: Decimal,
) -> ScanStatus {
    // Reject resolved markets (redeemable=true) and markets whose end date has
    // strictly passed (end_date < today).
    //
    // We intentionally use < today, NOT <= today.
    //
    // Same-day markets (end_date == today) are still open and accepting orders.
    // A 5-min BTC market resolving at 9:05am is still valid to enter at 9:01am.
    // A daily "BTC above $80k on April 1" market is valid at 6am even if endDate=today.
    //
    // The authoritative "market is over" signal is redeemable=true, which Polymarket
    // sets once settlement is confirmed on-chain. We rely on that, not the date.
    //
    // end_date < today is only a backstop for the edge case where redeemable hasn't
    // been flipped yet for a market that resolved yesterday or earlier.
    let today = chrono::Utc::now().date_naive();
    if redeemable || end_date.is_some_and(|d| d < today) {
        return ScanStatus::SkippedExpired;
    }
    if our_tokens.contains(token_id) {
        ScanStatus::SkippedOwned
    } else if already_queued.contains(token_id) {
        ScanStatus::Entered
    } else if cur_price < min_price || cur_price > max_price {
        ScanStatus::SkippedPrice
    } else if (position_size * cur_price) < min_amount || (position_size * cur_price) > max_amount {
        ScanStatus::SkippedSize
    } else if percent_pnl < -max_copy_loss_pct {
        ScanStatus::SkippedLoss
    } else if percent_pnl > max_copy_gain_pct {
        ScanStatus::SkippedGain
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
        // Pre-populate already_queued from positions we already hold AND from
        // live GTC orders seeded from the CLOB at boot. This prevents re-entry
        // of both filled positions and pending-but-unfilled orders on restart.
        let mut already_queued: HashSet<String> = {
            let guard = state.read().await;
            guard
                .positions
                .keys()
                .cloned()
                .chain(guard.pending_orders.keys().cloned())
                .collect()
        };

        // Run immediately on startup so TUI is populated before the first sleep
        if let Err(e) = scan_positions(&config, &state, &tx, &mut already_queued).await {
            warn!("Initial position scan failed: {}", e);
        } else {
            let mut g = state.write().await;
            g.last_scan_at = Some(std::time::Instant::now());
        }

        // Gap 6: track consecutive scan failures for exponential backoff.
        let mut consecutive_errors: u32 = 0;

        loop {
            // Compute next sleep based on how enterable the target's open positions still are.
            let adaptive_interval = {
                let guard = state.read().await;
                compute_scan_interval(&guard.target_positions, config.max_copy_loss_pct)
            };

            // Chronological Alignment: High-Frequency Minute Burst.
            // Rapidly poll every 1 second during the minute crossover (:58 to :03) to snipe new time-based positions.
            let now = chrono::Utc::now();
            use chrono::Timelike;
            let current_sec = now.second();

            let secs_until_burst = if !(4..58).contains(&current_sec) {
                // We are inside the aggressive strike window. Sleep for exactly 1 second.
                1
            } else {
                // Outside the window. Sleep normally, but guarantee we wake up EXACTLY at :58.
                (58 - current_sec) as u64
            };

            // Intercept: use shortest interval between adaptive needs and chronological alignment.
            let interval_secs = adaptive_interval.min(secs_until_burst);

            // Apply backoff on top of normal interval if API is misbehaving.
            let sleep_secs = if consecutive_errors > 0 {
                let backoff = next_backoff(consecutive_errors, interval_secs, 300);
                warn!(
                    "Scanner: backing off {}s after {} consecutive scan errors.",
                    backoff, consecutive_errors
                );
                backoff
            } else {
                interval_secs
            };

            debug!(
                "Next position scan in {}s (catch-up urgency: target entry proximity)",
                sleep_secs
            );
            // Record the scheduled interval so the TUI can show a countdown
            {
                let mut g = state.write().await;
                g.next_scan_secs = sleep_secs;
            }
            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;

            match scan_positions(&config, &state, &tx, &mut already_queued).await {
                Ok(()) => {
                    consecutive_errors = 0;
                    let mut g = state.write().await;
                    g.last_scan_at = Some(std::time::Instant::now());
                }
                Err(e) => {
                    consecutive_errors += 1;
                    warn!(
                        "Position scan failed (consecutive={}): {}",
                        consecutive_errors, e
                    );
                }
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
///   closeness = 1 - |target_percent_pnl| / MAX_INTERESTING_MOVE   (clamped 0 -> 1)
///   interval  = MAX - best_closeness x (MAX - MIN)
///
/// Examples (max_copy_loss_pct = 10%, threshold = 15%):
///   Target PnL =  0%  -> closeness = 1.0 -> 10s  (at entry - urgent catch-up)
///   Target PnL = +5%  -> closeness = 0.67 -> 27s  (small move, still interesting)
///   Target PnL = -5%  -> closeness = 0.67 -> 27s  (within drawdown, still enterable)
///   Target PnL = -10% -> closeness = 0.33 -> 43s  (at limit, borderline)
///   Target PnL = -11% -> filtered out by classify_position (SkippedLoss)
///   Target PnL = +15% -> closeness = 0.0  -> 60s  (too far, not worth scanning fast)
///
/// We NEVER exit based on this. Exits happen ONLY when the target exits.
pub fn compute_scan_interval(
    target_positions: &[TargetPosition],
    max_copy_loss_pct: Decimal,
) -> u64 {
    // Beyond this absolute PnL move from the target's entry, the catch-up opportunity
    // is no longer urgent (we'd be chasing). Must be > max_copy_loss_pct.
    // 0.15 = 15 x 10^-2
    let max_interesting_move = Decimal::new(15, 2);

    // Consider only positions the scanner classifies as Monitoring (enterable).
    // SkippedLoss, SkippedPrice, Entered, etc. are irrelevant - won't be entered.
    let best_closeness = target_positions
        .iter()
        .filter(|p| p.status == ScanStatus::Monitoring && p.percent_pnl > -max_copy_loss_pct)
        .map(|p| {
            // How close is the current price to the target's average entry?
            // percent_pnl near 0 -> closeness near 1.0 -> very urgent
            // |percent_pnl| at or above threshold -> closeness = 0 -> not urgent
            let abs_pnl = p.percent_pnl.abs();
            (Decimal::ONE - abs_pnl / max_interesting_move).clamp(Decimal::ZERO, Decimal::ONE)
        })
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(Decimal::ZERO);

    // best_closeness = 1.0 -> min interval (scan urgently)
    // best_closeness = 0.0 -> max interval (nothing interesting to catch up on)
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

    // Gap D fix: sync already_queued with the live pending_orders HashMap.
    // The order watcher calls `guard.pending_orders.remove(tid)` when it
    // cancels a GTC order. Without this sync, the scanner would permanently treat
    // those tokens as "already queued" until the bot restarts.
    //
    // We drop any token from already_queued that is no longer in pending_orders
    // AND is no longer held outright in our positions. This allows the scanner to
    // re-enter a position after a GTC cancellation (e.g., target recovered from
    // drawdown) while keeping the "skip positions we already hold" invariant.
    {
        let guard = state.read().await;
        already_queued.retain(|tid| {
            guard.pending_orders.contains_key(tid) || guard.positions.contains_key(tid)
        });
    }

    // Brief read lock to snapshot our current holdings
    let (our_token_ids, current_balance) = {
        let guard = state.read().await;
        let ids = guard.positions.keys().cloned().collect();
        let bal = guard.total_balance;
        (ids, bal)
    };
    let our_token_ids: HashSet<String> = our_token_ids;

    let mut all_positions: Vec<TargetPosition> = Vec::new();
    // Gap 3 fix: tuple includes percent_pnl so we can sort by it correctly.
    let mut to_enter: Vec<(String, TradeEvent, Decimal)> = Vec::new();

    for wallet_str in &config.target_wallets {
        let wallet_str = wallet_str.trim();
        let addr = match Address::from_str(wallet_str) {
            Ok(a) => a,
            Err(_) => {
                warn!("Invalid target wallet address: {}", wallet_str);
                continue;
            }
        };

        let positions = match crate::utils::fetch_all_positions(&data_client, addr).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to fetch positions for {}: {}", wallet_str, e);
                return Err(e); // let the caller apply backoff
            }
        };

        debug!(
            "Scanner: {} has {} open position(s)",
            wallet_str,
            positions.len()
        );

        for pos in positions {
            let token_id = pos.asset.to_string();
            let pnl_frac = pos.percent_pnl / Decimal::from(100);

            let status = classify_position(
                &token_id,
                pos.cur_price,
                pnl_frac,
                pos.redeemable,
                pos.end_date,
                &our_token_ids,
                already_queued,
                min_price,
                max_price,
                config.max_copy_loss_pct,
                config.max_copy_gain_pct,
                pos.size,
                config.scan_min_amount,
                config.scan_max_amount,
            );

            debug!(
                "  {} [{}] price={:.3} pnl={:.1}%",
                pos.title,
                status.label(),
                pos.cur_price,
                pos.percent_pnl
            );

            if status == ScanStatus::Monitoring && pos.avg_price > Decimal::ZERO {
                let short_id = &token_id[..token_id.len().min(8)];
                let event = TradeEvent {
                    transaction_hash: format!("scan_{}", short_id),
                    maker_address: wallet_str.to_string(),
                    taker_address: wallet_str.to_string(),
                    token_id: token_id.clone(),
                    // Use avg_price (what the target paid) not cur_price.
                    price: pos.avg_price,
                    // Store full target size -- strategy engine will apply budget cap
                    size: pos.size,
                    side: TradeSide::BUY,
                    timestamp: chrono::Utc::now().timestamp(),
                };
                // Gap 3 fix: store percent_pnl alongside the event for correct sorting.
                to_enter.push((token_id.clone(), event, pnl_frac));
            }

            let title = if pos.title.chars().count() > 45 {
                format!("{}...", pos.title.chars().take(45).collect::<String>())
            } else {
                pos.title.clone()
            };

            let engine_reason = {
                let guard = state.read().await;
                guard.rejection_reasons.get(&token_id).cloned()
            };

            all_positions.push(TargetPosition {
                title,
                outcome: pos.outcome.clone(),
                token_id: token_id.clone(),
                cur_price: pos.cur_price,
                avg_price: pos.avg_price,
                percent_pnl: pnl_frac,
                size: pos.size,
                status,
                source_wallet: wallet_str.to_string(),
                engine_reason,
            });
        }
    }

    let mut sized_entries: Vec<(String, TradeEvent, Decimal)> = Vec::new();
    let mut dropped_entries = Vec::new();
    let mut cleared_entries = Vec::new();

    for (token_id, mut ev, percent_pnl) in to_enter {
        let pos_avg = all_positions
            .iter()
            .find(|p| p.token_id == token_id)
            .map(|p| p.avg_price)
            .unwrap_or(ev.price);
        let target_notional = pos_avg * ev.size;
        let wallet_scalar = config
            .target_scalars
            .get(&ev.maker_address)
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
        let size = (budget_usd / ev.price).min(ev.size).round_dp(2);
        if size > Decimal::ZERO {
            ev.size = size;
            sized_entries.push((token_id.clone(), ev, percent_pnl));
            cleared_entries.push(token_id);
        } else {
            let msg = if current_balance < Decimal::ONE {
                format!("Scanner skipped: Insufficient wallet balance to meet $1.00 minimum entry (Balance: ${:.2})", current_balance)
            } else {
                format!(
                    "Scanner skipped: Scaled budget evaluated to < $1.00 min threshold (target notional: ${:.2}, scalar: {})",
                    target_notional, wallet_scalar
                )
            };
            dropped_entries.push((token_id, msg));
        }
    }

    if !dropped_entries.is_empty() || !cleared_entries.is_empty() {
        let mut guard = state.write().await;
        for (token_id, msg) in dropped_entries {
            guard.rejection_reasons.insert(token_id, msg);
        }
        for token_id in cleared_entries {
            guard.rejection_reasons.remove(&token_id);
        }
    }

    // Sort: WATCH first, then QUEUED, HELD, LOSS, RANGE; within each by pnl desc
    all_positions.sort_by(|a, b| {
        a.status.sort_key().cmp(&b.status.sort_key()).then(
            b.percent_pnl
                .partial_cmp(&a.percent_pnl)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    // Batch-write all positions to state
    {
        let mut guard = state.write().await;
        guard.target_positions = all_positions;
        guard.last_scan_at = Some(std::time::Instant::now());
    }

    // Gap 3 fix: sort by |percent_pnl| ascending so the position closest to the
    // target's entry price (freshest opportunity) is entered first.
    // Previously this sorted by ev.price (token price in $) which was semantically wrong.
    sized_entries.sort_by(|(_, _, pnl_a), (_, _, pnl_b)| {
        pnl_a
            .abs()
            .partial_cmp(&pnl_b.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Gap 8: queue up to scan_max_entries_per_cycle positions per cycle
    // (default 1 = conservative; configurable via SCAN_MAX_ENTRIES_PER_CYCLE).
    // Sort guarantees the freshest catch-up opportunities are taken first.
    let max_entries = config.scan_max_entries_per_cycle;
    for (token_id, event, _pnl) in sized_entries.into_iter().take(max_entries) {
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
