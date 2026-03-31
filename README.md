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
their trades into your own account in real time. Beyond reactive copying, it also scans
the target's full open-position portfolio at regular intervals, evaluating whether each
position still represents a sensible entry — and acting on those that do.

The entire interface is a terminal UI (TUI) that displays live activity, your own holdings,
and the opportunity scanner side-by-side with no browser required.

---

## Features

- **Real-time trade copying** — polls the Polymarket Data API every 2 seconds and mirrors
  qualified trades within your configured slippage and size limits.

- **Open-position scanner** — fetches the target wallet's full portfolio every 60 seconds
  and evaluates each position against entry criteria: price range, unrealized loss threshold,
  and whether you already hold the token.

- **Interactive setup wizard** — on first run the bot prompts for all required credentials
  and saves them to `.env`. Subsequent runs load from the saved file automatically.

- **Live balance tracking** — the CLOB API is polled every 10 seconds so your USDC balance
  stays current in the dashboard.

- **Risk engine** — built-in spoofing protection (minimum notional value) and per-trade
  size cap. Easily extensible for drawdown limits and exposure controls.

- **Terminal UI** — a four-panel ratatui interface showing:
  - Account dashboard (balance, PnL, copy stats)
  - Live copy feed with pass/fail status per event
  - Your open positions
  - Full opportunity scanner table (color-coded by entry status)

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

| Variable | Required | Default | Description |
|---|---|---|---|
| `PRIVATE_KEY` | Yes | — | Hex private key for the signing wallet (`0x…` or plain hex) |
| `FUNDER_ADDRESS` | Yes | — | Proxy/Safe wallet address that holds USDC (shown on your Polymarket profile) |
| `TARGET_WALLETS` | Yes | — | Comma-separated list of target proxy wallet addresses to copy |
| `MAX_SLIPPAGE_PCT` | No | `0.02` | Maximum allowed price deviation from the copied trade (2% = `0.02`) |
| `MAX_TRADE_SIZE_USD` | No | `10.00` | Maximum USDC to spend per copied trade |
| `MAX_DELAY_SECONDS` | No | `2` | Discard live trade events older than this many seconds |
| `MAX_COPY_LOSS_PCT` | No | `0.40` | Skip open-position entries where the target is already this far underwater (40% = `0.40`) |

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
4. Slippage, trade size, delay, and loss-threshold limits

All values are saved to `.env` and reused on subsequent runs.

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
  +-- listener.rs        Data API polling loop (2s interval) -> TradeEvent channel
  |
  +-- position_scanner.rs  Open-position mirror scanner (60s interval) -> TradeEvent channel
  |
  +-- strategy.rs        Receives TradeEvents, applies debounce + risk checks, submits orders
  |
  +-- risk.rs            RiskEngine: minimum notional, max size enforcement
  |
  +-- state.rs           Shared BotState (Arc<RwLock<_>>): balance, positions, feed, scanner data
  |
  +-- ui.rs              ratatui TUI: dashboard, live feed, positions, opportunity scanner
  |
  +-- models.rs          Core data types: TradeEvent, EvaluatedTrade, TargetPosition, ScanStatus
  +-- utils.rs           Timestamp formatting helpers
```

### Data Flow

```
Polymarket Data API
    |
    +-- listener (2s poll) ------------> mpsc::Sender<TradeEvent>
    |                                              |
    +-- position_scanner (60s poll) ---> mpsc::Sender<TradeEvent> (cloned)
                                                   |
                                         strategy engine
                                           - wallet filter
                                           - debounce (fragmented fills)
                                           - risk check
                                           - size cap
                                                   |
                                         CLOB API  (order submission)
```

---

## Opportunity Scanner Logic

The position scanner evaluates each of the target's open positions against four criteria
in order. A position is skipped at the first failing guard:

1. **Already held** — the bot already holds this token (`SkippedOwned`)
2. **Already queued** — an entry order was sent this session (`Entered`)
3. **Price range** — current price must be between `$0.02` and `$0.95` (`SkippedPrice`)
4. **Loss threshold** — the target's unrealized loss must be less than `MAX_COPY_LOSS_PCT` (`SkippedLoss`)

Positions that pass all guards are classified as `Monitoring` (shown in green in the TUI)
and an entry order is queued. Entries are size-capped to `MAX_TRADE_SIZE_USD / cur_price`
and rounded to two decimal places (the SDK's lot-size constraint).

---

## Development

```bash
# Run with live reloading (requires cargo-watch)
cargo watch -x run

# Check for errors and warnings
cargo check

# Run clippy lints
cargo clippy -- -D warnings

# Format code
cargo fmt
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
publishes a GitHub Release with the compiled artifacts attached and release notes generated
from commits since the previous tag.

To cut a release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

GitHub Actions will handle the rest. The release will appear at
`https://github.com/cbaezp/polycopier/releases` within a few minutes.

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
