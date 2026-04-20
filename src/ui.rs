use crate::config::Config;
use crate::log_capture::LogBuffer;

use crate::models::{EvaluatedTrade, Position, TargetPosition, TradeSide};
use crate::state::BotState;
use crate::utils;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, List, ListItem, Paragraph, Row, Table},
    Frame, Terminal,
};
use rust_decimal::Decimal;
use std::io;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Helper: read a dotted TOML key from config.toml into a display string
// Used by SettingsField::new to pre-populate the settings editor.
// ---------------------------------------------------------------------------
fn read_toml_field(dotted_key: &str) -> Option<String> {
    let raw = std::fs::read_to_string("config.toml").ok()?;
    let table: toml::Table = raw.parse().ok()?;
    let (section, key) = dotted_key.split_once('.')?;
    let val = table.get(section)?.get(key)?;
    // Render the TOML value as a plain string (strip quotes for strings).
    // For arrays (e.g. targets.wallets), render as comma-separated.
    let s = match val {
        toml::Value::String(s) => s.clone(),
        toml::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    };
    Some(s)
}

// -- Exit signal ---------------------------------------------------------------
/// Returned by start_tui to tell main what to do next.
pub enum TuiExit {
    /// User pressed [q] -- shut down.
    Quit,
    /// Settings were saved inside the TUI. Main should execv to restart.
    Settings,
}

// -- In-TUI settings editor ---------------------------------------------------
pub struct SettingsField {
    pub label: &'static str,
    pub env_key: &'static str,
    pub value: String,
    pub original: String,
    pub hint: &'static str,
    pub secret: bool,
}

impl SettingsField {
    pub fn new(
        label: &'static str,
        env_key: &'static str,
        default: &'static str,
        hint: &'static str,
        secret: bool,
    ) -> Self {
        // env_key is now a dotted TOML path like "execution.max_slippage_pct".
        // Read the current value from config.toml (via the BotConfig struct) so the
        // settings editor shows real values, not env-var defaults.
        let value = if env_key.starts_with("env.") {
            std::env::var(env_key.trim_start_matches("env."))
                .unwrap_or_else(|_| default.to_string())
        } else {
            read_toml_field(env_key).unwrap_or_else(|| default.to_string())
        };
        Self {
            label,
            env_key,
            value: value.clone(),
            original: value,
            hint,
            secret,
        }
    }
    pub fn is_changed(&self) -> bool {
        self.value != self.original
    }
    pub fn display(&self, editing: bool, buf: &str) -> String {
        if editing {
            format!("{}_", buf)
        } else if self.secret && !self.value.is_empty() {
            "[hidden]".to_string()
        } else {
            self.value.clone()
        }
    }
}

pub struct SettingsScreen {
    pub fields: Vec<SettingsField>,
    pub selected: usize,
    pub editing: bool,
    pub edit_buf: String,
    pub status: String,
}

pub enum SettingsExit {
    Back,
    SaveAndRestart,
}

