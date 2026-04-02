//! Configuration loading — two-file design:
//!
//! | File            | Contains                          | Version-controlled? |
//! |-----------------|-----------------------------------|---------------------|
//! | `.env`          | Secrets: `PRIVATE_KEY`, `FUNDER_ADDRESS` only | **No** |
//! | `config.toml`   | All tunables + `[targets]` (wallet addresses) | **Yes** |
//!
//! ## Loading order
//!
//! 1. `.env` is read via `dotenvy` for secrets and any legacy tunable keys.
//! 2. `config.toml` is read for tunables; it takes precedence over `.env` for
//!    shared keys so the split is a non-breaking migration.
//! 3. If `config.toml` does not exist, it is generated from defaults (and from
//!    any legacy tunable values already in `.env`) so existing setups continue
//!    to work without manual intervention.
//! 4. Interactive prompts are only shown for secrets that are still missing or
//!    look like placeholders.

use inquire::{Password, Text};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;

// Re-export SizingMode so callers can import it via `polycopier::config::SizingMode`
pub use crate::models::SizingMode;

// ---------------------------------------------------------------------------
// TOML-serialisable tunables structure
// ---------------------------------------------------------------------------

/// All non-secret tunables + target wallet list.
/// Written to / read from `config.toml`.
/// Secrets (PRIVATE_KEY, FUNDER_ADDRESS) stay in `.env`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    pub targets: TargetsConfig,
    pub execution: ExecutionConfig,
    pub sizing: SizingConfig,
    pub scanner: ScannerConfig,
    pub risk: RiskConfig,
    pub ledger: LedgerConfig,
}

/// Copy-trade target wallets — public on-chain addresses, safe in config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsConfig {
    /// Polymarket proxy wallet addresses to copy-trade.
    pub wallets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Slippage buffer applied to copied trade price for limit orders (0.02 = 2%).
    pub max_slippage_pct: Decimal,
    /// Hard ceiling per copied trade regardless of sizing mode.
    pub max_trade_size_usd: Decimal,
    /// Discard listener events older than this many seconds (staleness filter).
    pub max_delay_seconds: i64,
    /// SELL size = held_size × sell_fee_buffer. Absorbs CLOB fee. Default 0.97.
    pub sell_fee_buffer: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizingConfig {
    /// Sizing algorithm: "self_pct" | "target_usd" | "fixed".
    pub mode: String,
    /// Fraction of our balance per trade for self_pct mode (e.g. 0.15 = 15%).
    pub copy_size_pct: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerConfig {
    /// Skip catch-up if target is already this % underwater (0.40 = 40%).
    pub max_copy_loss_pct: Decimal,
    /// Skip catch-up if target is already this % in profit (0.05 = 5%).
    pub max_copy_gain_pct: Decimal,
    /// Minimum token price for catch-up entries (filters near-zero dust).
    pub min_entry_price: Decimal,
    /// Maximum token price for catch-up entries.
    pub max_entry_price: Decimal,
    /// Max positions queued per scan cycle (default 1 = conservative).
    pub max_entries_per_cycle: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Max USD traded per UTC day (BUY + SELL). 0 = disabled.
    pub max_daily_volume_usd: Decimal,
    /// Consecutive losses before cooldown pause. 0 = disabled.
    pub max_consecutive_losses: u32,
    /// Seconds to pause after hitting max_consecutive_losses. Default 300.
    pub loss_cooldown_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerConfig {
    /// Days to keep closed ledger entries. 0 = never prune.
    pub retention_days: u32,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            targets: TargetsConfig { wallets: vec![] },
            execution: ExecutionConfig {
                max_slippage_pct: Decimal::from_str("0.02").unwrap(),
                max_trade_size_usd: Decimal::from_str("10.00").unwrap(),
                max_delay_seconds: 10,
                sell_fee_buffer: Decimal::from_str("0.97").unwrap(),
            },
            sizing: SizingConfig {
                mode: "self_pct".to_string(),
                copy_size_pct: Some(Decimal::from_str("0.15").unwrap()),
            },
            scanner: ScannerConfig {
                max_copy_loss_pct: Decimal::from_str("0.40").unwrap(),
                max_copy_gain_pct: Decimal::from_str("0.05").unwrap(),
                min_entry_price: Decimal::from_str("0.02").unwrap(),
                max_entry_price: Decimal::from_str("0.999").unwrap(),
                max_entries_per_cycle: 1,
            },
            risk: RiskConfig {
                max_daily_volume_usd: Decimal::ZERO,
                max_consecutive_losses: 0,
                loss_cooldown_secs: 300,
            },
            ledger: LedgerConfig { retention_days: 90 },
        }
    }
}

