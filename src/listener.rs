use crate::backoff::next_backoff;
use crate::config::Config;
use crate::models::{TradeEvent, TradeSide};
use alloy::primitives::Address;
use anyhow::Result;
use chrono::Utc;
use polymarket_client_sdk::data::types::request::TradesRequest;
use polymarket_client_sdk::data::Client as DataClient;
use std::collections::{HashSet, VecDeque};
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// How many recent tx hashes to remember for deduplication.
/// At 20 trades/poll x 1 poll/2s, 500 covers ~50 seconds of burst.
const SEEN_HASHES_CAP: usize = 500;

pub async fn start_ws_listener(config: &Config, tx: mpsc::Sender<TradeEvent>) -> Result<()> {
    let targets = config.target_wallets.clone();
    let data_client = DataClient::default();

    tokio::spawn(async move {
        info!("Started tracking third-party targets: {:?}", targets);

        // Deduplicate by transaction hash instead of a timestamp watermark.
        // Timestamps are coarse (seconds) and drop burst trades - hashes are exact.
        let mut seen_hashes: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        // Gap 6: track consecutive API errors per target for backoff.
        let mut consecutive_errors: u32 = 0;

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut had_error = false;

            for target in &targets {
                let target_addr = match Address::from_str(target) {
                    Ok(addr) => addr,
                    Err(_) => continue,
                };

                // Fetch the last 1000 trades (was 20 - raised to survive hyper-active algorithmic targets)
                let req = match TradesRequest::builder()
                    .user(target_addr)
                    .limit(1000)
                    .map(|b| b.build())
                {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                let trades = match data_client.trades(&req).await {
                    Ok(t) => {
                        // Success — backoff reset happens after the full loop
                        t
                    }
                    Err(e) => {
                        had_error = true;
                        warn!(
                            "Listener: API error for {}: {e} (consecutive_errors={})",
                            &target[..target.len().min(10)],
                            consecutive_errors + 1
                        );
                        continue; // skip this target this cycle
                    }
                };

                for trade in trades {
                    let hash = trade.transaction_hash.to_string();

                    // Skip already-processed trades
                    if seen_hashes.contains(&hash) {
                        continue;
                    }

                    // Skip trades that are too old (more than 5 minutes)
                    // to avoid replaying history on reconnect / restart
                    let age_secs = Utc::now().timestamp() - trade.timestamp;
                    if age_secs > 300 {
                        // Still mark as seen so we don't re-evaluate on next poll
                        evict_and_insert(&mut seen_hashes, &mut seen_order, hash);
                        continue;
                    }

                    let side = match trade.side {
                        polymarket_client_sdk::data::types::Side::Buy => TradeSide::BUY,
                        _ => TradeSide::SELL,
                    };

                    let event = TradeEvent {
                        transaction_hash: hash.clone(),
                        // Normalize to lowercase: alloy Address::to_string() produces
                        // EIP-55 checksum (mixed-case). config.target_wallets stores
                        // lowercase strings from .env. Vec::contains is case-sensitive,
                        // so without normalization every live trade fails the wallet check.
                        maker_address: trade.proxy_wallet.to_string().to_lowercase(),
                        taker_address: trade.proxy_wallet.to_string().to_lowercase(),
                        token_id: trade.asset.to_string(),
                        price: trade.price,
                        size: trade.size,
                        side,
                        timestamp: trade.timestamp,
                    };

                    evict_and_insert(&mut seen_hashes, &mut seen_order, hash);

                    // Gap 10: use try_send instead of send().await to avoid
                    // blocking the polling loop when the strategy channel is full.
                    match tx.try_send(event) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            warn!(
                                "Listener: strategy channel full — dropping event (backpressure)"
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            error!("Listener: strategy channel closed — shutting down listener.");
                            return;
                        }
                    }
                }
            }

            // Gap 6: exponential backoff on sustained API errors.
            if had_error {
                consecutive_errors += 1;
                let backoff_secs = next_backoff(consecutive_errors, 2, 120);
                if consecutive_errors > 1 {
                    warn!(
                        "Listener: backing off {}s after {} consecutive errors.",
                        backoff_secs, consecutive_errors
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
                }
            } else {
                consecutive_errors = 0; // reset on clean cycle
            }
        }
    });

    Ok(())
}

/// Insert a hash into the dedup set, evicting the oldest entry when capacity is reached.
fn evict_and_insert(seen: &mut HashSet<String>, order: &mut VecDeque<String>, hash: String) {
    if order.len() >= SEEN_HASHES_CAP {
        if let Some(oldest) = order.pop_front() {
            seen.remove(&oldest);
        }
    }
    seen.insert(hash.clone());
    order.push_back(hash);
}