impl Default for SettingsScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsScreen {
    pub fn new() -> Self {
        let fields = vec![
            SettingsField::new(
                "Private Key",
                "env.PRIVATE_KEY",
                "",
                "EVM Private Key (Requires restart to take effect)",
                true,
            ),
            SettingsField::new(
                "Funder Address",
                "env.FUNDER_ADDRESS",
                "",
                "Public address corresponding to your bot's wallet",
                false,
            ),
            SettingsField::new(
                "Target Wallets",
                "targets.wallets",
                "0xabc:1.0, 0xdef:0.01",
                "Proxy addresses to copy. Append ':scalar' to fractional scale (e.g. 0xabc:0.01)",
                false,
            ),
            SettingsField::new(
                "Sizing Mode",
                "sizing.mode",
                "self_pct",
                "fixed | self_pct | target_usd",
                false,
            ),
            SettingsField::new(
                "Copy Size %",
                "sizing.copy_size_pct",
                "",
                "Only used for self_pct mode  (0.15 = 15%)",
                false,
            ),
            SettingsField::new(
                "Max Trade Size (USD)",
                "execution.max_trade_size_usd",
                "10.00",
                "Hard ceiling per copied trade regardless of sizing mode",
                false,
            ),
            SettingsField::new(
                "Max Slippage",
                "execution.max_slippage_pct",
                "0.02",
                "Price deviation allowed from copied trade  (0.02 = 2%)",
                false,
            ),
            SettingsField::new(
                "Max Event Age (secs)",
                "execution.max_delay_seconds",
                "10",
                "Drop live events older than N seconds",
                false,
            ),
            SettingsField::new(
                "Ignore Closing (mins)",
                "execution.ignore_closing_in_mins",
                "15",
                "Skip entering/holding markets closing in <X mins. Leave blank to disable.",
                false,
            ),
            SettingsField::new(
                "Sell Fee Buffer",
                "execution.sell_fee_buffer",
                "0.97",
                "SELL size = held x buffer (0.97 absorbs ~3% CLOB fee)",
                false,
            ),
            SettingsField::new(
                "Max Copy Loss",
                "scanner.max_copy_loss_pct",
                "0.40",
                "Skip catch-up if target already this % underwater  (0.40 = 40%)",
                false,
            ),
            SettingsField::new(
                "Max Copy Gain",
                "scanner.max_copy_gain_pct",
                "0.05",
                "Skip catch-up if target already this % in profit  (0.05 = 5%)",
                false,
            ),
            SettingsField::new(
                "Min Entry Price",
                "scanner.min_entry_price",
                "0.02",
                "Minimum token price for catch-up entries",
                false,
            ),
            SettingsField::new(
                "Max Entry Price",
                "scanner.max_entry_price",
                "0.999",
                "Maximum token price for catch-up entries",
                false,
            ),
            SettingsField::new(
                "Scan Max/Cycle",
                "scanner.max_entries_per_cycle",
                "1",
                "Max positions queued per scan cycle  (1 = conservative)",
                false,
            ),
            SettingsField::new(
                "Min Entry Amount (USD)",
                "scanner.min_amount",
                "0",
                "Minimum target position USD value limit",
                false,
            ),
            SettingsField::new(
                "Max Entry Amount (USD)",
                "scanner.max_amount",
                "9999999999",
                "Maximum target position USD value limit",
                false,
            ),
            SettingsField::new(
                "Daily Volume Limit",
                "risk.max_daily_volume_usd",
                "0",
                "Max USD traded per UTC day. 0 = disabled.",
                false,
            ),
            SettingsField::new(
                "Max Consec Losses",
                "risk.max_consecutive_losses",
                "0",
                "Losses before cooldown pause. 0 = disabled.",
                false,
            ),
            SettingsField::new(
                "Loss Cooldown (secs)",
                "risk.loss_cooldown_secs",
                "300",
                "Seconds to pause after hitting max consecutive losses",
                false,
            ),
            SettingsField::new(
                "Ledger Retention (days)",
                "ledger.retention_days",
                "90",
                "Days to keep closed ledger entries. 0 = never prune.",
                false,
            ),
        ];
        Self {
            fields,
            selected: 0,
            editing: false,
            edit_buf: String::new(),
            status: "  Arrow keys: navigate   Enter: edit   [s]: Save & Restart   [q]: Back without saving".into(),
        }
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> Option<SettingsExit> {
        if self.editing {
            match k.code {
                KeyCode::Esc => {
                    self.editing = false;
                    self.status = format!(
                        "  Cancelled -- '{}' unchanged",
                        self.fields[self.selected].label
                    );
                }
                KeyCode::Enter => {
                    let label = self.fields[self.selected].label;
                    self.fields[self.selected].value = self.edit_buf.trim().to_string();
                    self.editing = false;
                    self.status =
                        format!("  '{}' updated -- [s] to Save & Restart  [q] Back", label);
                }
                KeyCode::Backspace => {
                    self.edit_buf.pop();
                }
                KeyCode::Char(c) => {
                    self.edit_buf.push(c);
                }
                _ => {}
            }
        } else {
            match k.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    self.status = format!("  {}   |   Arrow keys: navigate   Enter: edit   [s]: Save & Restart   [q]: Back",
                        self.fields[self.selected].hint);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(self.fields.len() - 1);
                    self.status = format!("  {}   |   Arrow keys: navigate   Enter: edit   [s]: Save & Restart   [q]: Back",
                        self.fields[self.selected].hint);
                }
                KeyCode::Enter => {
                    self.edit_buf = self.fields[self.selected].value.clone();
                    self.editing = true;
                    self.status = format!(
                        "  Editing '{}'  -- Enter to confirm  Esc to cancel",
                        self.fields[self.selected].label
                    );
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    return Some(SettingsExit::SaveAndRestart);
                }
                KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                    return Some(SettingsExit::Back);
                }
                _ => {}
            }
        }
        None
    }

    pub fn save_to_dotenv(&self) -> anyhow::Result<()> {
        self.save_to_path(std::path::Path::new("config.toml"))
    }

    /// Testable variant -- writes tunables to an arbitrary TOML path; also
    /// refreshes `.env` with secrets-only entries.
    pub fn save_to_path(&self, path: &std::path::Path) -> anyhow::Result<()> {
        // Build a BotConfig from the current field values
        let get = |key: &str| -> String {
            self.fields
                .iter()
                .find(|f| f.env_key == key)
                .map(|f| f.value.clone())
                .unwrap_or_default()
        };
        let dec = |key: &str, fallback: &str| -> rust_decimal::Decimal {
            get(key)
                .parse()
                .unwrap_or_else(|_| fallback.parse().unwrap())
        };
        let u32v = |key: &str, fallback: u32| -> u32 { get(key).parse().unwrap_or(fallback) };
        let u64v = |key: &str, fallback: u64| -> u64 { get(key).parse().unwrap_or(fallback) };
        let i64v = |key: &str, fallback: i64| -> i64 { get(key).parse().unwrap_or(fallback) };
        let usizev =
            |key: &str, fallback: usize| -> usize { get(key).parse().unwrap_or(fallback).max(1) };

        let copy_size_pct: Option<rust_decimal::Decimal> = get("sizing.copy_size_pct")
            .parse()
            .ok()
            .filter(|&p: &rust_decimal::Decimal| {
                p > rust_decimal::Decimal::ZERO && p <= rust_decimal::Decimal::ONE
            });

        // Parse target wallets from the comma-separated TUI field
        let wallets: Vec<String> = get("targets.wallets")
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let wallets_toml = if wallets.is_empty() {
            "[]".to_string()
        } else {
            let q: Vec<String> = wallets.iter().map(|w| format!("\"{w}\"")).collect();
            format!("[{}]", q.join(", "))
        };

        let copy_size_line = match copy_size_pct {
            Some(p) => format!("copy_size_pct = {p}"),
            None => "# copy_size_pct = 0.15  # only used for self_pct mode".to_string(),
        };

        // Build TOML manually (same format as config.rs write_toml).
        let content = format!(
            "# polycopier config -- safe to version control (no secrets here)\n             # Secrets (PRIVATE_KEY, FUNDER_ADDRESS) stay in .env\n             \n             [targets]\n             wallets = {wallets}\n             \n             [execution]\n             max_slippage_pct = {slippage}\n             max_trade_size_usd = {max_trade}\n             max_delay_seconds = {delay}\n             {ignore_closing}\n             sell_fee_buffer = {fee_buf}\n             \n             [sizing]\n             mode = \"{mode}\"\n             {copy_size_line}\n             \n             [scanner]\n             max_copy_loss_pct = {loss_pct}\n             max_copy_gain_pct = {gain_pct}\n             min_entry_price = {min_price}\n             max_entry_price = {max_price}\n             max_entries_per_cycle = {max_entries}\n             {min_amount_line}\n             {max_amount_line}\n             \n             [risk]\n             max_daily_volume_usd = {daily_vol}\n             max_consecutive_losses = {consec_loss}\n             loss_cooldown_secs = {cooldown}\n             \n             [ledger]\n             retention_days = {retention}\n",
            wallets = wallets_toml,
            slippage = dec("execution.max_slippage_pct", "0.02"),
            max_trade = dec("execution.max_trade_size_usd", "10.00"),
            delay = i64v("execution.max_delay_seconds", 10),
            ignore_closing = {
                let s = get("execution.ignore_closing_in_mins");
                if s.trim().is_empty() {
                    "# ignore_closing_in_mins = disabled".to_string()
                } else if let Ok(m) = s.parse::<u64>() {
                    format!("ignore_closing_in_mins = {}", m)
                } else {
                    "# invalid ignore_closing_in_mins".to_string()
                }
            },
            fee_buf = dec("execution.sell_fee_buffer", "0.97"),
            mode = get("sizing.mode"),
            copy_size_line = copy_size_line,
            loss_pct = dec("scanner.max_copy_loss_pct", "0.40"),
            gain_pct = dec("scanner.max_copy_gain_pct", "0.05"),
            min_price = dec("scanner.min_entry_price", "0.02"),
            max_price = dec("scanner.max_entry_price", "0.999"),
            max_entries = usizev("scanner.max_entries_per_cycle", 1),
            min_amount_line = {
                let s = get("scanner.min_amount");
                if s.trim().is_empty() { "# min_amount = 0  # optional, filters positions with size < X".to_string() } else { format!("min_amount = {}", s) }
            },
            max_amount_line = {
                let s = get("scanner.max_amount");
                if s.trim().is_empty() { "# max_amount = 9999999999  # optional, filters positions with size > X".to_string() } else { format!("max_amount = {}", s) }
            },
            daily_vol = dec("risk.max_daily_volume_usd", "0"),
            consec_loss = u32v("risk.max_consecutive_losses", 0),
            cooldown = u64v("risk.loss_cooldown_secs", 300),
            retention = u32v("ledger.retention_days", 90),
        );
        std::fs::write(path, content)?;

        // Refresh .env with secrets only (TARGET_WALLETS is now in config.toml)
        let pk = get("env.PRIVATE_KEY");
        let fa = get("env.FUNDER_ADDRESS");
        if !pk.is_empty() {
            let _ = crate::config::write_secrets_env(&pk, &fa);
        }
        Ok(())
    }

    pub fn has_changes(&self) -> bool {
        self.fields.iter().any(|f| f.is_changed())
    }
}

