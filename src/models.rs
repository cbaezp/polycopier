use ratatui::style::Color;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

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

// ── Opportunity Scanner types ─────────────────────────────────────────────────

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
            Self::Monitoring => "◉ WATCH",
            Self::Entered => "⏺ QUEUED",
            Self::SkippedOwned => "◆ HELD",
            Self::SkippedLoss => "✘ LOSS",
            Self::SkippedPrice => "─ RANGE",
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
}