const CONFIG_TOML_PATH: &str = "config.toml";

// ---------------------------------------------------------------------------
// Flat runnable Config (what the rest of the code sees)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Config {
    // Secrets (from .env — never logged or committed)
    pub private_key: String,
    pub funder_address: String,
    pub chain_id: u64,

    // Target wallets (from config.toml [targets].wallets)
    // Target wallets (from config.toml [targets].wallets)
    pub target_wallets: Vec<String>,
    pub target_scalars: std::collections::HashMap<String, Decimal>,

    // Tunables (from config.toml)
    pub max_slippage_pct: Decimal,
    pub max_trade_size_usd: Decimal,
    pub max_delay_seconds: i64,
    pub max_copy_loss_pct: Decimal,
    pub max_copy_gain_pct: Decimal,
    pub min_entry_price: Decimal,
    pub max_entry_price: Decimal,
    pub sizing_mode: SizingMode,
    pub copy_size_pct: Option<Decimal>,
    pub scan_max_entries_per_cycle: usize,
    pub sell_fee_buffer: Decimal,
    pub ledger_retention_days: u32,
    pub max_daily_volume_usd: Decimal,
    pub max_consecutive_losses: u32,
    pub loss_cooldown_secs: u64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns true if a config value looks like a placeholder that hasn't been filled in.
pub fn is_placeholder(val: &str) -> bool {
    let v = val.trim().trim_matches('"');
    v.is_empty()
        || v == "."
        || v.starts_with("your-")
        || v.starts_with("0xYour")
        || v.starts_with("0xTarget")
        || v.contains("here")
}

/// Returns true if the value looks like a valid 32-byte EVM private key (64 hex chars,
/// optionally prefixed with "0x"). Used to catch test/placeholder keys before the
/// alloy SDK gives an opaque "invalid string length" error.
pub fn is_valid_private_key_format(val: &str) -> bool {
    let v = val.trim().trim_matches('"');
    let hex = v.strip_prefix("0x").unwrap_or(v);
    hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Load `config.toml` if it exists. Returns `None` on missing or parse error.
fn load_toml() -> Option<BotConfig> {
    let raw = fs::read_to_string(CONFIG_TOML_PATH).ok()?;
    match toml::from_str::<BotConfig>(&raw) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("config.toml parse error — using defaults: {e}");
            None
        }
    }
}