// -- Snapshot cloned out of the RwLock each frame ------------------------------
struct Snap {
    balance: Decimal,
    realized_pnl: Decimal,
    unrealized_pnl: Decimal,
    feed: Vec<EvaluatedTrade>,
    positions: Vec<Position>,
    target_positions: Vec<TargetPosition>,
    target_portfolio_est: Option<Decimal>,
    /// Positions WE hold that the target ALSO holds (HELD status).
    copied_count: usize,
    skips: u32,
    /// Last N log entries for the log panel (newest last).
    logs: Vec<(String, String, String)>, // (timestamp, level, message)
    /// Seconds since the scanner last completed a full cycle (None = not run yet).
    last_scan_secs_ago: Option<u64>,
    /// Scheduled seconds until the NEXT scan.
    next_scan_secs: u64,
    /// Seconds since the price-refresh task last updated cur_price (None = not run yet).
    last_price_refresh_secs_ago: Option<u64>,
    /// Seconds since the order watcher last completed a successful cycle (None = not run yet).
    last_watcher_secs_ago: Option<u64>,
}

fn shorten(addr: &str) -> String {
    if addr.len() > 13 {
        format!("{}..{}", &addr[..6], &addr[addr.len() - 4..])
    } else {
        addr.to_string()
    }
}

