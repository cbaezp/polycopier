use inquire::{Password, Select, Text};
use rust_decimal::Decimal;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::str::FromStr;

/// How trade size is determined for BUY orders.
#[derive(Clone, Debug, PartialEq)]
pub enum SizingMode {
    /// Always spend exactly `max_trade_size_usd` (default, safest).
    Fixed,
    /// Spend a fixed percentage of OUR current balance per trade.
    /// Floored at $5 (CLOB minimum), capped at `max_trade_size_usd`.
    BalancePct(Decimal),
    /// Mirror the TARGET's portfolio allocation percentage:
    ///   ratio = target_trade_usd / target_portfolio_usd
    ///   our_trade_usd = our_balance × ratio
    /// Falls back to `Fixed` when target portfolio data is not yet available.
    MirrorPct,
    /// Copy the TARGET's exact dollar amount:
    ///   our_trade_usd = target_trade_usd
    /// Floored at MIN_ORDER_USD, capped at `max_trade_size_usd`.
    MirrorAbsolute,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub private_key: String,
    pub funder_address: String,
    pub chain_id: u64,
    pub target_wallets: Vec<String>,
    pub max_slippage_pct: Decimal,
    pub max_trade_size_usd: Decimal,
    pub max_delay_seconds: i64,
    /// Skip copying a position if the target is already this % underwater (e.g. 0.20 = 20% down)
    pub max_copy_loss_pct: Decimal,
    /// Minimum token price for catch-up entries (default 0.02 — filters near-zero dust)
    pub min_entry_price: Decimal,
    /// Maximum token price for catch-up entries (default 0.999 — allows near-certainty positions)
    pub max_entry_price: Decimal,
    /// How to size BUY orders. See [`SizingMode`].
    pub sizing_mode: SizingMode,
}

/// Returns true if a config value looks like a placeholder that hasn't been filled in.
pub fn is_placeholder(val: &str) -> bool {
    let v = val.trim().trim_matches('"');
    v.is_empty()
        || v == "."
        || v.starts_with("your-")
        || v.starts_with("0xYour")
        || v.starts_with("0xTarget")
        || v.contains("here")
    // Note: no length check — short numeric values like "2" or "10" are valid
}

impl Config {
    pub async fn load_or_prompt() -> anyhow::Result<Self> {
        // Try reading existing env variables first
        let _ = dotenvy::dotenv();

        let mut write_new_env = false;

        let private_key = match env::var("PRIVATE_KEY").ok().filter(|v| !is_placeholder(v)) {
            Some(v) => v,
            None => {
                write_new_env = true;
                Password::new("Enter your Polymarket Signer Private Key (Hidden):")
                    .without_confirmation()
                    .prompt()
                    .unwrap_or_default()
            }
        };

        let funder_address = match env::var("FUNDER_ADDRESS")
            .ok()
            .filter(|v| !is_placeholder(v))
        {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Enter your Polymarket Funder Address (Gnosis Safe / Proxy):")
                    .prompt()
                    .unwrap_or_default()
            }
        };

