import { useState, useEffect } from "react";

export default function SettingsManager() {
  const [config, setConfig] = useState<any>(null);
  const [envData, setEnvData] = useState<any>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [message, setMessage] = useState("");

  useEffect(() => {
    fetch("http://localhost:3000/api/config")
      .then((res) => res.json())
      .then(setConfig);
    fetch("http://localhost:3000/api/env")
      .then((res) => res.json())
      .then(setEnvData);
  }, []);

  const handleSave = async () => {
    setIsSaving(true);
    setMessage("");

    try {
      // Clean up target wallets array if string
      const payloadConfig = { ...config };
      if (typeof payloadConfig.targets?.wallets === "string") {
        payloadConfig.targets.wallets = payloadConfig.targets.wallets
          .split(",")
          .map((s: string) => s.trim())
          .filter(Boolean);
      }

      await fetch("http://localhost:3000/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payloadConfig),
      });

      await fetch("http://localhost:3000/api/env", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(envData),
      });

      setMessage("Saved successfully! Restart bot to apply.");
    } catch (e) {
      setMessage("Failed to save.");
    }

    setIsSaving(false);
  };

  const handleRestart = async () => {
    if (confirm("The bot will self-terminate. You must restart it manually via cargo run if not using a daemon. Proceed?")) {
      await fetch("http://localhost:3000/api/action/restart", { method: "POST" });
      setMessage("Bot stopped.");
    }
  };

  if (!config || !envData) return <div className="loading">Loading Settings...</div>;

  return (
    <div className="settings-container">
      <div className="glass-panel" style={{ marginBottom: "1.5rem" }}>
        <div className="panel-header">
          Environment Secrets (.env)
        </div>
        <div className="form-group">
          <label>Private Key (Polymarket Signer)</label>
          <input
            type="password"
            value={envData.private_key}
            onChange={(e) => setEnvData({ ...envData, private_key: e.target.value })}
            placeholder="0x..."
          />
        </div>
        <div className="form-group">
          <label>Funder Wallet Address (Polymarket Proxy)</label>
          <input
            type="text"
            value={envData.funder_address}
            onChange={(e) => setEnvData({ ...envData, funder_address: e.target.value })}
            placeholder="0x..."
          />
        </div>
      </div>

      <div className="grid-cols-2">
        <div className="glass-panel">
          <div className="panel-header">Targets (config.toml)</div>
          <div className="form-group">
            <label>Wallets to Copy (comma separated)</label>
            <textarea
              rows={4}
              value={Array.isArray(config.targets.wallets) ? config.targets.wallets.join(",\n") : config.targets.wallets}
              onChange={(e) =>
                setConfig({
                  ...config,
                  targets: { ...config.targets, wallets: e.target.value },
                })
              }
            />
          </div>

          <div className="panel-header" style={{ marginTop: "1rem" }}>Scanner Tuning</div>
          <div className="form-group">
            <label>Max Copy Loss Pct (e.g. 0.40 = 40%)</label>
            <input
              type="text"
              value={config.scanner.max_copy_loss_pct}
              onChange={(e) =>
                setConfig({ ...config, scanner: { ...config.scanner, max_copy_loss_pct: e.target.value } })
              }
            />
          </div>
          <div className="form-group">
            <label>Max Entries Per Cycle</label>
            <input
              type="number"
              value={config.scanner.max_entries_per_cycle}
              onChange={(e) =>
                setConfig({ ...config, scanner: { ...config.scanner, max_entries_per_cycle: parseInt(e.target.value) } })
              }
            />
          </div>
        </div>

        <div className="glass-panel">
          <div className="panel-header">Execution & Risk</div>
          <div className="form-group">
            <label>Max Slippage Pct (e.g. 0.02 = 2%)</label>
            <input
              type="text"
              value={config.execution.max_slippage_pct}
              onChange={(e) =>
                setConfig({ ...config, execution: { ...config.execution, max_slippage_pct: e.target.value } })
              }
            />
          </div>
          <div className="form-group">
            <label>Max Trade Size USD ($)</label>
            <input
              type="text"
              value={config.execution.max_trade_size_usd}
              onChange={(e) =>
                setConfig({ ...config, execution: { ...config.execution, max_trade_size_usd: e.target.value } })
              }
            />
          </div>
          <div className="form-group">
            <label>Sizing Mode</label>
            <select
              value={config.sizing.mode}
              onChange={(e) => setConfig({ ...config, sizing: { ...config.sizing, mode: e.target.value } })}
            >
              <option value="fixed">Fixed (Max Trade Size)</option>
              <option value="self_pct">Self Percentage</option>
              <option value="target_usd">Mirror Target USD</option>
            </select>
          </div>
        </div>
      </div>

      <div className="settings-actions">
        {message && <span className="message">{message}</span>}
        <button className="btn btn-danger" onClick={handleRestart}>Force Restart</button>
        <button className="btn btn-primary" onClick={handleSave} disabled={isSaving}>
          {isSaving ? "Saving..." : "Save Settings"}
        </button>
      </div>
    </div>
  );
}