fn pnl_color(v: Decimal) -> Color {
    if v > Decimal::ZERO {
        Color::Green
    } else if v < Decimal::ZERO {
        Color::Red
    } else {
        Color::Gray
    }
}

// -- Number of log lines visible in the log panel ------------------------------
const LOG_PANEL_LINES: usize = 5;

// ------------------------------------------------------------------------------
pub async fn start_tui(
    state: Arc<RwLock<BotState>>,
    config: Config,
    log_buffer: LogBuffer,
) -> Result<TuiExit> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // -- Screen state: either the live dashboard or the in-TUI settings editor --
    enum Screen {
        Dashboard,
        Settings(SettingsScreen),
    }
    let mut screen = Screen::Dashboard;

    let exit = loop {
        // -- Handle events first (needs &mut screen) ---------------------------
        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                match &mut screen {
                    Screen::Dashboard => match k.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => break TuiExit::Quit,
                        KeyCode::Char('s') | KeyCode::Char('S') => {
                            screen = Screen::Settings(SettingsScreen::new());
                        }
                        _ => {}
                    },
                    Screen::Settings(s) => match s.handle_key(k) {
                        Some(SettingsExit::Back) => {
                            screen = Screen::Dashboard;
                        }
                        Some(SettingsExit::SaveAndRestart) => match s.save_to_dotenv() {
                            Ok(()) => break TuiExit::Settings,
                            Err(e) => {
                                s.status = format!("  Save failed: {}  -- press [q] to go back", e);
                            }
                        },
                        None => {}
                    },
                }
            }
        }

        // -- Draw (needs &screen immutably) ------------------------------------
        match &screen {
            Screen::Dashboard => {
                let snap = {
                    let g = state.read().await;
                    let logs = if let Ok(buf) = log_buffer.try_lock() {
                        buf.iter()
                            .rev()
                            .take(LOG_PANEL_LINES)
                            .map(|e| (e.timestamp.clone(), e.level.clone(), e.message.clone()))
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect()
                    } else {
                        vec![]
                    };
                    Snap {
                        balance: g.total_balance,
                        realized_pnl: g.realized_pnl,
                        unrealized_pnl: g.unrealized_pnl,
                        feed: g.live_feed.iter().take(20).cloned().collect(),
                        positions: g.positions.values().cloned().collect(),
                        target_positions: g.target_positions.clone(),
                        target_portfolio_est: None,
                        copied_count: g.copied_count,
                        skips: g.trades_skipped,
                        logs,
                        last_scan_secs_ago: g.last_scan_at.map(|t| t.elapsed().as_secs()),
                        next_scan_secs: g.next_scan_secs,
                        last_price_refresh_secs_ago: g
                            .last_price_refresh_at
                            .map(|t| t.elapsed().as_secs()),
                        last_watcher_secs_ago: g.last_watcher_run_at.map(|t| t.elapsed().as_secs()),
                    }
                };
                terminal.draw(|f| render(f, &snap, &config))?;
            }
            Screen::Settings(s) => {
                terminal.draw(|f| render_settings_editor(f, s))?;
            }
        }
    };

    // -- Clean up terminal -----------------------------------------------------
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(exit)
}

