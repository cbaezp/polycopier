# polycopier

A high-performance, terminal-based copy trading bot for Polymarket prediction markets,
built in Rust against the official [`polymarket-client-sdk`](https://github.com/Polymarket/rs-clob-client).

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org)
[![Status: Experimental](https://img.shields.io/badge/status-experimental-red.svg)](#disclaimer)
[![CI](https://github.com/cbaezp/polycopier/actions/workflows/ci.yml/badge.svg)](https://github.com/cbaezp/polycopier/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/cbaezp/polycopier?include_prereleases)](https://github.com/cbaezp/polycopier/releases)

> **Experimental Software.**
> This project is in active development and has not been audited for production use.
> It executes real trades on your behalf. Run it only with capital you can afford to lose,
> keep `MAX_TRADE_SIZE_USD` low while testing, and review all risk parameters before
> increasing position sizes. See the [Disclaimer](#disclaimer) section for full details.

---

## Overview

polycopier monitors one or more target wallets on Polymarket and automatically mirrors
their trades into your own account in real time.

**Two independent signal sources feed the same execution engine:**

1. **Real-time listener** — polls the Data API every 2 seconds and copies new fills the
   moment they appear, using transaction-hash deduplication to handle burst activity.

2. **Adaptive position scanner** — scans the target's full open-position portfolio and
   evaluates catch-up entries (positions the target had open before the bot started).
   Scan frequency adapts dynamically between 10 and 60 seconds based on how close each
   target position's current price is to their original entry — scanning most aggressively
   when a catch-up entry is still at a favorable price.

**Trade intent classification** — the bot tracks the target's current positions (via the
scanner data) and uses them to classify every event before acting:

| Target has position? | Event | Bot has position? | Intent | Action |
|---|---|---|---|---|
| No | BUY | No | Fresh long entry | Copy BUY |
| Yes | BUY | Any | Adding to long | Copy BUY |
| No | BUY | Yes | Closing a short | **Skip** |
| Yes | SELL | Yes | Closing long | Copy SELL |
| Yes | SELL | No | Closing long we missed | **Skip** |
| No | SELL | Any | Opening a short | **Skip** |

The bot **never exits a position autonomously**. All exits are triggered only when the
target wallet closes their position.

The entire interface is a terminal UI (TUI) with no browser required.

---

## Features

- **Real-time trade copying** — polls the Polymarket Data API every 2 seconds with a
  rate limit of 20 fills per cycle. Deduplicates by transaction hash (not timestamp) so
  burst activity is never silently dropped.

- **Adaptive open-position scanner** — catches positions the target opened before the bot
  started. Scan interval scales from 10s (target position still at entry price) to 60s
  (price has moved significantly or no enterable positions exist).

- **Intent classification** — every incoming BUY and SELL is checked against the target's
  last-known positions to determine true intent (fresh entry, adding to long, closing long,
  closing short). Short entries and short closures by the target are correctly skipped.

- **SELL execution guards**:
  - 97% collateral buffer applied to all SELL sizes to satisfy the CLOB's fee reserve requirement.
  - SELL quantities are based on **our own held size**, not the target's order size.
  - SELL is never submitted for a token we don't hold.

- **Proportional position sizing** (`COPY_SIZE_PCT`) — sizes each trade as a percentage of
  your current balance (e.g. `0.10` = 10%), clamped between the CLOB's $5 minimum lot and
  `MAX_TRADE_SIZE_USD` as a hard cap. Auto-scales as your wallet grows without any config change.

- **Interactive setup wizard** — prompts for all credentials on first run and saves to `.env`.

- **Live balance tracking** — CLOB balance polled every 10 seconds.

- **Risk engine** — per-trade minimum notional ($1.00), per-trade size cap, and
  per-position drawdown filter (`MAX_COPY_LOSS_PCT`).

- **Terminal UI** — four-panel ratatui interface:
  - Account dashboard (balance, PnL, copy stats)
  - Live copy feed (pass/fail, skip reason per event)
  - Your open positions
  - Opportunity scanner table (color-coded by status)

- **Pre-commit quality gates** — `cargo fmt`, `cargo clippy -D warnings`, and `cargo test`
  run automatically before every commit via `.githooks/pre-commit`.

---

## Requirements

- Rust 1.78 or later (`rustup update stable`)
- A Polymarket account with a funded proxy wallet
- Your wallet's private key (for signing orders)

> **Warning:** Never commit your private key or `.env` file to version control.
> The provided `.gitignore` excludes `.env` by default.

---

## Installation

```bash
git clone https://github.com/cbaezp/polycopier
cd polycopier
# Enable the pre-commit quality gate
git config core.hooksPath .githooks
cargo build --release
```

The compiled binary will be at `target/release/polycopier`.

---

## Configuration

All configuration is stored in a `.env` file. You can either create it manually by copying
`.env.example`, or let the bot generate it interactively on first run.

```bash
cp .env.example .env
# Edit .env with your values, then:
cargo run --release
```

### Environment Variables

| Variable | Required | Default | Wizard | Description |
|---|---|---|---|---|
| `PRIVATE_KEY` | Yes | — | ✅ | Hex private key for the signing wallet (`0x…` or plain hex) |
| `FUNDER_ADDRESS` | Yes | — | ✅ | Proxy/Safe wallet address that holds USDC (shown on your Polymarket profile) |
| `TARGET_WALLETS` | Yes | — | ✅ | Comma-separated list of target proxy wallet addresses to copy |
| `MAX_SLIPPAGE_PCT` | No | `0.02` | ✅ | Maximum allowed price deviation from the copied trade (2% = `0.02`) |
| `MAX_TRADE_SIZE_USD` | No | `50.00` | ✅ | Hard cap: maximum USDC to spend on any single copied trade |
| `MAX_DELAY_SECONDS` | No | `10` | ✅ | Discard live trade events older than this many seconds |
| `MAX_COPY_LOSS_PCT` | No | `0.20` | ✅ | Skip catch-up entries where the target is already this far underwater (20% = `0.20`) |
| `COPY_SIZE_PCT` | No | `0.10` | ✅ | Size each trade as this fraction of available balance (10% = `0.10`). Floored at $5 (CLOB minimum), capped at `MAX_TRADE_SIZE_USD`. Set to blank/0 to always use `MAX_TRADE_SIZE_USD` exactly. |
| `MIN_ENTRY_PRICE` | No | `0.02` | — | Minimum token price for catch-up entries (filters near-zero dust) |
| `MAX_ENTRY_PRICE` | No | `0.999` | — | Maximum token price for catch-up entries. Set to `0.999` for targets who trade high-confidence NO positions. |

### Wallet Type

The SDK supports three signature types. Set the one matching your Polymarket account:

| Account Type | Signature Type in code |
|---|---|
| MetaMask / hardware wallet (EOA) | `SignatureType::Eoa` |
| Magic / email wallet | `SignatureType::Proxy` (current default) |
| Browser wallet via Gnosis Safe | `SignatureType::GnosisSafe` |

Edit `src/clients.rs` and change `SignatureType::Proxy` to match your setup.

---

## Usage

```bash
cargo run --release
```

On first run with an empty or placeholder `.env`, the setup wizard prompts for:

1. Private key (hidden input)
2. Funder address
3. Target wallet addresses
4. Max slippage, trade size cap, delay, and drawdown loss threshold
5. Proportional trade size (`COPY_SIZE_PCT`) — press Enter to accept `0.10` (10% of balance)

All values are saved to `.env` and reused on subsequent runs.

### Logging

By default the bot suppresses all log output below `WARN` level to keep the TUI clean.
To see verbose diagnostic output:

```bash
RUST_LOG=debug cargo run --release
```

### Keyboard Controls

| Key | Action |
|---|---|
| `q` | Quit |

---

## Architecture

```
main.rs
  |
  +-- config.rs          Load .env / interactive wizard
  |
  +-- clients.rs         CLOB authentication + order submission + balance fetcher
  |
  +-- listener.rs        Data API polling loop (2s, hash-dedup) -> TradeEvent channel
  |
  +-- position_scanner.rs  Catch-up scanner (adaptive 10–60s) -> TradeEvent channel
  |
  +-- strategy.rs        Receives TradeEvents, classifies intent, applies risk checks,
  |                       submits orders via OrderSubmitter
  |
  +-- risk.rs            RiskEngine: minimum notional, max size enforcement
  |
  +-- state.rs           Shared BotState (Arc<RwLock<_>>): balance, our positions,
  |                       target positions, live feed, TUI counters
  |
  +-- ui.rs              ratatui TUI: dashboard, live feed, positions, opportunity scanner
  |
  +-- models.rs          Core types: TradeEvent, EvaluatedTrade, TargetPosition, ScanStatus
  +-- utils.rs           Timestamp formatting helpers
```

### Data Flow

```
Polymarket Data API
    |
    +-- listener (2s poll, limit 20, hash-dedup) -----> mpsc::Sender<TradeEvent>
    |                                                              |
    +-- position_scanner (adaptive 10–60s poll) -----> mpsc::Sender<TradeEvent> (cloned)
                                                                   |
                                                         strategy engine
                                                           - wallet filter
                                                           - intent classification
                                                             (target_positions lookup)
                                                           - risk check (notional, size)
                                                           - SELL guard (must hold position)
                                                           - 97% SELL buffer
                                                                   |
                                                         CLOB API  (order submission)
```

---

## Opportunity Scanner Logic

The scanner fetches the target's full open portfolio and evaluates each position in order.
A position is skipped at the first failing guard:

1. **Already held** — the bot already holds this token (`SkippedOwned`)
2. **Already queued** — an entry order was sent this session (`Entered`)
3. **Price range** — current price must be between `$0.02` and `$0.95` (`SkippedPrice`)
4. **Loss threshold** — the target's unrealized loss must be less than `MAX_COPY_LOSS_PCT` (`SkippedLoss`)

Positions passing all guards are classified as `Monitoring` (green in TUI) and an entry is queued.

### Catch-up Intervals

The scanner reschedules itself after each cycle based on the best available opportunity:

| Target position state | Scan interval |
|---|---|
| Price exactly at target's entry (0% PnL) | 10s |
| Small move (±5%) | ~27s |
| Moderate move (±10%) | ~43s |
| Large move (±15%+) — would be chasing | 60s |
| No enterable (Monitoring) positions | 60s |
| Position past `MAX_COPY_LOSS_PCT` | 60s (filtered out) |

---

## Development

```bash
# Run with live reloading (requires cargo-watch)
cargo watch -x run

# Run the full test suite (82 tests, all pure/unit — no network)
cargo test --all

# Lint
cargo clippy --all-targets -- -D warnings

# Format
cargo fmt
```

The pre-commit hook runs fmt + clippy + tests automatically. To install it:

```bash
git config core.hooksPath .githooks
```

---

## Security Notes

- Your private key is used locally to sign EIP-712 order hashes. It is never transmitted
  to any server — only the resulting signature is sent to the CLOB API.
- The `.env` file is excluded from version control by `.gitignore`. Treat it like a password.
- Review `src/risk.rs` and configure appropriate limits before running with significant capital.
- This software is provided as-is. You are solely responsible for any trades it executes.

---

## Releases

Releases are created automatically by GitHub Actions whenever a version tag is pushed.
The workflow builds binaries for macOS (Apple Silicon), macOS (Intel), and Linux, then
publishes a GitHub Release with the compiled artifacts attached.

To cut a release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

Pre-release versions (e.g. `v0.2.0-beta`) are automatically marked as pre-release on GitHub.

---

## Disclaimer

This software is **experimental and provided for educational purposes only.**

- It has not been audited and may contain bugs that result in unintended order execution.
- Prediction market trading carries significant financial risk. Positions can go to zero.
- Past performance of any copied wallet is not indicative of future results.
- The authors take no responsibility for financial losses incurred through use of this software.
- You are solely responsible for reviewing the risk parameters, monitoring the bot while it runs,
  and ensuring compliance with the terms of service of Polymarket and applicable laws in
  your jurisdiction.

Start with the minimum `MAX_TRADE_SIZE_USD` and verify each order in your Polymarket
dashboard before increasing capital exposure.

---

## License

MIT. See [LICENSE](LICENSE).
