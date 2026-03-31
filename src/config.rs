use inquire::{Password, Select, Text};
use rust_decimal::Decimal;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::str::FromStr;

// Re-export SizingMode so callers can import it via `polycopier::config::SizingMode`
pub use crate::models::SizingMode;

#[derive(Clone, Debug)]
pub struct Config {
    pub private_key: String,
    pub funder_address: String,
    pub chain_id: u64,
    pub target_wallets: Vec<String>,
    pub max_slippage_pct: Decimal,
    /// Hard ceiling: no single trade may exceed this regardless of sizing mode.
    pub max_trade_size_usd: Decimal,
    pub max_delay_seconds: i64,
    /// Skip copying a position if the target is already this % underwater (e.g. 0.40 = 40% down)
    pub max_copy_loss_pct: Decimal,
    /// Minimum token price for catch-up entries (default 0.02 -- filters near-zero dust)
    pub min_entry_price: Decimal,
    /// Maximum token price for catch-up entries (default 0.999 -- allows near-certainty positions)
    pub max_entry_price: Decimal,
    /// Which sizing algorithm to use. See [`SizingMode`] for full docs.
    pub sizing_mode: SizingMode,
    /// Fraction of OUR balance to use per trade. Only relevant for `SizingMode::SelfPct`.
    pub copy_size_pct: Option<Decimal>,
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
    // Note: no length check -- short numeric values like "2" or "10" are valid
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

        // Price range and proportional sizing -- read from env, no prompt (advanced settings)
        let min_entry_price_str =
            env::var("MIN_ENTRY_PRICE").unwrap_or_else(|_| "0.02".to_string());
        let max_entry_price_str =
            env::var("MAX_ENTRY_PRICE").unwrap_or_else(|_| "0.999".to_string());

        // -- Sizing mode: mutually exclusive, covers all sizing strategies --
        let sizing_mode_str = match env::var("SIZING_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            Some(v) => v,
            None => {
                write_new_env = true;
                let options = vec![
                    "fixed     -- always use MAX_TRADE_SIZE_USD",
                    "self_pct  -- % of MY balance (set COPY_SIZE_PCT)",
                    "target_usd-- copy target's exact $ amount (capped at MAX_TRADE_SIZE_USD)",
                ];
                let choice = Select::new("Position sizing mode:", options)
                    .with_starting_cursor(1) // default: self_pct
                    .prompt()
                    .unwrap_or("self_pct  -- % of MY balance (set COPY_SIZE_PCT)");
                // Extract the keyword before the em-dash
                choice
                    .split_whitespace()
                    .next()
                    .unwrap_or("self_pct")
                    .to_string()
            }
        };

        // COPY_SIZE_PCT is only needed for self_pct mode
        let copy_size_pct_str =
            if sizing_mode_str.starts_with("self_pct") || sizing_mode_str == "self_pct" {
                match env::var("COPY_SIZE_PCT")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                {
                    Some(v) => Some(v),
                    None => {
                        write_new_env = true;
                        let input =
                            Text::new("Fraction of MY balance to use per trade (e.g. 0.10 = 10%):")
                                .with_default("0.10")
                                .prompt()
                                .unwrap_or_else(|_| "0.10".to_string());
                        let t = input.trim().to_string();
                        if t.is_empty() || t == "0" {
                            None
                        } else {
                            Some(t)
                        }
                    }
                }
            } else {
                // Read from env if set, but don't prompt (irrelevant for other modes)
                env::var("COPY_SIZE_PCT")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
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
            if let Some(ref pct) = copy_size_pct_str {
                writeln!(file, "COPY_SIZE_PCT=\"{}\"", pct)?;
            }
        }

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

        let max_delay_seconds = max_delay_str.parse::<i64>().unwrap_or(2);

        let max_copy_loss_pct = max_copy_loss_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.20").unwrap());

        let min_entry_price = min_entry_price_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.02").unwrap());

        let max_entry_price = max_entry_price_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.999").unwrap());

        let sizing_mode = SizingMode::from_mode_str(&sizing_mode_str);

