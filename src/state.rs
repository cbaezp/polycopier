use std::collections::{HashMap, VecDeque};
use rust_decimal::Decimal;
use crate::models::{Position, EvaluatedTrade, TargetPosition};

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
