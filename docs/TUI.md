# Terminal UI & Headless Worker Documentation

Polycopier utilizes a highly optimized `ratatui` asynchronous interface that allows absolute terminal monitoring and an invisible background worker mode when deployed to cloud servers.

## 1. Pure Terminal Mode (`cargo run --release`)
This is the default initialization. The terminal bypasses all web APIs and binds strictly to the console, maximizing execution speed and minimizing footprint.

### Live Copy Feed (Left Panel)
Tracks active positions in real-time pulling directly from the Polymarket CLOB via WebSocket mapping. 
*   Shows exactly when you entered.
*   Displays the real-time spread drift and realized loss/gain visually to prevent CLI clutter.

### Opportunity Scanner (Right Panel)
Showcases exact network polling metrics orchestrating your execution environment:
*   How often the Target Wallets are being scanned vs when the next tick dynamically occurs.
*   How many positions are currently tracked versus actively watching for specific price threshold conditions.

### TUI Interactivity & Hot Reloading
You can press `s` inside the Terminal to open the built-in Console Settings Manager. It offers an intuitive form driven by the `inquire` library, letting you dynamically modify:
*   **Target Wallets:** The addresses you are trailing.
*   **Sizing Mode:** Switching between `fixed`, `self_pct`, and `copy_pct` natively.
*   **Risk Parameters:** Hard caps like `max_slip_pct`, USD thresholds, and cooldowns.

Pressing **Save** updates `config.toml` and instantly applies the changes to the live bot logic without restarting or interrupting active CLOB listeners.

## 2. Headless Daemon Worker (`cargo run --release -- --daemon`)
For cloud environments (e.g. AWS, PM2, or pure Ubuntu remote servers), the terminal UI is completely discarded.

*   **No Background Processes:** The `axum` Web UI server is explicitly blocked from spawning on `localhost:3000` to guarantee absolute daemon purity.
*   **Stdout Logging:** The bot gracefully falls back to `tracing` info-level logging, printing JSON or plaintext directly to `stdout`. This makes it completely compatible with services like `journalctl`, `systemd`, or `Docker`.
*   **24/7 Stability:** Since all UI overhead is removed, memory footprint remains mathematically negligible for infinite-loop persistence across thousands of trading cycles.
