use crate::config::BotConfig;
use crate::models::{EvaluatedTrade, Position, TargetPosition};
use crate::state::BotState;
use axum::{
    extract::{Json, State},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct ApiState {
    pub bot_state: Arc<RwLock<BotState>>,
    pub copy_ledger: Arc<tokio::sync::Mutex<crate::copy_ledger::CopyLedger>>,
}

// ── SETUP ROUTER (when .env is missing) ───────────────────────────────────

pub fn create_setup_router() -> Router {
    Router::new()
        .route(
            "/api/state",
            get(|| async { axum::Json(serde_json::json!({ "status": "setup_required" })) }),
        )
        .route("/api/setup", post(handle_setup))
        .fallback_service(
            tower_http::services::ServeDir::new("web/dist").append_index_html_on_directories(true),
        )
}

#[derive(serde::Deserialize)]
pub struct SetupPayload {
    pub private_key: String,
    pub funder_address: String,
}

async fn handle_setup(Json(payload): Json<SetupPayload>) -> axum::response::Response {
    use crate::config::{BotConfig, TargetsConfig};
    use std::io::Write;

    // 1. Write the genuine secrets to `.env`
    if let Ok(mut env_file) = std::fs::File::create(".env") {
        let _ = writeln!(env_file, "PRIVATE_KEY=\"{}\"", payload.private_key);
        let _ = writeln!(env_file, "FUNDER_ADDRESS=\"{}\"", payload.funder_address);
    }

    // 2. Initialize default config.toml (Target Wallets can be configured later in UI)
    let default_cfg = BotConfig {
        targets: TargetsConfig { wallets: vec![] },
        ..Default::default()
    };
    let _ = crate::config::write_toml(&default_cfg);

    // Force a semantic wait to ensure file flush
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Trigger seamless hot reboot after a short delay so the HTTP response returns cleanly
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let exe = std::env::current_exe().unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&exe).arg("--ui-reboot").exec();
            tracing::error!("Seamless setup reboot failed: {}", err);
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new(&exe).arg("--ui-reboot").spawn();
            std::process::exit(0);
        }
    });

    axum::Json(serde_json::json!({ "success": true })).into_response()
}

// ────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StateResponse {
    pub positions: HashMap<String, Position>,
    pub live_feed: Vec<EvaluatedTrade>,
    pub total_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub target_positions: Vec<TargetPosition>,
    pub copies_executed: u32,
    pub trades_skipped: u32,
    pub copied_count: usize,
    pub next_scan_secs: u64,
    pub pending_orders: std::collections::HashMap<String, crate::models::QueuedOrder>,
    pub active_orders: Vec<crate::models::ActiveApiOrder>,
    pub position_sources: HashMap<String, String>,
}

async fn get_state(State(api_state): State<ApiState>) -> Json<StateResponse> {
    let guard = api_state.bot_state.read().await;
    let ledger = api_state.copy_ledger.lock().await;

    let mut position_sources = HashMap::new();
    // Map live holdings
    for token_id in guard.positions.keys() {
        if let Some(entry) = ledger.find_active_for_token(token_id) {
            position_sources.insert(token_id.clone(), entry.source_wallet.clone());
        } else if let Some(entry) = ledger
            .entries
            .iter()
            .rev()
            .find(|e| e.token_id == *token_id)
        {
            // Fallback for positions currently in the process of closing
            // (Limit SELL pending but still held in balance)
            position_sources.insert(token_id.clone(), entry.source_wallet.clone());
        }
    }
    // Map active API limit orders (pending opens / pending closes)
    for o in &guard.active_orders {
        if let Some(entry) = ledger.find_active_for_token(&o.token_id) {
            position_sources.insert(o.token_id.clone(), entry.source_wallet.clone());
        } else if let Some(entry) = ledger
            .entries
            .iter()
            .rev()
            .find(|e| e.token_id == o.token_id)
        {
            position_sources.insert(o.token_id.clone(), entry.source_wallet.clone());
        }
    }

    // Convert current state to serializable DTO, stripping Instants
    let response = StateResponse {
        positions: guard.positions.clone(),
        live_feed: guard.live_feed.iter().cloned().collect(),
        total_balance: guard.total_balance,
        unrealized_pnl: guard.unrealized_pnl,
        realized_pnl: guard.realized_pnl,
        target_positions: guard.target_positions.clone(),
        copies_executed: guard.copies_executed,
        trades_skipped: guard.trades_skipped,
        copied_count: guard.copied_count,
        next_scan_secs: guard.next_scan_secs,
        pending_orders: guard.pending_orders.clone(),
        active_orders: guard.active_orders.clone(),
        position_sources,
    };

    Json(response)
}

use serde::Deserialize;

#[derive(Serialize, Deserialize)]
pub struct EnvData {
    pub private_key: String,
    pub funder_address: String,
}

async fn get_config() -> Json<serde_json::Value> {
    let raw = std::fs::read_to_string("config.toml").unwrap_or_default();
    let toml_val: toml::Value = raw
        .parse()
        .unwrap_or(toml::Value::Table(toml::map::Map::new()));
    let json_val = serde_json::to_value(toml_val).unwrap();
    Json(json_val)
}

async fn post_config(Json(payload): Json<BotConfig>) -> Json<serde_json::Value> {
    if let Err(e) = crate::config::write_toml(&payload) {
        return Json(serde_json::json!({ "error": e.to_string() }));
    }
    Json(serde_json::json!({ "success": true }))
}

async fn get_env() -> Json<EnvData> {
    let _ = dotenvy::dotenv_override();
    let private_key = std::env::var("PRIVATE_KEY").unwrap_or_default();
    let funder_address = std::env::var("FUNDER_ADDRESS").unwrap_or_default();
    Json(EnvData {
        private_key,
        funder_address,
    })
}

async fn post_env(Json(payload): Json<EnvData>) -> Json<serde_json::Value> {
    if let Err(e) = crate::config::write_secrets_env(&payload.private_key, &payload.funder_address)
    {
        return Json(serde_json::json!({ "error": e.to_string() }));
    }
    Json(serde_json::json!({ "success": true }))
}

async fn restart() -> Json<serde_json::Value> {
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Execute seamless process replacement (hot reboot in the same terminal)
        let exe = std::env::current_exe().unwrap_or_else(|_| "cargo".into());
        let args: Vec<String> = std::env::args().collect();

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            tracing::warn!("Executing seamless API reboot...");
            let err = std::process::Command::new(&exe).args(&args[1..]).exec();
            tracing::error!("Seamless API reboot failed: {}", err);
            std::process::exit(1);
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new(&exe).args(&args[1..]).spawn();
            std::process::exit(0);
        }
    });
    Json(serde_json::json!({ "success": true }))
}

pub fn create_router(
    bot_state: Arc<RwLock<BotState>>,
    copy_ledger: Arc<tokio::sync::Mutex<crate::copy_ledger::CopyLedger>>,
) -> Router {
    use tower_http::cors::{Any, CorsLayer};
    use tower_http::services::{ServeDir, ServeFile};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let state = ApiState {
        bot_state,
        copy_ledger,
    };

    let root_path = std::env::current_dir().unwrap().join("web/dist");
    let serve_dir =
        ServeDir::new(&root_path).fallback(ServeFile::new(root_path.join("index.html")));

    Router::new()
        .route("/api/state", get(get_state))
        .route("/api/config", get(get_config).post(post_config))
        .route("/api/env", get(get_env).post(post_env))
        .route("/api/action/restart", post(restart))
        .with_state(state)
        .layer(cors)
        .fallback_service(serve_dir)
}
