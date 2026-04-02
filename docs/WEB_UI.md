# Web UI Dashboard Documentation

The Polycopier Web Dashboard is a comprehensive, glassmorphism-styled React application served natively by the Rust backend. It acts as the primary control center for managing targets, tweaking risk parameters, and monitoring bot performance.

## 1. Web Setup Wizard
When running the bot for the first time without an `.env` file via `cargo run --release -- --ui`, the Web UI automatically displays the Setup Wizard.
*   **Funder Address:** Captures your Gnosis Safe or proxy wallet address holding USDC.
*   **Private Key:** Securely captures your signing key.
*   **Hot-Reboot Engine:** Upon clicking "Initialize", the Rust backend intercepts the credentials, saves them securely to `.env`, generates a default `config.toml`, and gracefully re-executes itself in operational mode (`--ui-reboot`) to spin up the actual dashboard seamlessly without launching duplicate browser tabs.

## 2. Live Performance Feed (Header)
The top header provides real-time state:
*   **Active State:** Displays whether the bot is `Running` or `Checking X targets...` based on API polling state.
*   **Total Balance:** Live aggregate USDC value of your funder address fetched directly from your backend proxy.
*   **Total Realized / Unrealized PnL:** Performance metrics extracted from closed and active trades dynamically.
*   **Target Map:** Displays all current target addresses the backend is natively scraping.

## 3. Position Monitor (Main Body)
The left panel presents all active and closed trades. 
*   **Active Positions:** Dynamically displays live positions mapped to Polymarket order book prices. Includes token name, entry price, current price drift, and realized loss/gain.
*   **Closed Positions:** Retains the lifecycle of executed trades for accountability and post-mortem analysis.

## 4. Settings Manager
Clicking the Settings icon (Gear) opens the visual editor mapped explicitly to `config.toml`. All changes made here automatically trigger a backend reload without restarting the Rust process.

### Global Controls
*   **Target Wallets:** Add, remove, or modify the Polymarket addresses you wish to mimic via a pill-based interface.
*   **Poll Interval:** Dynamically adjusts the execution speed and scan frequency to prevent rate limits.

### Sizing Mode
Provides intuitive dropdowns for order calculation:
*   `fixed`: Uses exactly X shares per trade regardless of current balance.
*   `self_pct`: Calculates trade size based on a percentage of YOUR active total portfolio value.
*   `copy_pct`: Calculates trade size based on a percentage of the TARGET's trade value.

### Risk Management parameters
Easily enforce strict limits to prevent blowout scenarios:
*   **Max Absolute USD per trade:** Hardcap to prevent 100% wallet drain on single tokens.
*   **Slippage (% limits):** Refuses execution if the order book is actively shifting outside tolerable boundaries.
*   **Max open positions:** Total positions the bot will handle simultaneously.
*   **Max consecutive losses & Loss cooldown time:** Failsafes to suspend scraping if the bot catches extreme negative deviation repeatedly.
