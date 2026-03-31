use crate::config::Config;
use crate::log_capture::LogBuffer;
use crate::models::{EvaluatedTrade, Position, TargetPosition, TradeSide};
use crate::state::BotState;
use crate::utils;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
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

// -- Exit signal ---------------------------------------------------------------
/// Returned by start_tui to tell main what to do next.
pub enum TuiExit {
    /// User pressed [q] -- shut down.
    Quit,
    /// User pressed [s] -- run settings wizard then restart.
    Settings,
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

    let exit = loop {
        // -- Snapshot state (brief read lock) ----------------------------------
        let snap = {
            let g = state.read().await;

            // Grab the last LOG_PANEL_LINES log entries from the buffer
            let logs = {
                if let Ok(buf) = log_buffer.try_lock() {
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
                }
            };

            Snap {
                balance: g.total_balance,
                realized_pnl: g.realized_pnl,
                unrealized_pnl: g.unrealized_pnl,
                feed: g.live_feed.iter().take(20).cloned().collect(),
                positions: g.positions.values().cloned().collect(),
                target_positions: g.target_positions.clone(),
                target_portfolio_est: if config.sizing_mode == crate::models::SizingMode::TargetPct
                {
                    Some(g.target_portfolio_usd)
                } else {
                    None
                },
                // Authoritative count from the dedicated API task (main.rs).
                // Queries our wallet and each target wallet every 30s.
                copied_count: g.copied_count,
                skips: g.trades_skipped,
                logs,
            }
        };

        terminal.draw(|f| render(f, &snap, &config))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => break TuiExit::Quit,
                    KeyCode::Char('s') | KeyCode::Char('S') => break TuiExit::Settings,
                    _ => {}
                }
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
            Span::styled("   |   refreshes every 60s", label_style),
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
            let market = format!("{} ({})", pos.title, pos.outcome);
            let market_trunc = if market.len() > 44 {
                format!("{}...", &market[..44])
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

    let title = if let Some(portfolio) = snap.target_portfolio_est {
        format!(
            " [S] Opportunity Scanner  -- {} watching / {} tracked | Est. target portfolio: ${:.0} ",
            watch, total, portfolio
        )
    } else {
        format!(
            " [S] Opportunity Scanner  -- {} watching / {} tracked ",
            watch, total
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
        Line::from(Span::styled(
            "  [s] Edit settings   (re-runs wizard, restarts bot)",
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
