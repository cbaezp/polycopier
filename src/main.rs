pub mod api;
pub mod backoff;
pub mod clients;
pub mod config;
pub mod copied_counter;
pub mod copy_ledger;
pub mod listener;
pub mod log_capture;
pub mod models;
pub mod order_watcher;
pub mod position_scanner;
pub mod risk;
pub mod state;
pub mod strategy;
pub mod ui;
pub mod utils;
pub mod wallet_sync;

use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Run mode ──────────────────────────────────────────────────────────────
    // --daemon     Skip the TUI; log to stdout. Intended for server / PM2.
    // --ui         Skip the TUI; log to stdout; start Web UI; open browser.
    // (default)    Interactive TUI mode for local use.
    let args: Vec<String> = std::env::args().collect();
    let cli = crate::config::parse_cli_args(&args);
    let is_daemon = cli.is_daemon;
    let is_ui = cli.is_ui;
    let skip_open = cli.skip_open;
    let headless = cli.headless;

    // ── Tracing ───────────────────────────────────────────────────────────────
    // File log  : WARN+ always written to ./polycopier.log (full message, no truncation).
    // TUI mode  : WARN+ also captured in-memory and shown in the log panel.
    // Headless  : INFO+ written to stdout (journalctl / Docker friendly).

    // File layer — shared by both modes so errors are always inspectable.
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("polycopier.log")
        .expect("Failed to open polycopier.log");
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_target(true)
        .with_level(true)
        .with_ansi(false)
        .with_filter(tracing_subscriber::filter::LevelFilter::WARN);

    let log_buffer = if headless {
        use tracing_subscriber::EnvFilter;
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("polycopier=info,warn"));
        tracing_subscriber::registry()
            .with(file_layer)
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_level(true),
            )
            .init();
        log_capture::new_log_buffer()
    } else {
        let log_buffer = log_capture::new_log_buffer();
        let tui_layer = log_capture::TuiLogLayer::new(log_buffer.clone());
        tracing_subscriber::registry()
            .with(file_layer)
            .with(tui_layer.with_filter(tracing_subscriber::filter::LevelFilter::WARN))
            .init();
        log_buffer
    };

    if is_daemon {
        tracing::info!(
            "polycopier starting in DAEMON mode. Send SIGTERM or SIGINT (Ctrl-C) to stop."
        );
    } else if is_ui {
        tracing::info!(
            "polycopier starting in Web UI mode. Dashboard available at http://localhost:3000"
        );
        if !skip_open {
            let _ = std::process::Command::new("open")
                .arg("http://localhost:3000")
                .spawn();
        }
    }

    // ── Boot sequence ─────────────────────────────────────────────────────────

    // ── Boot sequence ─────────────────────────────────────────────────────────

    // Load .env early to evaluate whether keys are actually missing or invalid
    let _ = dotenvy::dotenv();
    let private_key = std::env::var("PRIVATE_KEY").unwrap_or_default();
    let needs_setup = private_key.trim_matches('"').trim_start_matches("0x").len() != 64
        || !std::path::Path::new("config.toml").exists();

    // If setup is needed and the user requested the Web UI setup mode,
    // we launch a dedicated lightweight Setup API on port 3000 and suspend the backend.
    // If they run the standard `cargo run`, it ignores this and uses the terminal natively.
    if is_ui && needs_setup {
        let setup_router = api::create_setup_router();
        tokio::spawn(async move {
            if let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:3000").await {
                let _ = axum::serve(listener, setup_router).await;
            }
        });
    }

    let mut config = config::Config::load_or_prompt(is_ui).await?;
    config.is_sim = cli.is_sim;
    config.sim_balance = cli.sim_balance;

    if config.is_sim {
        tracing::warn!("===========================================================");
        tracing::warn!("                 SIMULATION MODE ACTIVE                    ");
        tracing::warn!("   No real orders will be placed. Tracking mock P&L.       ");
        tracing::warn!("===========================================================");
    }

    let state = Arc::new(RwLock::new(state::BotState::new(
        config.is_sim,
        config.sim_balance,
    )));
    // Wrap RiskEngine in Arc<Mutex<>> so both strategy engine and order watcher
    // can reference it — order watcher calls record_loss() on loss-triggered cancels.
    let risk_engine = Arc::new(Mutex::new(risk::RiskEngine::new(config.clone())));

    let (poly_submitter, balance_fetcher, clob) = if config.is_sim {
        clients::build_sim_order_submitter(&config).await?
    } else {
        clients::build_order_submitter(&config).await?
    };

    let (event_tx, event_rx) = tokio::sync::mpsc::channel(100);
    listener::start_ws_listener(&config, event_tx.clone()).await?;

    // ── Copy ledger ───────────────────────────────────────────────────────────
    // Load the persisted copy ledger.  This records which positions we entered
    // and from which target wallet, enabling correct SELL classification and the
    // one-position-per-token rule across restarts.
    let ledger_path = if config.is_sim {
        "sim_copy_ledger.json"
    } else {
        "copy_ledger.json"
    };
    let copy_ledger = Arc::new(Mutex::new(copy_ledger::CopyLedger::load_from(ledger_path)));

    strategy::start_strategy_engine(
        event_rx,
        state.clone(),
        risk::RiskEngine::new(config.clone()),
        poly_submitter,
        config.clone(),
        copy_ledger.clone(),
        strategy::make_live_holds_query(),
    );

    // ── Background tasks ──────────────────────────────────────────────────────
    // Seed OUR positions AND live GTC orders before starting the scanner.
    // Both must complete so the scanner's first run sees accurate SkippedOwned
    // and already-queued state — preventing duplicate orders on restart.
    if !config.is_sim {
        wallet_sync::seed_own_positions(&config.funder_address, state.clone()).await;
        wallet_sync::seed_pending_orders(&clob, state.clone()).await;
    }

    // Reconcile the copy ledger against our live wallet positions.
    // Any open ledger entry where we no longer hold the token is marked closed.
    // This corrects for positions that were closed while the bot was offline.
    {
        let live_token_ids: HashSet<String> = {
            let guard = state.read().await;
            guard.positions.keys().cloned().collect()
        };
        let mut ledger = copy_ledger.lock().await;
        ledger.reconcile(&live_token_ids);
        // Prune closed entries older than configured retention period (Gap 11).
        ledger.prune_closed_older_than(config.ledger_retention_days);
    }

    // Ongoing wallet sync (positions, prices, balance) — all fire-and-forget loops.
    wallet_sync::start_position_sync(
        config.clone(), // Pass the full config so it can check is_sim
        state.clone(),
        copy_ledger.clone(), // Gap 4: passed so sync can update fill sizes
    );
    wallet_sync::start_price_refresh(config.target_wallets.clone(), state.clone());

    if !config.is_sim {
        wallet_sync::start_balance_poll(balance_fetcher, state.clone());
    }

    // Position scanner — safe to start now; seed has completed.
    position_scanner::start_position_scanner(config.clone(), state.clone(), event_tx.clone());

    // Position-close sweep — backstop that emits synthetic SELLs for any
    // position we hold that no target still holds (catches missed WS SELL events).
    // Gap 2 fix: pass copy_ledger so sweep uses the correct source_wallet.
    wallet_sync::start_position_close_sweep(
        config.target_wallets.clone(),
        state.clone(),
        event_tx,
        copy_ledger.clone(),
    );

    // Copied counter (header "Copied: N" — live API intersection every 30 s)
    copied_counter::start_copied_counter(
        config.funder_address.clone(),
        config.target_wallets.clone(),
        state.clone(),
        30,
    );

    // Order watcher (cancel stale GTC orders every 10 s)
    // Gap C + Gap E: now receives risk_engine Arc to call record_loss() on loss-triggered
    // cancellations, and uses exponential backoff on CLOB errors.
    if !config.is_sim {
        order_watcher::start_order_watcher(config.clone(), clob, state.clone(), risk_engine);
    }

    // ── Local Web API Server ──────────────────────────────────────────────────
    let api_router = api::create_router(state.clone(), copy_ledger.clone());
    if is_ui {
        tokio::spawn(async move {
            match tokio::net::TcpListener::bind("127.0.0.1:3000").await {
                Ok(listener) => {
                    if let Err(e) = axum::serve(listener, api_router).await {
                        tracing::error!("Local API Server crashed: {}", e);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "FATAL: Could not bind to Port 3000. Is another instance of Polycopier already running? Error: {}",
                        e
                    );
                    std::process::exit(1);
                }
            }
        });
    }

    // ── Main thread: TUI or headless wait ─────────────────────────────────────
    if headless {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate())?;
            tokio::select! {
                _ = tokio::signal::ctrl_c() => tracing::info!("Received SIGINT — shutting down."),
                _ = sigterm.recv()          => tracing::info!("Received SIGTERM — shutting down."),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await?;
            tracing::info!("Received Ctrl-C — shutting down.");
        }
    } else {
        match ui::start_tui(state.clone(), config.clone(), log_buffer).await? {
            ui::TuiExit::Quit => {}
            ui::TuiExit::Settings => {
                // Settings saved inside TUI — replace this process with a fresh instance.
                let exe = std::env::current_exe()?;
                let args: Vec<String> = std::env::args().collect();

                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    let err = std::process::Command::new(&exe).args(&args[1..]).exec();
                    return Err(anyhow::anyhow!("exec failed: {}", err));
                }
                #[cfg(not(unix))]
                {
                    let _ = std::process::Command::new(&exe).args(&args[1..]).spawn();
                    std::process::exit(0);
                }
            }
        }
    }

    Ok(())
}
