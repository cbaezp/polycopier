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
use tracing::{error, info};

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

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            for target in &targets {
                let target_addr = match Address::from_str(target) {
                    Ok(addr) => addr,
                    Err(_) => continue,
                };

                // Fetch the last 20 trades (was 5 - raised to survive burst activity)
                let req = match TradesRequest::builder()
                    .user(target_addr)
                    .limit(20)
                    .map(|b| b.build())
                {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                if let Ok(recent_trades) = data_client.trades(&req).await {
                    for trade in recent_trades {
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

                        if let Err(e) = tx.send(event).await {
                            error!("Failed to route trade event to engine: {}", e);
                            return;
                        }
                    }
                }
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
