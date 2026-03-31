use crate::config::Config;
use crate::models::{TradeEvent, TradeSide};
use alloy::primitives::Address;
use anyhow::Result;
use chrono::Utc;
use polymarket_client_sdk::data::types::request::TradesRequest;
use polymarket_client_sdk::data::Client as DataClient;
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::{error, info};

pub async fn start_ws_listener(config: &Config, tx: mpsc::Sender<TradeEvent>) -> Result<()> {
    let targets = config.target_wallets.clone();

    // Using Data API to poll arbitrary wallet trades since WS is auth-limited
    let data_client = DataClient::default();

    tokio::spawn(async move {
        info!("Started tracking third-party targets: {:?}", targets);
        let mut last_processed_time = Utc::now().timestamp();

        loop {
            // Poll every 2 seconds to avoid rate limits initially
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            for target in &targets {
                let target_addr = match Address::from_str(target) {
                    Ok(addr) => addr,
                    Err(_) => continue,
                };

                // Fetch recent manual trades for target
                let req = TradesRequest::builder()
                    .user(target_addr)
                    .limit(5)
                    .unwrap()
                    .build();

                if let Ok(recent_trades) = data_client.trades(&req).await {
                    for trade in recent_trades {
                        // Only process trades newer than the last poll cycle
                        if trade.timestamp <= last_processed_time {
                            continue;
                        }

                        let side = match trade.side {
                            polymarket_client_sdk::data::types::Side::Buy => TradeSide::BUY,
                            _ => TradeSide::SELL,
                        };

                        let event = TradeEvent {
                            // B256 displays as 0x-prefixed hex string
                            transaction_hash: trade.transaction_hash.to_string(),
                            // The proxy_wallet IS the trader identity — no separate maker/taker
                            // we use it for both fields; strategy filters by target_wallets
                            maker_address: trade.proxy_wallet.to_string(),
                            taker_address: trade.proxy_wallet.to_string(),
                            // U256 displays as decimal string matching token_id format
                            token_id: trade.asset.to_string(),
                            price: trade.price,
                            size: trade.size,
                            side,
                            timestamp: trade.timestamp,
                        };

                        if let Err(e) = tx.send(event).await {
                            error!("Failed to route trade event to engine: {}", e);
                            return;
                        }
                    }
                }
            }
            last_processed_time = Utc::now().timestamp();
        }
    });

    Ok(())
}
