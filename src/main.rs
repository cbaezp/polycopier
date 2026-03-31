pub mod clients;
pub mod config;
pub mod copied_counter;
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

    // 10. Dedicated "Copied" counter: queries our wallet and each target via the
    //     API every 30 seconds and writes the intersection count to state.copied_count.
    copied_counter::start_copied_counter(
        config.funder_address.clone(),
        config.target_wallets.clone(),
        state.clone(),
        30,
    );

    // 11. Dedicated price refresh task.
    //     The scanner's adaptive interval can reach 60s when all target positions are
    //     deeply in-the-money (best_closeness = 0). This means OUR_PNL% in the Copied
    //     table would show prices up to 60s stale.
    //     This task refreshes cur_price in target_positions every 20 seconds,
    //     completely independent of scanner urgency. It patches ONLY cur_price -- it
    //     does not re-run classification logic or queue any trade events.
    {
        use alloy::primitives::Address;
        use polymarket_client_sdk::data::types::request::PositionsRequest;
        use polymarket_client_sdk::data::Client as DataClient;
        use std::collections::HashMap;
        use std::str::FromStr;

        let targets = config.target_wallets.clone();
        let state_pr = state.clone();

        tokio::spawn(async move {
            let client = DataClient::default();
            // Small initial delay so scanner runs first and populates target_positions
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            loop {
                // Fetch fresh prices from each target wallet
                let mut price_map: HashMap<String, rust_decimal::Decimal> = HashMap::new();
                for wallet_str in &targets {
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
                    let mut g = state_pr.write().await;
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

    // 12. Poll live USDC balance every 10 seconds and update TUI

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
            drop(config::Config::load_or_prompt().await?);

            // Replace the current process entirely (execv) so that:
            //   - All background tasks (scanner, listener, etc.) are gone.
            //   - No duplicate bots fight over the same terminal.
            //   - The new process starts fresh with the updated .env.
            let exe = std::env::current_exe()?;
            let args: Vec<String> = std::env::args().collect();

            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let err = std::process::Command::new(&exe).args(&args[1..]).exec(); // never returns on success
                return Err(anyhow::anyhow!("exec failed: {}", err));
            }

            #[cfg(not(unix))]
            {
                // Fallback for non-Unix: spawn child, then exit this process.
                let _ = std::process::Command::new(&exe).args(&args[1..]).spawn();
                std::process::exit(0);
            }
        }
    }

    Ok(())
}
