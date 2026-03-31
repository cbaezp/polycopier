use ratatui::style::Color;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Controls how order sizes are computed for each copied trade.
/// Set via the `SIZING_MODE` env var. Mutually exclusive -- only one is active at a time.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum SizingMode {
    /// Always spend exactly `MAX_TRADE_SIZE_USD` per trade.
    #[default]
    Fixed,
    /// Spend `COPY_SIZE_PCT` * our current balance (floored at CLOB $5 minimum,
    /// capped at `MAX_TRADE_SIZE_USD`).
    SelfPct,
    /// Mirror the target's exact dollar notional (`event.size * event.price`),
    /// capped at `MAX_TRADE_SIZE_USD`.
    TargetUsd,
}

impl SizingMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::SelfPct => "self_pct",
            Self::TargetUsd => "target_usd",
        }
    }

    pub fn from_mode_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "self_pct" => Self::SelfPct,
            "target_usd" => Self::TargetUsd,
            _ => Self::Fixed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeEvent {
    pub transaction_hash: String,
    pub maker_address: String,
    pub taker_address: String,
    pub token_id: String,
    pub price: Decimal,
    pub size: Decimal,
    pub side: TradeSide,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide {
    BUY,
    SELL,
}

#[derive(Debug, Clone)]
pub struct EvaluatedTrade {
    pub original_event: TradeEvent,
    pub validated: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub token_id: String,
    pub size: Decimal,
    pub average_entry_price: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub token_id: String,
    pub price: Decimal,
    pub size: Decimal,
    pub side: TradeSide,
}

// -- Opportunity Scanner types -------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanStatus {
    Monitoring,   // Valid entry candidate
    Entered,      // Order queued this session
    SkippedOwned, // We already hold this token
    SkippedLoss,  // Too far underwater
    SkippedPrice, // Price out of valid range
}

impl ScanStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Monitoring => "[W] WATCH",
            Self::Entered => "[Q] QUEUED",
            Self::SkippedOwned => "[H] HELD",
            Self::SkippedLoss => "[X] LOSS",
            Self::SkippedPrice => "[-] RANGE",
        }
    }
    pub fn color(&self) -> Color {
        match self {
            Self::Monitoring => Color::Green,
            Self::Entered => Color::Cyan,
            Self::SkippedOwned => Color::Magenta,
            Self::SkippedLoss => Color::Red,
            Self::SkippedPrice => Color::DarkGray,
        }
    }
    pub fn sort_key(&self) -> u8 {
        match self {
            Self::Monitoring => 0,
            Self::Entered => 1,
            Self::SkippedOwned => 2,
            Self::SkippedLoss => 3,
            Self::SkippedPrice => 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TargetPosition {
    pub title: String,
    pub outcome: String,
    pub token_id: String,
    pub cur_price: Decimal,
    pub avg_price: Decimal,
    pub percent_pnl: Decimal,
    pub size: Decimal,
    pub status: ScanStatus,
    /// The target wallet address this position was fetched from.
    pub source_wallet: String,
}