        let copy_size_pct = copy_size_pct_str
            .as_deref()
            .and_then(|s| s.parse::<Decimal>().ok())
            .filter(|&p| p > Decimal::ZERO && p <= Decimal::ONE);

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
            copy_size_pct,
        })
    }

    /// Always-interactive settings update.
    ///
    /// Unlike `load_or_prompt()` which only prompts for missing/placeholder values,
    /// this loads the current .env values and shows every field in the wizard
    /// pre-filled with those values. The user can accept any field by pressing Enter
    /// or type a new value. Called when [s] is pressed inside the TUI.
    pub async fn prompt_update() -> anyhow::Result<Self> {
        // Load whatever is currently in .env so we can use it as defaults.
        let _ = dotenvy::dotenv();
        let cur = |key: &str| env::var(key).unwrap_or_default();

        println!();
        println!("=== polycopier settings (press Enter to keep current value) ===");
        println!();

        let private_key = Password::new("Signer Private Key:")
            .without_confirmation()
            .prompt()
            .unwrap_or_else(|_| cur("PRIVATE_KEY"));
        // If the user just pressed Enter, password returns an empty string -- keep current
        let private_key = if private_key.is_empty() {
            cur("PRIVATE_KEY")
        } else {
            private_key
        };

        let funder_address = Text::new("Funder Address (Gnosis Safe / Proxy):")
            .with_default(&cur("FUNDER_ADDRESS"))
            .prompt()
            .unwrap_or_else(|_| cur("FUNDER_ADDRESS"));

        let target_wallets_str = Text::new("Target Wallets (comma-separated):")
            .with_default(&cur("TARGET_WALLETS"))
            .prompt()
            .unwrap_or_else(|_| cur("TARGET_WALLETS"));

        let max_slippage_str = Text::new("Max slippage % (e.g. 0.02 = 2%):")
            .with_default(&{
                let v = cur("MAX_SLIPPAGE_PCT");
                if v.is_empty() {
                    "0.02".into()
                } else {
                    v
                }
            })
            .prompt()
            .unwrap_or_else(|_| cur("MAX_SLIPPAGE_PCT"));

        let max_trade_size_str = Text::new("Max USD per copied trade:")
            .with_default(&{
                let v = cur("MAX_TRADE_SIZE_USD");
                if v.is_empty() {
                    "10.00".into()
                } else {
                    v
                }
            })
            .prompt()
            .unwrap_or_else(|_| cur("MAX_TRADE_SIZE_USD"));

        let max_delay_str = Text::new("Max event age in seconds (staleness filter):")
            .with_default(&{
                let v = cur("MAX_DELAY_SECONDS");
                if v.is_empty() {
                    "2".into()
                } else {
                    v
                }
            })
            .prompt()
            .unwrap_or_else(|_| cur("MAX_DELAY_SECONDS"));

        let max_copy_loss_str = Text::new("Max target drawdown before skipping (e.g. 0.40 = 40%):")
            .with_default(&{
                let v = cur("MAX_COPY_LOSS_PCT");
                if v.is_empty() {
                    "0.40".into()
                } else {
                    v
                }
            })
            .prompt()
            .unwrap_or_else(|_| cur("MAX_COPY_LOSS_PCT"));

        let min_entry_price_str = Text::new("Min entry price for catch-up trades (e.g. 0.02):")
            .with_default(&{
                let v = cur("MIN_ENTRY_PRICE");
                if v.is_empty() {
                    "0.02".into()
                } else {
                    v
                }
            })
            .prompt()
            .unwrap_or_else(|_| cur("MIN_ENTRY_PRICE"));

        let max_entry_price_str = Text::new("Max entry price for catch-up trades (e.g. 0.999):")
            .with_default(&{
                let v = cur("MAX_ENTRY_PRICE");
                if v.is_empty() {
                    "0.999".into()
                } else {
                    v
                }
            })
            .prompt()
            .unwrap_or_else(|_| cur("MAX_ENTRY_PRICE"));

        let sizing_options = vec![
            "fixed     -- always use MAX_TRADE_SIZE_USD",
            "self_pct  -- % of MY balance (set COPY_SIZE_PCT)",
            "target_usd-- copy target's exact $ amount (capped at MAX_TRADE_SIZE_USD)",
        ];
        let cur_sizing = cur("SIZING_MODE");
        let starting = sizing_options
            .iter()
            .position(|o| o.starts_with(cur_sizing.trim()))
            .unwrap_or(1);
        let sizing_choice = Select::new("Position sizing mode:", sizing_options)
            .with_starting_cursor(starting)
            .prompt()
            .unwrap_or("fixed     -- always use MAX_TRADE_SIZE_USD");
        let sizing_mode_str = sizing_choice
            .split_whitespace()
            .next()
            .unwrap_or("fixed")
            .to_string();

        let copy_size_pct_str: Option<String> = if sizing_mode_str == "self_pct" {
            let default_pct = {
                let v = cur("COPY_SIZE_PCT");
                if v.is_empty() {
                    "0.10".into()
                } else {
                    v
                }
            };
            let v = Text::new("Fraction of MY balance per trade (e.g. 0.10 = 10%):")
                .with_default(&default_pct)
                .prompt()
                .unwrap_or(default_pct);
            Some(v).filter(|s| !s.is_empty() && s != "0")
        } else {
            env::var("COPY_SIZE_PCT")
                .ok()
                .filter(|v| !v.trim().is_empty())
        };

        // Always write .env so the new values survive a restart.
        println!("Saving settings to .env...");
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
        if let Some(ref pct) = copy_size_pct_str {
            writeln!(file, "COPY_SIZE_PCT=\"{}\"", pct)?;
        }

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

        let max_delay_seconds = max_delay_str.parse::<i64>().unwrap_or(2);

        let max_copy_loss_pct = max_copy_loss_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.20").unwrap());

        let min_entry_price = min_entry_price_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.02").unwrap());

        let max_entry_price = max_entry_price_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.999").unwrap());

        let sizing_mode = SizingMode::from_mode_str(&sizing_mode_str);

        let copy_size_pct = copy_size_pct_str
            .as_deref()
            .and_then(|s| s.parse::<Decimal>().ok())
            .filter(|&p| p > Decimal::ZERO && p <= Decimal::ONE);

        println!("Settings saved. Restarting bot...");

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
            copy_size_pct,
        })
    }
}
