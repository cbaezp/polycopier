use std::env;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::fs::OpenOptions;
use std::io::Write;
use inquire::{Password, Text};

#[derive(Clone, Debug)]
pub struct Config {
    pub private_key: String,
    pub funder_address: String,
    pub chain_id: u64,
    pub target_wallets: Vec<String>,
    pub max_slippage_pct: Decimal,
    pub max_trade_size_usd: Decimal,
    pub max_delay_seconds: i64,
    /// Skip copying a position if the target is already this % underwater (e.g. 0.40 = 40% down)
    pub max_copy_loss_pct: Decimal,
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
        || v.len() < 3
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

        let funder_address = match env::var("FUNDER_ADDRESS").ok().filter(|v| !is_placeholder(v)) {
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

        let max_slippage_str = match env::var("MAX_SLIPPAGE_PCT").ok().filter(|v| !is_placeholder(v)) {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Max slippage % allowed from copied trade price (e.g. 0.02 = 2%):")
                    .with_default("0.02")
                    .prompt()
                    .unwrap_or_else(|_| "0.02".to_string())
            }
        };

        let max_trade_size_str = match env::var("MAX_TRADE_SIZE_USD").ok().filter(|v| !is_placeholder(v)) {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Max USD to spend per copied trade:")
                    .with_default("10.00")
                    .prompt()
                    .unwrap_or_else(|_| "10.00".to_string())
            }
        };

        let max_delay_str = match env::var("MAX_DELAY_SECONDS").ok().filter(|v| !is_placeholder(v)) {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Ignore trades older than N seconds (staleness filter):")
                    .with_default("2")
                    .prompt()
                    .unwrap_or_else(|_| "2".to_string())
            }
        };

        let max_copy_loss_str = match env::var("MAX_COPY_LOSS_PCT").ok().filter(|v| !is_placeholder(v)) {
            Some(v) => v,
            None => {
                write_new_env = true;
                Text::new("Skip copying a target position if it is already X% underwater (e.g. 0.40 = 40%):")
                    .with_default("0.40")
                    .prompt()
                    .unwrap_or_else(|_| "0.40".to_string())
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
            .unwrap_or_else(|_| Decimal::from_str("10.00").unwrap());

        let max_delay_seconds = max_delay_str
            .parse::<i64>()
            .unwrap_or(2);

        let max_copy_loss_pct = max_copy_loss_str
            .parse::<Decimal>()
            .unwrap_or_else(|_| Decimal::from_str("0.40").unwrap());

        Ok(Self {
            private_key,
            funder_address,
            chain_id: 137,
            target_wallets,
            max_slippage_pct,
            max_trade_size_usd,
            max_delay_seconds,
            max_copy_loss_pct,
        })
    }
}