/// Write `BotConfig` to `config.toml` with inline comments.
pub fn write_toml(cfg: &BotConfig) -> anyhow::Result<()> {
    // Format the wallets as a TOML inline array.
    let wallets_toml = if cfg.targets.wallets.is_empty() {
        "[]".to_string()
    } else {
        let quoted: Vec<String> = cfg
            .targets
            .wallets
            .iter()
            .map(|w| format!("\"{w}\""))
            .collect();
        format!("[{}]", quoted.join(", "))
    };

    let content = format!(
        r#"# polycopier config -- safe to version control (no secrets here)
# Secrets (PRIVATE_KEY, FUNDER_ADDRESS) stay in .env

[targets]
# Polymarket proxy wallet addresses to copy-trade
wallets = {wallets}

[execution]
# Slippage buffer applied to copied trade price for limit orders (2% = 0.02)
max_slippage_pct = {slippage}
# Hard ceiling per copied trade in USD
max_trade_size_usd = {max_trade}
# Drop listener events older than N seconds (staleness filter)
max_delay_seconds = {delay}
# SELL size = held_size x sell_fee_buffer (absorbs CLOB fee, default 0.97)
sell_fee_buffer = {fee_buf}

[sizing]
# Sizing algorithm: "self_pct" | "target_usd" | "fixed"
mode = "{mode}"
# Fraction of our balance per trade for self_pct mode (0.15 = 15%)
{copy_size_line}

[scanner]
# Skip catch-up if target already this % underwater (0.40 = 40%)
max_copy_loss_pct = {loss_pct}
# Skip catch-up if target already this % in profit (0.05 = 5%)
max_copy_gain_pct = {gain_pct}
# Minimum token price for catch-up entries (filters near-zero dust)
min_entry_price = {min_price}
# Maximum token price for catch-up entries
max_entry_price = {max_price}
# Max positions queued per scan cycle (1 = conservative, raise to 2-3 for bulk)
max_entries_per_cycle = {max_entries}

[risk]
# Max USD traded per UTC day (BUY + SELL combined). 0 = disabled.
max_daily_volume_usd = {daily_vol}
# Consecutive losses before triggering a cooldown pause. 0 = disabled.
max_consecutive_losses = {consec_loss}
# Seconds to pause after hitting max_consecutive_losses
loss_cooldown_secs = {cooldown}

[ledger]
# Days to keep closed ledger entries before pruning on startup. 0 = never prune.
retention_days = {retention}
"#,
        wallets = wallets_toml,
        slippage = cfg.execution.max_slippage_pct,
        max_trade = cfg.execution.max_trade_size_usd,
        delay = cfg.execution.max_delay_seconds,
        fee_buf = cfg.execution.sell_fee_buffer,
        mode = cfg.sizing.mode,
        copy_size_line = match cfg.sizing.copy_size_pct {
            Some(p) => format!("copy_size_pct = {p}"),
            None => "# copy_size_pct = 0.15  # only used for self_pct mode".to_string(),
        },
        loss_pct = cfg.scanner.max_copy_loss_pct,
        gain_pct = cfg.scanner.max_copy_gain_pct,
        min_price = cfg.scanner.min_entry_price,
        max_price = cfg.scanner.max_entry_price,
        max_entries = cfg.scanner.max_entries_per_cycle,
        daily_vol = cfg.risk.max_daily_volume_usd,
        consec_loss = cfg.risk.max_consecutive_losses,
        cooldown = cfg.risk.loss_cooldown_secs,
        retention = cfg.ledger.retention_days,
    );
    fs::write(CONFIG_TOML_PATH, content)?;
    Ok(())
}

/// Write `.env` with secrets only (PRIVATE_KEY and FUNDER_ADDRESS).
/// TARGET_WALLETS is now in config.toml [targets].wallets.
pub fn write_secrets_env(private_key: &str, funder_address: &str) -> anyhow::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(".env")?;
    writeln!(f, "# polycopier secrets -- DO NOT version control")?;
    writeln!(f, "PRIVATE_KEY=\"{private_key}\"")?;
    writeln!(f, "FUNDER_ADDRESS=\"{funder_address}\"")?;
    Ok(())
}

