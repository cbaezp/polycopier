use crate::models::{EvaluatedTrade, Position, QueuedOrder, TargetPosition};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

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

    /// Number of positions WE currently hold that the TARGET also holds.
    /// Set by a dedicated background task that queries both wallets via the API
    /// every 30 seconds -- never inferred from local scanner state.
    pub copied_count: usize,
    /// When the position scanner last completed a full cycle (wall clock).
    /// None until the first scan finishes.
    pub last_scan_at: Option<Instant>,
    /// How many seconds until the next scan is scheduled (set just before sleeping).
    pub next_scan_secs: u64,
    /// When target_positions.cur_price was last refreshed via the dedicated price
    /// refresh task (runs every 20s, independent of scanner urgency).
    pub last_price_refresh_at: Option<Instant>,
    /// Token IDs for which we have a live GTC order in the CLOB that has NOT
    /// yet been filled. Seeded from open CLOB orders at boot, updated by the
    /// strategy engine on submission and by the order watcher on cancellation.
    /// The scanner uses this alongside `positions` to prevent duplicate orders
    /// across bot restarts.
    pub pending_orders: HashMap<String, QueuedOrder>,
    /// When the order watcher last completed a cycle.
    pub last_watcher_run_at: Option<Instant>,
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
            copied_count: 0,
            last_scan_at: None,
            next_scan_secs: 0,
            last_price_refresh_at: None,
            pending_orders: HashMap::new(),
            last_watcher_run_at: None,
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