        let target_wallets_str = match env::var("TARGET_WALLETS").ok().filter(|v| {
            // Treat the value as a placeholder if ALL addresses in it look like templates
            let all_placeholder = v.split(',').all(is_placeholder);
            !all_placeholder && !v.trim().is_empty()
        }) {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Enter Target Wallets to copy-trade (comma separated):")
                    .prompt()
                    .unwrap_or_default()
            }
        };

        let max_slippage_str = match env::var("MAX_SLIPPAGE_PCT")
            .ok()
            .filter(|v| !is_placeholder(v))
        {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Max slippage % allowed from copied trade price (e.g. 0.02 = 2%):")
                    .with_default("0.02")
                    .prompt()
                    .unwrap_or_else(|_| "0.02".to_string())
            }
        };

        let max_trade_size_str = match env::var("MAX_TRADE_SIZE_USD")
            .ok()
            .filter(|v| !is_placeholder(v))
        {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Max USD to spend per copied trade:")
                    .with_default("10.00")
                    .prompt()
                    .unwrap_or_else(|_| "10.00".to_string())
            }
        };

        let max_delay_str = match env::var("MAX_DELAY_SECONDS")
            .ok()
            .filter(|v| !is_placeholder(v))
        {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Ignore trades older than N seconds (staleness filter):")
                    .with_default("2")
                    .prompt()
                    .unwrap_or_else(|_| "2".to_string())
            }
        };

        let max_copy_loss_str = match env::var("MAX_COPY_LOSS_PCT")
            .ok()
            .filter(|v| !is_placeholder(v))
        {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Skip copying a target position if it is already X% underwater (e.g. 0.40 = 40%):")
                    .with_default("0.40")
                    .prompt()
                    .unwrap_or_else(|_| "0.40".to_string())
            }
        };

        // Price range — read from env, no wizard prompt (advanced filter, rarely changed)
        let min_entry_price_str =
            env::var("MIN_ENTRY_PRICE").unwrap_or_else(|_| "0.02".to_string());
        let max_entry_price_str =
            env::var("MAX_ENTRY_PRICE").unwrap_or_else(|_| "0.999".to_string());

        // ── Sizing mode ──────────────────────────────────────────────────────
        // Three modes, mutually exclusive, stored in a single SIZING_MODE env var:
        //   fixed        — always spend MAX_TRADE_SIZE_USD
        //   balance_pct  — spend X% of OUR balance (follow-up: asks for %)
        //   mirror       — mirror target's % allocation of THEIR portfolio
        //
        // COPY_SIZE_PCT is kept for backwards compatibility:
        //   if SIZING_MODE is absent but COPY_SIZE_PCT is present, we infer balance_pct.
        let (sizing_mode_str, balance_pct_str) = match env::var("SIZING_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            Some(mode) => {
                // Already configured — read companion BALANCE_PCT if needed
                let pct = env::var("BALANCE_PCT")
                    .or_else(|_| env::var("COPY_SIZE_PCT")) // backwards compat
                    .unwrap_or_else(|_| "0.10".to_string());
                (mode, pct)
            }
            None => {
                // Back-compat: if old COPY_SIZE_PCT is set, use balance_pct silently
                if let Ok(pct) = env::var("COPY_SIZE_PCT") {
                    if !pct.trim().is_empty() && pct.trim() != "0" {
                        return Self::build(
                            private_key,
                            funder_address,
                            target_wallets_str,
                            max_slippage_str,
                            max_trade_size_str,
                            max_delay_str,
                            max_copy_loss_str,
                            min_entry_price_str,
                            max_entry_price_str,
                            "balance_pct".to_string(),
                            pct,
                        );
                    }
                }

                // Fresh setup — ask wizard
                write_new_env = true;
                let options = vec![
                    "mirror_pct — Mirror target's % of their portfolio, scaled to MY balance (recommended)",
                    "mirror_amt — Copy target's exact dollar amount (or platform minimum)",
                    "balance    — Fixed % of MY balance per trade (ignores target's sizing)",
                    "fixed      — Always spend MAX_TRADE_SIZE_USD exactly",
                ];
                let choice = Select::new("Trade sizing strategy:", options)
                    .prompt()
                    .unwrap_or("fixed      — Always spend MAX_TRADE_SIZE_USD exactly");

                let mode = if choice.starts_with("mirror_pct") {
                    "mirror_pct".to_string()
                } else if choice.starts_with("mirror_amt") {
                    "mirror_amt".to_string()
                } else if choice.starts_with("balance") {
                    "balance_pct".to_string()
                } else {
                    "fixed".to_string()
                };

                let pct = if mode == "balance_pct" {
                    Text::new("% of MY balance to use per trade (e.g. 0.10 = 10%):")
                        .with_default("0.10")
                        .prompt()
                        .unwrap_or_else(|_| "0.10".to_string())
                } else {
                    "0.10".to_string() // default, stored but unused in other modes
                };

                (mode, pct)
            }
        };

        if write_new_env {
            println!("Saving credentials to .env...");
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(".env")?;

            writeln!(file, "PRIVATE_KEY=\"{}\"", private_key)?;
            writeln!(file, "FUNDER_ADDRESS=\"{}\"", funder_address)?;
            writeln!(file, "TARGET_WALLETS=\"{}\"", target_wallets_str)?;
            writeln!(file, "MAX_SLIPPAGE_PCT=\"{}\"", max_slippage_str)?;
            writeln!(file, "MAX_TRADE_SIZE_USD=\"{}\"", max_trade_size_str)?;
            writeln!(file, "MAX_DELAY_SECONDS=\"{}\"", max_delay_str)?;
            writeln!(file, "MAX_COPY_LOSS_PCT=\"{}\"", max_copy_loss_str)?;
            writeln!(file, "MIN_ENTRY_PRICE=\"{}\"", min_entry_price_str)?;
            writeln!(file, "MAX_ENTRY_PRICE=\"{}\"", max_entry_price_str)?;
            writeln!(file, "SIZING_MODE=\"{}\"", sizing_mode_str)?;
            writeln!(file, "BALANCE_PCT=\"{}\"", balance_pct_str)?;
        }

        Self::build(
            private_key,
            funder_address,
            target_wallets_str,
            max_slippage_str,
            max_trade_size_str,
            max_delay_str,
            max_copy_loss_str,
            min_entry_price_str,
            max_entry_price_str,
            sizing_mode_str,
            balance_pct_str,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        private_key: String,
        funder_address: String,
        target_wallets_str: String,
        max_slippage_str: String,
        max_trade_size_str: String,
        max_delay_str: String,
        max_copy_loss_str: String,
        min_entry_price_str: String,
        max_entry_price_str: String,
        sizing_mode_str: String,
        balance_pct_str: String,
    ) -> anyhow::Result<Self> {
        let target_wallets: Vec<String> = target_wallets_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let max_slippage_pct = max_slippage_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.02").unwrap());

        let max_trade_size_usd = max_trade_size_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("50.00").unwrap());

        let max_delay_seconds = max_delay_str.parse::<i64>().unwrap_or(10);

        let max_copy_loss_pct = max_copy_loss_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.20").unwrap());

        let min_entry_price = min_entry_price_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.02").unwrap());

        let max_entry_price = max_entry_price_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.999").unwrap());

        let sizing_mode = match sizing_mode_str.trim() {
            "mirror_pct" | "mirror" => SizingMode::MirrorPct, // "mirror" = backwards compat
            "mirror_amt" => SizingMode::MirrorAbsolute,
            "balance_pct" => {
                let pct = balance_pct_str
                    .parse::<Decimal>()
                    .unwrap_or_else(|_| Decimal::from_str("0.10").unwrap());
                let pct = pct.max(Decimal::ZERO).min(Decimal::ONE);
                if pct > Decimal::ZERO {
                    SizingMode::BalancePct(pct)
                } else {
                    SizingMode::Fixed
                }
            }
            _ => SizingMode::Fixed,
        };

        Ok(Self {
            private_key,
            funder_address,
            chain_id: 137,
            target_wallets,
            max_slippage_pct,
            max_trade_size_usd,
            max_delay_seconds,
            max_copy_loss_pct,
            min_entry_price,
            max_entry_price,
            sizing_mode,
        })
    }
}