// ------------------------------------------------------------------------------
fn render(f: &mut Frame, snap: &Snap, config: &Config) {
    let area = f.size();

    // Outer vertical split:  header | body | logs | footer
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),                          // header
            Constraint::Min(0),                             // body (scanner + feed)
            Constraint::Length(LOG_PANEL_LINES as u16 + 2), // log panel (+ 2 for border)
            Constraint::Length(1),                          // footer
        ])
        .split(area);

    render_header(f, snap, config, outer[0]);
    render_body(f, snap, config, outer[1]);
    render_logs(f, &snap.logs, outer[2]);
    render_footer(f, outer[3]);
}

// -- Header --------------------------------------------------------------------
fn render_header(f: &mut Frame, snap: &Snap, config: &Config, area: ratatui::layout::Rect) {
    let title_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let live_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::DarkGray);
    let val_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let pos_pnl = pnl_color(snap.realized_pnl + snap.unrealized_pnl);

    let wallet_short = shorten(&config.funder_address);
    let targets_short = config
        .target_wallets
        .iter()
        .map(|w| shorten(w))
        .collect::<Vec<_>>()
        .join(", ");

    let watch_count = snap
        .target_positions
        .iter()
        .filter(|p| p.status == crate::models::ScanStatus::Monitoring)
        .count();
    let total_scanned = snap.target_positions.len();

    let lines = vec![
        Line::from(vec![
            Span::styled("  [*] POLYCOPIER ", title_style),
            Span::styled(
                "- Automated Copy Trading Engine  ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("* LIVE", live_style),
        ]),
        Line::from(vec![
            Span::styled("  Wallet: ", label_style),
            Span::styled(wallet_short, val_style),
            Span::styled("   |   Target(s): ", label_style),
            Span::styled(&targets_short as &str, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("  Balance: ", label_style),
            Span::styled(
                format!("${:.2}", snap.balance),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   |   Realized: ", label_style),
            Span::styled(
                format!("${:.2}", snap.realized_pnl),
                Style::default().fg(pos_pnl),
            ),
            Span::styled("   |   Unrealized: ", label_style),
            Span::styled(
                format!("${:.2}", snap.unrealized_pnl),
                Style::default().fg(pnl_color(snap.unrealized_pnl)),
            ),
            Span::styled("   |   Copied: ", label_style),
            Span::styled(
                format!("{}", snap.copied_count),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   Skipped: ", label_style),
            Span::styled(
                format!("{}", snap.skips),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Scanner: ", label_style),
            Span::styled(
                format!("{} positions tracked", total_scanned),
                Style::default().fg(Color::White),
            ),
            Span::styled("   |   ", label_style),
            Span::styled(
                format!("{} entry opportunities", watch_count),
                Style::default().fg(Color::Green),
            ),
            Span::styled("   |   ", label_style),
            Span::styled(
                match snap.last_watcher_secs_ago {
                    None => "Order Watcher: starting...".to_string(),
                    Some(s) => format!("Order Watcher: {}s ago", s),
                },
                Style::default().fg(Color::Magenta),
            ),
        ]),
    ];

    let header = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(header, area);
}

// -- Body: top row (feed + scanner) then full-width positions below ------------
fn render_body(f: &mut Frame, snap: &Snap, config: &Config, area: ratatui::layout::Rect) {
    // Vertical: top (feed + scanner + settings) | bottom (our positions)
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    // Top row: left = feed, right = scanner (top 70%) + settings (bottom 30%)
    let top_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(rows[0]);

    render_live_feed(f, snap, top_cols[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(top_cols[1]);

    render_scanner(f, snap, right[0]);
    render_settings(f, config, right[1]);

    // Bottom row: full-width positions table with entry quality analysis
    render_our_positions(f, snap, rows[1]);
}

// -- Live Feed ----------------------------------------------------------------
fn render_live_feed(f: &mut Frame, snap: &Snap, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = snap
        .feed
        .iter()
        .map(|ev| {
            let (icon, style) = if ev.validated {
                ("+", Style::default().fg(Color::Green))
            } else {
                ("-", Style::default().fg(Color::DarkGray))
            };
            let side = match ev.original_event.side {
                TradeSide::BUY => "BUY ",
                TradeSide::SELL => "SELL",
            };
            let ts = utils::format_timestamp(ev.original_event.timestamp);
            let reason = ev.reason.as_deref().unwrap_or("");
            let text = format!(
                "{} {} {}  ${:.3}  {:.2}sh  {}",
                icon,
                side,
                &ev.original_event.token_id[..ev.original_event.token_id.len().min(10)],
                ev.original_event.price,
                ev.original_event.size,
                if ev.validated { ts } else { reason.to_string() },
            );
            ListItem::new(Line::from(Span::styled(text, style)))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(Span::styled(
                " [~] Live Copy Feed ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Blue)),
    );
    f.render_widget(list, area);
}

// -- Our Positions (open + copied, with entry quality analysis) ----------------
//
// Only positions that are open in BOTH our wallet AND a target wallet are shown.
// Positions we hold that the target has already closed are omitted.
//
// Columns:
//   SOURCE       - shortened target wallet the position was copied from
//   TOKEN        - first 12 chars of token_id
//   SZ           - our share size
//   OUR ENTRY    - our average_entry_price
//   TGT ENTRY    - target's avg_price at their entry
//   DELTA%       - (our_entry - tgt_entry) / tgt_entry
//   CUR PRICE    - live market price (from target_positions)
//   OUR PNL%     - (cur_price - our_entry) / our_entry
//
// Row color: green (<= +5%), yellow (+5-15%), red (>+15% -- chased)
fn render_our_positions(f: &mut Frame, snap: &Snap, area: ratatui::layout::Rect) {
    use std::collections::HashMap;

    // token_id -> TargetPosition for cross-reference
    let target_map: HashMap<&str, &crate::models::TargetPosition> = snap
        .target_positions
        .iter()
        .map(|tp| (tp.token_id.as_str(), tp))
        .collect();

    let header = Row::new(vec![
        Cell::from("SOURCE").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("TOKEN").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("SZ").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("OUR ENTRY").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("TGT ENTRY").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("DELTA%").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("CUR PRICE").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("OUR PNL%").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::UNDERLINED));

    let hundred = Decimal::from(100);
    let five = Decimal::from(5);
    let fifteen = Decimal::from(15);

    // Only show positions that are currently open in a target wallet too
    let rows: Vec<Row> = snap
        .positions
        .iter()
        .filter_map(|p| target_map.get(p.token_id.as_str()).map(|tp| (p, *tp)))
        .map(|(p, tp)| {
            let token_short = &p.token_id[..p.token_id.len().min(12)];
            let wallet_short = shorten(&tp.source_wallet);

            let delta_pct = if tp.avg_price > Decimal::ZERO {
                (p.average_entry_price - tp.avg_price) / tp.avg_price * hundred
            } else {
                Decimal::ZERO
            };

            let our_pnl_pct = if p.average_entry_price > Decimal::ZERO {
                (tp.cur_price - p.average_entry_price) / p.average_entry_price * hundred
            } else {
                Decimal::ZERO
            };

            let delta_color = if delta_pct <= five {
                Color::Green
            } else if delta_pct <= fifteen {
                Color::Yellow
            } else {
                Color::Red
            };

            let pnl_color = if our_pnl_pct >= Decimal::ZERO {
                Color::Green
            } else {
                Color::Red
            };

            Row::new(vec![
                Cell::from(wallet_short).style(Style::default().fg(Color::Yellow)),
                Cell::from(token_short.to_string()),
                Cell::from(format!("{:.2}", p.size)),
                Cell::from(format!("${:.3}", p.average_entry_price)),
                Cell::from(format!("${:.3}", tp.avg_price)),
                Cell::from(format!("{:+.1}%", delta_pct)).style(
                    Style::default()
                        .fg(delta_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(format!("${:.3}", tp.cur_price)),
                Cell::from(format!("{:+.1}%", our_pnl_pct)).style(Style::default().fg(pnl_color)),
            ])
            .style(Style::default().fg(Color::White))
        })
        .collect();

    let copied_count = rows.len();

    let table = Table::new(
        rows,
        &[
            Constraint::Length(13),  // source wallet
            Constraint::Min(13),     // token
            Constraint::Length(6),   // size
            Constraint::Length(10),  // our entry
            Constraint::Length(10),  // tgt entry
            Constraint::Length(8),   // delta%
            Constraint::Length(10),  // cur price
            Constraint::Length(9),   // our pnl%
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(Span::styled(
                format!(
                    " [P] Copied & Open ({})  -- green: good entry (<5%)   yellow: mild premium (5-15%)   red: chased (>15%) ",
                    copied_count
                ),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Blue)),
    );
    f.render_widget(table, area);
}

// -- Opportunity Scanner -------------------------------------------------------
fn render_scanner(f: &mut Frame, snap: &Snap, area: ratatui::layout::Rect) {
    let header = Row::new(vec![
        Cell::from("STATUS").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("MARKET / OUTCOME").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("PRICE").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("PNL%").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("TARGET SZ").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::UNDERLINED));

    let rows: Vec<Row> = snap
        .target_positions
        .iter()
        .map(|pos| {
            let pnl_pct = pos.percent_pnl * Decimal::from(100);
            let pnl_str = format!("{:+.1}%", pnl_pct);
            let mut market = format!("{} ({})", pos.title, pos.outcome);
            if pos.status == crate::models::ScanStatus::Monitoring {
                if let Some(reason) = &pos.engine_reason {
                    market = format!("{}  << {} >>", market, reason);
                }
            }
            let market_trunc = if market.chars().count() > 75 {
                format!("{}...", market.chars().take(75).collect::<String>())
            } else {
                market.clone()
            };

            let row_style = Style::default().fg(pos.status.color());
            Row::new(vec![
                Cell::from(pos.status.label()),
                Cell::from(market_trunc),
                Cell::from(format!("${:.3}", pos.cur_price)),
                Cell::from(pnl_str),
                Cell::from(format!("{:.1}", pos.size)),
            ])
            .style(row_style)
        })
        .collect();

    let watch = snap
        .target_positions
        .iter()
        .filter(|p| p.status == crate::models::ScanStatus::Monitoring)
        .count();
    let total = snap.target_positions.len();

    let scan_timing = {
        let ago = match snap.last_scan_secs_ago {
            Some(s) => format!("{}s ago", s),
            None => "pending".to_string(),
        };
        let next = if snap.next_scan_secs > 0 {
            format!("next: {}s", snap.next_scan_secs)
        } else {
            "next: --".to_string()
        };
        let price_refresh = match snap.last_price_refresh_secs_ago {
            Some(s) => format!("prices: {}s ago", s),
            None => "prices: pending".to_string(),
        };
        format!("scan: {}  {}  {}", ago, next, price_refresh)
    };

    let title = if let Some(portfolio) = snap.target_portfolio_est {
        format!(
            " [S] Opportunity Scanner  {} | {} watching / {} tracked | portfolio: ${:.0} ",
            scan_timing, watch, total, portfolio
        )
    } else {
        format!(
            " [S] Opportunity Scanner  {} | {} watching / {} tracked ",
            scan_timing, watch, total
        )
    };

    let table = Table::new(
        rows,
        &[
            Constraint::Length(10),
            Constraint::Min(20),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    f.render_widget(table, area);
}

// -- Settings Summary ----------------------------------------------------------
fn render_settings(f: &mut Frame, config: &Config, area: ratatui::layout::Rect) {
    let label = Style::default().fg(Color::DarkGray);
    let val = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let sizing_str = config.sizing_mode.as_str().to_string();
    let sizing_detail = match &config.copy_size_pct {
        Some(pct) => format!(
            "{}  (COPY_SIZE_PCT: {:.0}%)",
            sizing_str,
            pct * rust_decimal::Decimal::from(100)
        ),
        None => sizing_str,
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("  Sizing:      ", label),
            Span::styled(sizing_detail, val),
        ]),
        Line::from(vec![
            Span::styled("  Max trade:   ", label),
            Span::styled(format!("${:.2}", config.max_trade_size_usd), val),
            Span::styled("   Slippage: ", label),
            Span::styled(
                format!(
                    "{:.1}%",
                    config.max_slippage_pct * rust_decimal::Decimal::from(100)
                ),
                val,
            ),
        ]),
        Line::from(vec![
            Span::styled("  Loss limit:  ", label),
            Span::styled(
                format!(
                    "{:.0}%",
                    config.max_copy_loss_pct * rust_decimal::Decimal::from(100)
                ),
                val,
            ),
            Span::styled("   Targets: ", label),
            Span::styled(format!("{}", config.target_wallets.len()), val),
        ]),
        Line::from(vec![
            Span::styled("  Risk:        ", label),
            Span::styled(
                if config.max_daily_volume_usd > rust_decimal::Decimal::ZERO {
                    format!("vol≤${:.0}/day", config.max_daily_volume_usd)
                } else {
                    "vol: unlimited".to_string()
                },
                val,
            ),
            Span::styled("   losses: ", label),
            Span::styled(
                if config.max_consecutive_losses == 0 {
                    "∞".to_string()
                } else {
                    format!(
                        "{} then {}s cooldown",
                        config.max_consecutive_losses, config.loss_cooldown_secs
                    )
                },
                Style::default()
                    .fg(if config.max_consecutive_losses == 0 {
                        Color::DarkGray
                    } else {
                        Color::Cyan
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            "  [s] Edit settings   (opens settings editor)",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let widget = Paragraph::new(lines).block(
        Block::default()
            .title(Span::styled(
                " Settings ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(widget, area);
}

// -- Log Panel -----------------------------------------------------------------
fn render_logs(f: &mut Frame, logs: &[(String, String, String)], area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = if logs.is_empty() {
        vec![ListItem::new(Span::styled(
            "  No warnings yet.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        logs.iter()
            .map(|(ts, level, msg)| {
                let level_color = if level == "WARN" {
                    Color::Yellow
                } else {
                    Color::Red
                };
                let line = Line::from(vec![
                    Span::styled(format!(" {} ", ts), Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("[{}] ", level),
                        Style::default()
                            .fg(level_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(msg.as_str(), Style::default().fg(Color::White)),
                ]);
                ListItem::new(line)
            })
            .collect()
    };

    let list = List::new(items).block(
        Block::default()
            .title(Span::styled(
                " System Logs (WARN+) ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    f.render_widget(list, area);
}

// -- Footer --------------------------------------------------------------------
fn render_footer(f: &mut Frame, area: ratatui::layout::Rect) {
    let footer = Paragraph::new(Line::from(vec![
        Span::styled("  [q] Quit", Style::default().fg(Color::Red)),
        Span::styled(
            "   [s] Settings",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "                    POLYCOPIER v0.1 - Powered by Polymarket SDK",
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .alignment(Alignment::Left);
    f.render_widget(footer, area);
}

// -- In-TUI settings editor screen --------------------------------------------
fn render_settings_editor(f: &mut Frame, screen: &SettingsScreen) {
    let area = f.size();

    // Outer layout: title block | fields table | status bar
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // fields
            Constraint::Length(3), // status + hints
        ])
        .split(area);

    // -- Outer border with title -----------------------------------------------
    let block = Block::default()
        .title(Span::styled(
            " [S] Settings   [Enter] Edit field   [s] Save & Restart   [q] Back ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));

    let inner_fields = block.inner(outer[0]);
    f.render_widget(block, outer[0]);

    // -- Fields table ----------------------------------------------------------
    let header = Row::new(vec![
        Cell::from("  Field").style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::UNDERLINED),
        ),
        Cell::from("Current Value").style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::UNDERLINED),
        ),
        Cell::from("").style(Style::default()), // changed marker column
    ])
    .height(1);

    let rows: Vec<Row> = screen
        .fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let is_sel = i == screen.selected;
            let is_edit = is_sel && screen.editing;

            let label = Cell::from(format!("  {}", field.label))
                .style(Style::default().fg(if is_sel { Color::Yellow } else { Color::White }));

            let val_str = field.display(is_edit, &screen.edit_buf);
            let val = Cell::from(format!(" {}", val_str)).style(if is_edit {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if is_sel {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            });

            let changed = Cell::from(if field.is_changed() { " *" } else { "" })
                .style(Style::default().fg(Color::Yellow));

            let row_style = if is_sel && !is_edit {
                Style::default().bg(Color::Rgb(40, 40, 60))
            } else {
                Style::default()
            };

            Row::new(vec![label, val, changed])
                .style(row_style)
                .height(1)
        })
        .collect();

    let table = Table::new(
        rows,
        &[
            Constraint::Length(26),
            Constraint::Min(30),
            Constraint::Length(3),
        ],
    )
    .header(header);

    f.render_widget(table, inner_fields);

    // -- Status / hint bar -----------------------------------------------------
    let dirty_note = if screen.has_changes() && !screen.editing {
        "  [*] Unsaved changes  -- press [s] to Save & Restart"
    } else {
        ""
    };

    let status_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::Yellow));

    let status_inner = status_block.inner(outer[1]);
    f.render_widget(status_block, outer[1]);

    let status_text = if dirty_note.is_empty() {
        screen.status.as_str()
    } else {
        dirty_note
    };

    f.render_widget(
        Paragraph::new(status_text).style(Style::default().fg(Color::Gray)),
        status_inner,
    );
}
