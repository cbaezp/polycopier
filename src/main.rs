pub mod clients;
pub mod config;
pub mod listener;
pub mod log_capture;
pub mod models;
pub mod position_scanner;
pub mod risk;
pub mod state;
pub mod strategy;
pub mod ui;
pub mod utils;

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Set up in-memory log capture so tracing output goes to the TUI log
    //    panel instead of corrupting the alternate-screen TUI.
    //    RUST_LOG is still honoured for the level filter.
    let log_buffer = log_capture::new_log_buffer();
    let tui_layer = log_capture::TuiLogLayer::new(log_buffer.clone());
    let level_filter = tracing_subscriber::filter::LevelFilter::WARN;
    tracing_subscriber::registry()
        .with(tui_layer.with_filter(level_filter))
        .init();

    // 2. Load Configuration via Prompt Wizard
    let config = config::Config::load_or_prompt().await?;

    // 3. Initialize Shared State
    let state = Arc::new(RwLock::new(state::BotState::new()));

    // 4. Initialize Risk Engine
    let risk_engine = risk::RiskEngine::new(config.clone());

    // 5. Initialize API / RPC Clients
    let (poly_submitter, balance_fetcher) = clients::build_order_submitter(&config).await?;

    // 6. Connect WebSocket Listener (Spawns Task)
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(100);
    listener::start_ws_listener(&config, event_tx.clone()).await?;

    // 7. Start Strategy & Execution Engines
    strategy::start_strategy_engine(
        event_rx,
        state.clone(),
        risk_engine,
        poly_submitter,
        config.clone(),
    );

    // 8. Seed OUR own open positions on startup so the scanner doesn't re-enter
    //    positions we already hold from a previous session.
    {
        use alloy::primitives::Address;
        use polymarket_client_sdk::data::types::request::PositionsRequest;
        use polymarket_client_sdk::data::Client as DataClient;
        use std::str::FromStr;

        let funder = config.funder_address.clone();
        let state_seed = state.clone();
        tokio::spawn(async move {
            let data_client = DataClient::default();
            if let Ok(addr) = Address::from_str(&funder) {
                let req = PositionsRequest::builder().user(addr).build();
                match data_client.positions(&req).await {
                    Ok(positions) => {
                        let mut guard = state_seed.write().await;
                        for p in positions {
                            let token_id = p.asset.to_string();
                            guard.positions.insert(
                                token_id.clone(),
                                crate::models::Position {
                                    token_id,
                                    size: p.size,
                                    average_entry_price: p.avg_price,
                                },
                            );
                        }
                        tracing::warn!(
                            "Seeded {} existing position(s) from wallet - scanner will skip these.",
                            guard.positions.len()
                        );
                    }
                    Err(e) => tracing::warn!("Could not seed own positions on startup: {}", e),
                }
            }
        });
    }

    // 9. Scan target wallets for pre-existing open positions (startup + adaptive interval)
    position_scanner::start_position_scanner(config.clone(), state.clone(), event_tx);

    // 10. Dedicated "Copied" counter task.
    //     Independently queries our wallet and each target wallet via the API,
    //     computes the intersection of held token IDs, and writes to
    //     state.copied_count every 30 seconds.
    //     This is the authoritative source for the TUI "Copied" counter --
    //     it never relies on local scanner state or session history.
    {
        use alloy::primitives::Address;
        use polymarket_client_sdk::data::types::request::PositionsRequest;
        use polymarket_client_sdk::data::Client as DataClient;
        use std::collections::HashSet;
        use std::str::FromStr;

        let funder = config.funder_address.clone();
        let targets = config.target_wallets.clone();
        let state_cc = state.clone();

        tokio::spawn(async move {
            let client = DataClient::default();
            loop {
                // -- Fetch OUR positions ---------------------------------------
                let our_tokens: HashSet<String> = if let Ok(addr) = Address::from_str(&funder) {
                    let req = PositionsRequest::builder().user(addr).build();
                    match client.positions(&req).await {
                        Ok(ps) => ps.into_iter().map(|p| p.asset.to_string()).collect(),
                        Err(e) => {
                            tracing::warn!("copied_count: failed to fetch our positions: {}", e);
                            HashSet::new()
                        }
                    }
                } else {
                    HashSet::new()
                };

                // -- Fetch TARGET positions and intersect ----------------------
                let mut count = 0usize;
                for wallet_str in &targets {
                    let wallet_str = wallet_str.trim();
                    if let Ok(addr) = Address::from_str(wallet_str) {
                        let req = PositionsRequest::builder().user(addr).build();
                        match client.positions(&req).await {
                            Ok(ps) => {
                                count += ps
                                    .iter()
                                    .filter(|p| our_tokens.contains(&p.asset.to_string()))
                                    .count();
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "copied_count: failed to fetch target {}: {}",
                                    wallet_str,
                                    e
                                );
                            }
                        }
                    }
                }

                // -- Write to state -------------------------------------------
                {
                    let mut g = state_cc.write().await;
                    g.copied_count = count;
                }

                tracing::debug!(
                    "copied_count: {} position(s) mirrored from target(s)",
                    count
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
            }
        });
    }

    // 10. Poll live USDC balance every 10 seconds and update TUI
    {
        let state = state.clone();
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

    // 11. Start Terminal UI (blocks main thread).
    //     Returns TuiExit::Settings when [s] is pressed, causing us to
    //     re-run the config wizard and restart the bot.
    match ui::start_tui(state.clone(), config.clone(), log_buffer.clone()).await? {
        ui::TuiExit::Quit => {}
        ui::TuiExit::Settings => {
            // Re-run the wizard; the new .env is written by load_or_prompt.
            // Then restart the process in-place so the new config is loaded.
            drop(config::Config::load_or_prompt().await?);
            let exe = std::env::current_exe()?;
            let args: Vec<String> = std::env::args().collect();
            let _ = std::process::Command::new(&exe).args(&args[1..]).status();
        }
    }

    Ok(())
}
