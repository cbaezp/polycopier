use crate::config::BotConfig;
use crate::models::{EvaluatedTrade, Position, TargetPosition};
use crate::state::BotState;
use axum::{
    extract::{Json, State},
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
}

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
}

async fn get_state(State(api_state): State<ApiState>) -> Json<StateResponse> {
    let guard = api_state.bot_state.read().await;

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
    let _ = dotenvy::dotenv();
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
        // In typical deployments, systemd/docker restarts the process on exit.
        std::process::exit(0);
    });
    Json(serde_json::json!({ "success": true }))
}

pub fn create_router(bot_state: Arc<RwLock<BotState>>) -> Router {
    use tower_http::cors::{Any, CorsLayer};
    use tower_http::services::{ServeDir, ServeFile};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let state = ApiState { bot_state };

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