/// Migrate legacy tunable values from `.env` into a `BotConfig`.
/// When a key is present in `.env`, it overrides the default.
/// This is called when `config.toml` doesn't exist so existing setups migrate seamlessly.
/// TARGET_WALLETS is also migrated to the targets section.
fn migrate_from_env(defaults: BotConfig) -> BotConfig {
    let e = |k: &str| env::var(k).unwrap_or_default();
    let dec = |k: &str, fallback: Decimal| -> Decimal {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let u32v = |k: &str, fallback: u32| -> u32 {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let u64v = |k: &str, fallback: u64| -> u64 {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let i64v = |k: &str, fallback: i64| -> i64 {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let usizev = |k: &str, fallback: usize| -> usize {
        env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
            .max(1)
    };

    let sizing_mode = e("SIZING_MODE");
    let sizing_mode = if sizing_mode.is_empty() {
        defaults.sizing.mode.clone()
    } else {
        sizing_mode
    };

    let copy_size_pct = env::var("COPY_SIZE_PCT")
        .ok()
        .and_then(|v| v.parse::<Decimal>().ok())
        .filter(|&p| p > Decimal::ZERO && p <= Decimal::ONE)
        .or(defaults.sizing.copy_size_pct);

    // Migrate TARGET_WALLETS from old .env format (comma-separated string) to Vec<String>
    let legacy_wallets: Vec<String> = env::var("TARGET_WALLETS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && !is_placeholder(s))
        .collect();
    let wallets = if legacy_wallets.is_empty() {
        defaults.targets.wallets
    } else {
        legacy_wallets
    };

    BotConfig {
        targets: TargetsConfig { wallets },
        execution: ExecutionConfig {
            max_slippage_pct: dec("MAX_SLIPPAGE_PCT", defaults.execution.max_slippage_pct),
            max_trade_size_usd: dec("MAX_TRADE_SIZE_USD", defaults.execution.max_trade_size_usd),
            max_delay_seconds: i64v("MAX_DELAY_SECONDS", defaults.execution.max_delay_seconds),
            sell_fee_buffer: dec("SELL_FEE_BUFFER", defaults.execution.sell_fee_buffer),
        },
        sizing: SizingConfig {
            mode: sizing_mode,
            copy_size_pct,
        },
        scanner: ScannerConfig {
            max_copy_loss_pct: dec("MAX_COPY_LOSS_PCT", defaults.scanner.max_copy_loss_pct),
            max_copy_gain_pct: dec("MAX_COPY_GAIN_PCT", defaults.scanner.max_copy_gain_pct),
            min_entry_price: dec("MIN_ENTRY_PRICE", defaults.scanner.min_entry_price),
            max_entry_price: dec("MAX_ENTRY_PRICE", defaults.scanner.max_entry_price),
            max_entries_per_cycle: usizev(
                "SCAN_MAX_ENTRIES_PER_CYCLE",
                defaults.scanner.max_entries_per_cycle,
            ),
        },
        risk: RiskConfig {
            max_daily_volume_usd: dec("MAX_DAILY_VOLUME_USD", defaults.risk.max_daily_volume_usd),
            max_consecutive_losses: u32v(
                "MAX_CONSECUTIVE_LOSSES",
                defaults.risk.max_consecutive_losses,
            ),
            loss_cooldown_secs: u64v("LOSS_COOLDOWN_SECS", defaults.risk.loss_cooldown_secs),
        },
        ledger: LedgerConfig {
            retention_days: u32v("LEDGER_RETENTION_DAYS", defaults.ledger.retention_days),
        },
    }
}

// ---------------------------------------------------------------------------
// Config::load_or_prompt — primary entry point
// ---------------------------------------------------------------------------

impl Config {
    pub async fn load_or_prompt(is_ui: bool) -> anyhow::Result<Self> {
        // Load .env (secrets + any legacy tunable keys)
        let _ = dotenvy::dotenv();

        let mut write_new_env = false;

        // -- Secrets: prompt only if missing, placeholder, or invalid format ---
        //
        // A valid EVM private key is exactly 32 bytes = 64 hex chars (+ optional "0x").
        // We re-prompt if the key fails this check so the alloy SDK never sees a
        // short/invalid key and crashes with the opaque "invalid string length" error.

        let funder_address = match env::var("FUNDER_ADDRESS")
            .ok()
            .filter(|v| !is_placeholder(v) && !v.is_empty())
        {
            Some(v) => v,
            None => {
                if is_ui {
                    tracing::info!("Web UI Setup Mode active! Please complete your configuration in the browser at http://localhost:3000");
                    std::future::pending::<()>().await;
                }

                write_new_env = true;
                Text::new("Enter your Polymarket Funder Address (Gnosis Safe / Proxy):")
                    .prompt()
                    .unwrap_or_default()
            }
        };

        let private_key = match env::var("PRIVATE_KEY")
            .ok()
            .filter(|v| !is_placeholder(v) && is_valid_private_key_format(v))
        {
            Some(v) => v,
            None => {
                if is_ui {
                    tracing::info!("Web UI Setup Mode active! Please complete your configuration in the browser at http://localhost:3000");
                    std::future::pending::<()>().await;
                }

                write_new_env = true;
                println!(
                    "PRIVATE_KEY is missing or invalid. A valid key is 64 hex chars (32 bytes)."
                );
                Password::new("Enter your Polymarket Signer Private Key (Hidden):")
                    .without_confirmation()
                    .prompt()
                    .unwrap_or_default()
            }
        };

        // -- Tunables + targets: load from config.toml, or migrate from .env ---

        let (toml_cfg, write_new_toml) = if Path::new(CONFIG_TOML_PATH).exists() {
            // config.toml already exists — use it.
            // But if the targets list is empty, we still need to prompt.
            let cfg = load_toml().unwrap_or_default();
            (cfg, false)
        } else {
            // First run or legacy setup: migrate any .env tunable + TARGET_WALLETS keys.
            let migrated = migrate_from_env(BotConfig::default());

            // If using self_pct and COPY_SIZE_PCT wasn't in env, prompt for it
            let migrated = if migrated.sizing.mode.starts_with("self_pct")
                && migrated.sizing.copy_size_pct.is_none()
            {
                let pct_str =
                    Text::new("Fraction of MY balance to use per trade (e.g. 0.15 = 15%):")
                        .with_default("0.15")
                        .prompt()
                        .unwrap_or_else(|_| "0.15".to_string());
                let copy_size_pct = pct_str
                    .parse::<Decimal>()
                    .ok()
                    .filter(|&p| p > Decimal::ZERO && p <= Decimal::ONE);
                BotConfig {
                    sizing: SizingConfig {
                        copy_size_pct,
                        ..migrated.sizing
                    },
                    ..migrated
                }
            } else {
                migrated
            };

            (migrated, true)
        };

        // -- Prompt for target wallets if the list is empty -------------------
        // (Either first run with no legacy TARGET_WALLETS, or config.toml
        //  was manually created without a [targets] section.)
        let mut prompted_targets = false;
        let toml_cfg = if toml_cfg.targets.wallets.is_empty() && !is_ui {
            prompted_targets = true;
            let raw = Text::new("Enter Target Wallets to copy-trade (comma separated):")
                .prompt()
                .unwrap_or_default();
            let wallets: Vec<String> = raw
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty() && !is_placeholder(s))
                .collect();
            BotConfig {
                targets: TargetsConfig { wallets },
                ..toml_cfg
            }
        } else {
            toml_cfg
        };

        // Write config.toml if it was newly generated / targets were just prompted.
        if write_new_toml || prompted_targets {
            if let Err(e) = write_toml(&toml_cfg) {
                tracing::warn!("Failed to write config.toml: {e}");
            } else if write_new_toml {
                println!("Generated config.toml from current settings.");
            }
        }

        // Write .env (secrets only: PRIVATE_KEY + FUNDER_ADDRESS) if prompted.
        if write_new_env {
            if let Err(e) = write_secrets_env(&private_key, &funder_address) {
                tracing::warn!("Failed to write .env: {e}");
            }
        }

        // -- Validate required fields before handing off ----------------------
        if private_key.trim().is_empty() {
            anyhow::bail!(
                "PRIVATE_KEY is missing.\n\
                 Set it in .env or re-run to be prompted."
            );
        }
        if !is_valid_private_key_format(&private_key) {
            anyhow::bail!(
                "PRIVATE_KEY looks invalid (expected 64 hex chars, got {}).\n\
                 Check .env and re-run.",
                private_key.trim_matches('"').trim_start_matches("0x").len()
            );
        }
        if funder_address.trim().is_empty() {
            anyhow::bail!(
                "FUNDER_ADDRESS is missing.\n\
                 Set it in .env or re-run to be prompted."
            );
        }
        if toml_cfg.targets.wallets.is_empty() && !is_ui {
            anyhow::bail!(
                "No target wallets configured.\n\
                 Add addresses to [targets].wallets in config.toml or re-run to be prompted."
            );
        }

        Ok(Self::from_parts(private_key, funder_address, toml_cfg))
    }

    /// Build a flat [`Config`] from secrets + a [`BotConfig`].
    fn from_parts(private_key: String, funder_address: String, cfg: BotConfig) -> Self {
        let mut target_wallets = Vec::new();
        let mut target_scalars = std::collections::HashMap::new();

        for entry in cfg.targets.wallets.iter() {
            let s = entry.trim().to_lowercase();
            if s.is_empty() {
                continue;
            }
            if let Some((addr, scalar_str)) = s.split_once(':') {
                let clean_addr = addr.trim().to_string();
                target_wallets.push(clean_addr.clone());
                let scalar = rust_decimal::Decimal::from_str(scalar_str.trim())
                    .unwrap_or(rust_decimal::Decimal::ONE);
                target_scalars.insert(clean_addr, scalar);
            } else {
                target_wallets.push(s.clone());
                target_scalars.insert(s, rust_decimal::Decimal::ONE);
            }
        }

        let sizing_mode = SizingMode::from_mode_str(&cfg.sizing.mode);

        Self {
            private_key,
            funder_address,
            chain_id: 137,
            target_wallets,
            target_scalars,
            max_slippage_pct: cfg.execution.max_slippage_pct,
            max_trade_size_usd: cfg.execution.max_trade_size_usd,
            max_delay_seconds: cfg.execution.max_delay_seconds,
            max_copy_loss_pct: cfg.scanner.max_copy_loss_pct,
            max_copy_gain_pct: cfg.scanner.max_copy_gain_pct,
            min_entry_price: cfg.scanner.min_entry_price,
            max_entry_price: cfg.scanner.max_entry_price,
            sizing_mode,
            copy_size_pct: cfg.sizing.copy_size_pct,
            scan_max_entries_per_cycle: cfg.scanner.max_entries_per_cycle,
            sell_fee_buffer: cfg.execution.sell_fee_buffer,
            ledger_retention_days: cfg.ledger.retention_days,
            max_daily_volume_usd: cfg.risk.max_daily_volume_usd,
            max_consecutive_losses: cfg.risk.max_consecutive_losses,
            loss_cooldown_secs: cfg.risk.loss_cooldown_secs,
        }
    }

    /// Reload config from disk (called after in-TUI settings save).
    /// Reads secrets from `.env`; target wallets + tunables from `config.toml`.
    pub fn reload() -> anyhow::Result<Self> {
        let _ = dotenvy::dotenv();
        let private_key = env::var("PRIVATE_KEY").unwrap_or_default();
        let funder_address = env::var("FUNDER_ADDRESS").unwrap_or_default();
        let cfg = load_toml().unwrap_or_default();
        Ok(Self::from_parts(private_key, funder_address, cfg))
    }
}

// ────────────────────────────────────────────────────────────────────────
// CLI Parsing Logic (Extracted for Testing)
// ────────────────────────────────────────────────────────────────────────

pub struct CliArgs {
    pub is_daemon: bool,
    pub is_ui: bool,
    pub skip_open: bool,
    pub headless: bool,
}

pub fn parse_cli_args(args: &[String]) -> CliArgs {
    let is_daemon = args.iter().any(|a| a == "--daemon" || a == "--headless");
    let is_ui = args.iter().any(|a| a == "--ui" || a == "--ui-reboot");
    let skip_open = args.iter().any(|a| a == "--ui-reboot");
    let headless = is_daemon || is_ui;

    CliArgs {
        is_daemon,
        is_ui,
        skip_open,
        headless,
    }
}
