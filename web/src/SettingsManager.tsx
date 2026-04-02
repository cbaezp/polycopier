import { useState, useEffect } from "react";

export default function SettingsManager() {
  const [config, setConfig] = useState<any>(null);
  const [envData, setEnvData] = useState<any>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [message, setMessage] = useState("");

  useEffect(() => {
    fetch("/api/config")
      .then((res) => res.json())
      .then(setConfig);
    fetch("/api/env")
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
      } else if (Array.isArray(payloadConfig.targets?.wallets)) {
        payloadConfig.targets.wallets = payloadConfig.targets.wallets
          .map((s: string) => s.trim())
          .filter(Boolean);
      }

      await fetch("/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payloadConfig),
      });

      await fetch("/api/env", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(envData),
      });

      if (confirm("Settings successfully flushed to disk. Restart the background engine to apply the changes?")) {
        await fetch("/api/action/restart", { method: "POST" });
        setMessage("Daemon restart signal sent.");
      } else {
        setMessage("Saved successfully! Restart later to apply.");
      }
    } catch (e) {
      setMessage("Failed to save.");
    }

    setIsSaving(false);
  };



  if (!config || !envData) return <div className="loading-container"><div className="spinner"></div><div>Loading Control Center...</div></div>;

  const isSelfPct = config.sizing.mode === "self_pct";
  const hasLossGuard = parseInt(config.risk.max_consecutive_losses || 0) > 0;

  return (
    <div className="settings-container">
      <div className="glass-panel" style={{ marginBottom: "1.5rem" }}>
        <div className="panel-header">Environment Secrets</div>
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
        <div style={{ display: 'flex', flexDirection: 'column', gap: '1.5rem' }}>
          <div className="glass-panel">
            <div className="panel-header">Targets Network</div>
            <div className="form-group">
              <label>Wallets to Copy</label>
              {(Array.isArray(config.targets.wallets) ? config.targets.wallets : []).map((w: string, idx: number) => (
                <div key={idx} style={{ display: 'flex', gap: '0.5rem', marginBottom: '0.5rem' }}>
                  <input
                    type="text"
                    value={w}
                    onChange={(e) => {
                      const newWallets = [...config.targets.wallets];
                      newWallets[idx] = e.target.value;
                      setConfig({ ...config, targets: { ...config.targets, wallets: newWallets }});
                    }}
                    placeholder="0x..."
                    style={{ flex: 1 }}
                  />
                  <button
                    className="btn btn-secondary"
                    onClick={() => {
                      const newWallets = config.targets.wallets.filter((_: any, i: number) => i !== idx);
                      setConfig({ ...config, targets: { ...config.targets, wallets: newWallets }});
                    }}
                  >
                    X
                  </button>
                </div>
              ))}
              <button
                className="btn btn-primary"
                style={{ marginTop: '0.5rem', alignSelf: 'flex-start' }}
                onClick={() => {
                   const curr = Array.isArray(config.targets.wallets) ? config.targets.wallets : [];
                   setConfig({ ...config, targets: { ...config.targets, wallets: [...curr, ""] }});
                }}
              >
                + Add Wallet
              </button>
              <span className="field-hint" style={{ marginTop: '0.5rem', display: 'block' }}>The proxy wallets tracked by the websocket listener natively.</span>
            </div>
            <div className="form-group" style={{ marginTop: '1rem' }}>
              <label>Max Websocket Delay (Staleness filter)</label>
              <div style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
                <input
                  type="number"
                  style={{ width: '100px' }}
                  value={config.execution.max_delay_seconds}
                  onChange={(e) =>
                    setConfig({ ...config, execution: { ...config.execution, max_delay_seconds: parseInt(e.target.value) } })
                  }
                />
                <span className="field-hint">seconds</span>
              </div>
            </div>
          </div>

          <div className="glass-panel">
            <div className="panel-header">Scanner Tuning</div>
            <div className="form-group">
              <label>Max Entries Per Scan Cycle</label>
              <input
                type="number"
                style={{ width: '100px' }}
                value={config.scanner.max_entries_per_cycle}
                onChange={(e) =>
                  setConfig({ ...config, scanner: { ...config.scanner, max_entries_per_cycle: parseInt(e.target.value) } })
                }
              />
              <span className="field-hint">How many concurrent Catch-up buys the scanner drops onto the CLOB simultaneously.</span>
            </div>
            <div className="form-group" style={{ marginTop: '1rem' }}>
              <label>Catch-up Loss Tolerance (Downside Threshold)</label>
              <div style={{ display: 'flex', alignItems: 'center', gap: '1rem' }}>
                <input
                  type="range"
                  min="0" max="1.0" step="0.01"
                  value={parseFloat(config.scanner.max_copy_loss_pct) || 0}
                  onChange={(e) =>
                    setConfig({ ...config, scanner: { ...config.scanner, max_copy_loss_pct: e.target.value } })
                  }
                  style={{ flex: 1 }}
                />
                <span className="val-negative" style={{ fontSize: '1.2rem', fontWeight: 600, minWidth: '60px' }}>
                  -{((parseFloat(config.scanner.max_copy_loss_pct) || 0) * 100).toFixed(0)}%
                </span>
              </div>
              <span className="field-hint" style={{ display: 'block', marginTop: '0.25rem' }}>
                If you just turned on the bot and the proxy wallet holds a token that is <strong>deep in the red</strong> (past this percentage), the system will intelligently <em>skip</em> copying it to protect you from catching a falling knife.
              </span>
            </div>

            <div className="form-group" style={{ marginTop: '1.5rem' }}>
              <label>Catch-up FOMO Guard (Upside Threshold)</label>
              <div style={{ display: 'flex', alignItems: 'center', gap: '1rem' }}>
                <input
                  type="range"
                  min="0" max="1.0" step="0.01"
                  value={parseFloat(config.scanner.max_copy_gain_pct) || 0}
                  onChange={(e) =>
                    setConfig({ ...config, scanner: { ...config.scanner, max_copy_gain_pct: e.target.value } })
                  }
                  style={{ flex: 1 }}
                />
                <span className="val-positive" style={{ fontSize: '1.2rem', fontWeight: 600, minWidth: '60px' }}>
                  +{((parseFloat(config.scanner.max_copy_gain_pct) || 0) * 100).toFixed(0)}%
                </span>
              </div>
              <span className="field-hint" style={{ display: 'block', marginTop: '0.25rem' }}>
                If the proxy wallet holds a token that has already <strong>pumped heavily</strong> (past this percentage), the system will <em>skip</em> copying it to prevent you from buying at the absolute top of a missed rally.
              </span>
            </div>
            <div className="form-group" style={{ marginTop: '1rem' }}>
              <label>Entry Price Dust Filter</label>
              <div style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
                <span style={{ color: 'var(--text-secondary)' }}>$</span>
                <input
                  type="text"
                  placeholder="Min"
                  style={{ width: '80px' }}
                  value={config.scanner.min_entry_price}
                  onChange={(e) => setConfig({ ...config, scanner: { ...config.scanner, min_entry_price: e.target.value } })}
                />
                <span>to</span>
                <span style={{ color: 'var(--text-secondary)' }}>$</span>
                <input
                  type="text"
                  placeholder="Max"
                  style={{ width: '80px' }}
                  value={config.scanner.max_entry_price}
                  onChange={(e) => setConfig({ ...config, scanner: { ...config.scanner, max_entry_price: e.target.value } })}
                />
              </div>
              <span className="field-hint" style={{ display: 'block', marginTop: '0.5rem' }}>
                <strong>Min Filter:</strong> Ignores dead markets (e.g. $0.01) so you don't accumulate abandoned losing tickets from the target's wallet.
              </span>
              <span className="field-hint" style={{ display: 'block', marginTop: '0.25rem' }}>
                <strong>Max Filter:</strong> Ignores fully-realized winning markets (e.g. $0.99) to prevent risking capital for pennies.
              </span>
            </div>
          </div>
        </div>

        <div style={{ display: 'flex', flexDirection: 'column', gap: '1.5rem' }}>
          <div className="glass-panel">
            <div className="panel-header">Sizing Engine</div>
            <div className="form-group">
              <label>Sizing Strategy Mode</label>
              <select
                value={config.sizing.mode}
                onChange={(e) => setConfig({ ...config, sizing: { ...config.sizing, mode: e.target.value } })}
              >
                <option value="fixed">Fixed (Enforce Max Trade Size)</option>
                <option value="self_pct">Self Percentage (Fraction of our balance)</option>
                <option value="target_usd">Mirror Target USD (Proportional size)</option>
              </select>
            </div>
            
            {isSelfPct && (
              <div className="form-group dynamic-field" style={{ animation: 'fadeIn 0.3s', marginTop: '1rem' }}>
                <label style={{ color: 'var(--accent-primary)' }}>Our Balance Allocation (Copy Size Pct)</label>
                <div style={{ display: 'flex', alignItems: 'center', gap: '1rem' }}>
                  <input
                    type="range"
                    min="0" max="1.0" step="0.01"
                    value={parseFloat(config.sizing.copy_size_pct || "0.10")}
                    onChange={(e) =>
                      setConfig({ ...config, sizing: { ...config.sizing, copy_size_pct: e.target.value } })
                    }
                    style={{ flex: 1 }}
                  />
                  <span style={{ fontSize: '1.2rem', fontWeight: 600, minWidth: '60px', color: 'var(--accent-primary)' }}>
                    {((parseFloat(config.sizing.copy_size_pct || "0.10")) * 100).toFixed(0)}%
                  </span>
                </div>
                <span className="field-hint" style={{ display: 'block', marginTop: '0.25rem' }}>
                  Fraction of our total available balance to deploy on each copied trade.
                </span>
              </div>
            )}

            <div className="form-group" style={{ marginTop: isSelfPct ? '1.5rem' : '1rem' }}>
              <label>Max Trade Ceiling (Safety Stop)</label>
              <div style={{ display: 'flex', alignItems: 'center', gap: '0.5rem' }}>
                <span style={{ fontSize: '1.2rem', fontWeight: 600, color: 'var(--text-secondary)' }}>$</span>
                <input
                  type="number"
                  min="5"
                  style={{ width: '120px' }}
                  value={config.execution.max_trade_size_usd}
                  onChange={(e) =>
                    setConfig({ ...config, execution: { ...config.execution, max_trade_size_usd: e.target.value } })
                  }
                />
                <span>USD</span>
              </div>
              <span className="field-hint" style={{ display: 'block', marginTop: '0.25rem' }}>
                Regardless of the algorithm above, a single entry will <strong>never</strong> exceed this absolute USD value cap. This strictly safeguards your balance.
              </span>
            </div>
          </div>

          <div className="glass-panel">
            <div className="panel-header">Risk Guards</div>
            <div className="form-group">
              <label>Slippage Aggression</label>
              <div style={{ display: 'flex', alignItems: 'center', gap: '1rem' }}>
                <input
                  type="range"
                  min="0" max="0.10" step="0.001"
                  value={parseFloat(config.execution.max_slippage_pct) || 0}
                  onChange={(e) =>
                    setConfig({ ...config, execution: { ...config.execution, max_slippage_pct: e.target.value } })
                  }
                  style={{ flex: 1 }}
                />
                <span style={{ fontSize: '1.2rem', fontWeight: 600, minWidth: '60px', color: 'var(--accent-warning)' }}>
                  {((parseFloat(config.execution.max_slippage_pct) || 0) * 100).toFixed(1)}%
                </span>
              </div>
              <span className="field-hint" style={{ display: 'block', marginTop: '0.25rem' }}>
                Price buffer strictly added to target's entry price to guarantee prompt execution if the orderbook is violently shifting.
              </span>
            </div>

            <div className="form-group" style={{ marginTop: '1.5rem' }}>
              <label style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', cursor: 'pointer' }}>
                <input
                  type="checkbox"
                  checked={config.risk.max_consecutive_losses > 0}
                  onChange={(e) => {
                    const enabled = e.target.checked;
                    setConfig({ ...config, risk: { ...config.risk, max_consecutive_losses: enabled ? 3 : 0 } });
                  }}
                  style={{ width: 'auto', margin: 0 }}
                />
                <span>Enable Systemic Cooldown Guard</span>
              </label>
              <span className="field-hint" style={{ display: 'block', marginTop: '0.25rem' }}>
                Automatically suspends copying entirely if the target proxy wallet starts bleeding heavily.
              </span>
            </div>

            {hasLossGuard && (
              <div className="form-group dynamic-field" style={{ animation: 'fadeIn 0.3s', paddingLeft: '1.5rem', borderLeft: '2px solid rgba(255,255,255,0.1)' }}>
                <label>Consecutive Realized Losses Limit</label>
                <div style={{ display: 'flex', alignItems: 'center', gap: '0.5rem' }}>
                  <input
                    type="number"
                    min="1"
                    style={{ width: '100px' }}
                    value={config.risk.max_consecutive_losses}
                    onChange={(e) =>
                      setConfig({ ...config, risk: { ...config.risk, max_consecutive_losses: Math.max(1, parseInt(e.target.value) || 1) } })
                    }
                  />
                  <span className="field-hint">losses</span>
                </div>
              </div>
            )}

            {hasLossGuard && (
              <div className="form-group dynamic-field" style={{ animation: 'fadeIn 0.3s' }}>
                <label style={{ color: 'var(--accent-danger)' }}>Loss Trigger Cooldown Window</label>
                <div style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
                  <input
                    type="number"
                    style={{ width: '100px' }}
                    value={config.risk.loss_cooldown_secs}
                    onChange={(e) =>
                      setConfig({ ...config, risk: { ...config.risk, loss_cooldown_secs: parseInt(e.target.value) } })
                    }
                  />
                  <span className="field-hint">seconds timeout until strategy engine resumes parsing WS events.</span>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>

      <div className="settings-actions" style={{ position: 'sticky', bottom: '-1px', background: 'var(--bg-secondary)', padding: '1rem', borderTop: '1px solid var(--border)', zIndex: 10 }}>
        {message && <span className="message">{message}</span>}
        <button className="btn btn-primary" onClick={handleSave} disabled={isSaving}>
          {isSaving ? "Writing configs..." : "Save Configuration System"}
        </button>
      </div>
    </div>
  );
}
