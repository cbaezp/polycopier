use crate::config::Config;
use crate::models::{ScanStatus, TargetPosition, TradeEvent, TradeSide};
use crate::state::BotState;
use anyhow::Result;
use polymarket_client_sdk::data::Client as DataClient;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use alloy::primitives::Address;

const SCAN_INTERVAL_SECS: u64 = 60;
const MIN_ENTRY_PRICE: &str = "0.02";
const MAX_ENTRY_PRICE: &str = "0.95";

// ── Pure classifier (extracted for testability) ───────────────────────────────

/// Classify a target position into a ScanStatus.
/// All parameters are plain values — no I/O, fully unit-testable.
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
        // Tracks tokens we've already queued this session — don't re-enter them
        let mut already_queued: HashSet<String> = HashSet::new();

        // Run immediately on startup so TUI is populated before the next 60s tick
        if let Err(e) = scan_positions(&config, &state, &tx, &mut already_queued).await {
            warn!("Initial position scan failed: {}", e);
        }

        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(SCAN_INTERVAL_SECS));
        interval.tick().await; // consume the immediate tick

        loop {
            interval.tick().await;
            if let Err(e) = scan_positions(&config, &state, &tx, &mut already_queued).await {
                warn!("Position scan failed: {}", e);
            }
        }
    });
}

async fn scan_positions(
    config: &Config,
    state: &Arc<RwLock<BotState>>,
    tx: &mpsc::Sender<TradeEvent>,
    already_queued: &mut HashSet<String>,
) -> Result<()> {
    let data_client = DataClient::default();
    let min_price = Decimal::from_str(MIN_ENTRY_PRICE)?;
    let max_price = Decimal::from_str(MAX_ENTRY_PRICE)?;

    // Brief read lock to snapshot our current holdings
    let our_token_ids: HashSet<String> = {
        let guard = state.read().await;
        guard.positions.keys().cloned().collect()
    };

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

        info!("Scanner: {} has {} open position(s)", wallet_str, positions.len());

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

            if status == ScanStatus::Monitoring {
                if pos.cur_price > Decimal::ZERO {
                    let size = (config.max_trade_size_usd / pos.cur_price)
                        .min(pos.size)
                        .round_dp(2);

                    if size > Decimal::ZERO {
                        let short_id = &token_id[..token_id.len().min(8)];
                        let event = TradeEvent {
                            transaction_hash: format!("scan_{}", short_id),
                            maker_address: wallet_str.to_string(),
                            taker_address: wallet_str.to_string(),
                            token_id: token_id.clone(),
                            price: pos.cur_price,
                            size,
                            side: TradeSide::BUY,
                            timestamp: chrono::Utc::now().timestamp(),
                        };
                        to_enter.push((token_id.clone(), event));
                    }
                }
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

    // Sort: WATCH first, then QUEUED, HELD, LOSS, RANGE; within each by pnl desc
    all_positions.sort_by(|a, b| {
        a.status.sort_key().cmp(&b.status.sort_key())
            .then(b.percent_pnl.partial_cmp(&a.percent_pnl).unwrap_or(std::cmp::Ordering::Equal))
    });

    // Batch-write all positions to state
    {
        let mut guard = state.write().await;
        guard.target_positions = all_positions;
    }

    // Queue entry events after releasing lock
    for (token_id, event) in to_enter {
        info!("Scanner queuing entry for token {}", &token_id[..token_id.len().min(12)]);
        already_queued.insert(token_id);
        if let Err(e) = tx.send(event).await {
            warn!("Failed to send scan event: {}", e);
            break;
        }
    }

    Ok(())
}
