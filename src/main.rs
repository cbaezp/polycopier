pub mod config;
pub mod listener;
pub mod strategy;
pub mod risk;
pub mod state;
pub mod ui;
pub mod models;
pub mod clients;
pub mod utils;
pub mod position_scanner;

use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize logging (to a file or standard error to not break TUI)
    // In TUI apps, it's better to log to a file. We'll use a basic subscriber for now.
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        // .with_writer(std::fs::File::create("bot.log")?)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    info!("Starting Polymarket Copy Trading Bot...");

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
        config.clone()
    );

    // 9. Scan target wallets for pre-existing open positions (startup + every 60s)
    position_scanner::start_position_scanner(
        config.clone(),
        state.clone(),
        event_tx,
    );

    // 8. Poll live USDC balance every 10 seconds and update TUI
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

    // 8. Start Terminal UI (Blocks main thread)
    ui::start_tui(state.clone(), config.clone()).await?;

    info!("Shutting down gracefully.");
    Ok(())
}
