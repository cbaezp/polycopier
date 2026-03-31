use crate::models::{EvaluatedTrade, Position, TargetPosition};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};

pub struct BotState {
    pub positions: HashMap<String, Position>,
    pub live_feed: VecDeque<EvaluatedTrade>,
    pub total_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub started: bool,
    pub target_positions: Vec<TargetPosition>,
    pub copies_executed: u32,
    pub trades_skipped: u32,
    /// Estimated total invested capital of the target wallet(s).
    /// Computed each scan cycle as Σ(avg_price × size) across all target open positions.
    /// Used by `SizingMode::TargetPct` to compute proportional order sizes.
    pub target_portfolio_usd: Decimal,
}

impl BotState {
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
            live_feed: VecDeque::with_capacity(100),
            total_balance: Decimal::from(0),
            unrealized_pnl: Decimal::from(0),
            realized_pnl: Decimal::from(0),
            started: false,
            target_positions: Vec::new(),
            copies_executed: 0,
            trades_skipped: 0,
            target_portfolio_usd: Decimal::ZERO,
        }
    }

    pub fn push_evaluated_trade(&mut self, trade: EvaluatedTrade) {
        if trade.validated {
            self.copies_executed += 1;
        } else {
            self.trades_skipped += 1;
        }
        if self.live_feed.len() == 100 {
            self.live_feed.pop_back();
        }
        self.live_feed.push_front(trade);
    }
}

impl Default for BotState {
    fn default() -> Self {
        Self::new()
    }
}
