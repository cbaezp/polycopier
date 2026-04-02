import { useEffect, useState } from 'react';
import SettingsManager from './SettingsManager';

function App() {
  const [state, setState] = useState<any>(null);
  const [activeTab, setActiveTab] = useState<'dashboard' | 'settings'>('dashboard');

  useEffect(() => {
    const fetchState = async () => {
      try {
        const res = await fetch('/api/state');
        const data = await res.json();
        setState(data);
      } catch (err) {
        console.error('Failed to fetch state', err);
      }
    };
    
    fetchState();
    const interval = setInterval(fetchState, 1000);
    return () => clearInterval(interval);
  }, []);

  if (!state) {
    return (
      <div className="loading-container">
        <div className="spinner"></div>
        <div style={{ color: 'var(--text-secondary)' }}>Connecting to Polycopier Daemon...</div>
      </div>
    );
  }

  const targets = (state.target_positions || []).sort((a: any, b: any) => {
    const statusScore = (s: string) => s.includes('Monitoring') ? 0 : s.includes('Entered') ? 1 : 2;
    return statusScore(a.status) - statusScore(b.status);
  });

  const getStatusLabel = (status: any) => {
      if (typeof status === 'string') { return status; }
      if (status && typeof status === 'object') { return Object.keys(status)[0]; }
      return 'UNKNOWN';
  };

  return (
    <div className="dashboard-container">
      <header>
        <div className="header-title" style={{ display: 'flex', justifyContent: 'space-between', width: '100%' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: '1rem' }}>
            <h1>Polycopier</h1>
            <div className="live-badge">LIVE</div>
          </div>
          <div className="nav-tabs">
            <button className={`nav-tab ${activeTab === 'dashboard' ? 'active' : ''}`} onClick={() => setActiveTab('dashboard')}>Dashboard</button>
            <button className={`nav-tab ${activeTab === 'settings' ? 'active' : ''}`} onClick={() => setActiveTab('settings')}>Settings & Env</button>
          </div>
        </div>
        
        <div className="stats-grid">
          <div className="stat-card">
            <span className="stat-label">Total Balance</span>
            <span className="stat-value">${parseFloat(state.total_balance).toFixed(2)}</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Unrealized PnL</span>
            <span className={`stat-value ${parseFloat(state.unrealized_pnl) < 0 ? 'val-negative' : parseFloat(state.unrealized_pnl) > 0 ? 'val-positive' : 'val-neutral'}`}>
              ${parseFloat(state.unrealized_pnl).toFixed(2)}
            </span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Copies Executed</span>
            <span className="stat-value">{state.copies_executed}</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Trades Skipped</span>
            <span className="stat-value">{state.trades_skipped}</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Next Scan</span>
            <span className="stat-value">{state.next_scan_secs}s</span>
          </div>
        </div>
      </header>

      {activeTab === 'dashboard' ? (
        <div className="grid-cols-2">
          <div style={{ display: 'flex', flexDirection: 'column', gap: '1.5rem', flex: 1 }}>
            
            {/* Our Open Positions */}
            <div className="glass-panel">
              <div className="panel-header">
                Our Positions
                <span className="panel-subtitle">Held: {Object.keys(state.positions || {}).length}</span>
              </div>
              <div className="table-container">
                <table>
                  <thead>
                    <tr>
                      <th>Market</th>
                      <th>Size</th>
                      <th>Our Avg</th>
                      <th>Target Avg</th>
                      <th>Diff %</th>
                      <th>Status</th>
                    </tr>
                  </thead>
                  <tbody>
                    {Object.values(state.positions || {}).map((p: any, i: number) => {
                      const t = targets.find((x: any) => x.token_id === p.token_id);
                      const title = t ? t.title : `${p.token_id.substring(0, 15)}...`;
                      const ourAvg = parseFloat(p.average_entry_price);
                      const targetAvg = t ? parseFloat(t.avg_price) : 0;
                      const diffPct = targetAvg > 0 ? ((ourAvg - targetAvg) / targetAvg) * 100 : 0;
                      return (
                        <tr key={i}>
                          <td className="td-truncate" title={title}>{title}</td>
                          <td>{parseFloat(p.size).toFixed(2)}</td>
                          <td>${ourAvg.toFixed(3)}</td>
                          <td>{targetAvg > 0 ? `$${targetAvg.toFixed(3)}` : '-'}</td>
                          <td className={targetAvg > 0 ? (diffPct > 0 ? 'val-negative' : 'val-positive') : ''}>
                            {targetAvg > 0 ? `${diffPct > 0 ? '+' : ''}${diffPct.toFixed(1)}%` : '-'}
                          </td>
                          <td><span className="status status-HELD">HELD</span></td>
                        </tr>
                      );
                    })}
                    {Object.keys(state.positions || {}).length === 0 && (
                      <tr><td colSpan={6} style={{ textAlign: 'center', color: 'var(--text-secondary)' }}>No open positions</td></tr>
                    )}
                  </tbody>
                </table>
              </div>
            </div>

            {/* Pending Orders */}
            <div className="glass-panel">
              <div className="panel-header">
                Pending Orders
                <span className="panel-subtitle">Queued: {(state.pending_order_tokens || []).length}</span>
              </div>
              <div className="table-container">
                <table>
                  <thead>
                    <tr>
                      <th>Market</th>
                      <th>Target Avg</th>
                      <th>Cur Price</th>
                      <th>Status</th>
                    </tr>
                  </thead>
                  <tbody>
                    {(state.pending_order_tokens || []).map((tokenId: string, i: number) => {
                      const t = targets.find((x: any) => x.token_id === tokenId);
                      const title = t ? t.title : `${tokenId.substring(0, 15)}...`;
                      const targetAvg = t ? parseFloat(t.avg_price) : 0;
                      const curPrice = t ? parseFloat(t.cur_price) : 0;
                      return (
                        <tr key={i}>
                          <td className="td-truncate" title={title}>{title}</td>
                          <td>{targetAvg > 0 ? `$${targetAvg.toFixed(3)}` : '-'}</td>
                          <td>{curPrice > 0 ? `$${curPrice.toFixed(3)}` : '-'}</td>
                          <td><span className="status status-QUEUED">QUEUED</span></td>
                        </tr>
                      );
                    })}
                    {(state.pending_order_tokens || []).length === 0 && (
                      <tr><td colSpan={4} style={{ textAlign: 'center', color: 'var(--text-secondary)' }}>No queued orders</td></tr>
                    )}
                  </tbody>
                </table>
              </div>
            </div>

            {/* Target Positions (Scanning/Watching) */}
            <div className="glass-panel">
              <div className="panel-header">
                Target Positions (Scanning / Watching)
                <span className="panel-subtitle">Total Scanned: {targets.length}</span>
              </div>
              <div className="table-container" style={{ maxHeight: '400px', overflowY: 'auto' }}>
                <table>
                  <thead>
                    <tr>
                      <th>Market</th>
                      <th>Side</th>
                      <th>Target Avg</th>
                      <th>Cur Price</th>
                      <th>PnL</th>
                      <th>Status</th>
                    </tr>
                  </thead>
                  <tbody>
                    {targets.slice(0, 30).map((t: any, i: number) => {
                      const statusKey = getStatusLabel(t.status);
                      const pnl = parseFloat(t.percent_pnl) * 100;
                      return (
                        <tr key={i}>
                          <td className="td-truncate" title={t.title}>{t.title}</td>
                          <td>{t.outcome}</td>
                          <td>${parseFloat(t.avg_price).toFixed(3)}</td>
                          <td>${parseFloat(t.cur_price).toFixed(3)}</td>
                          <td className={pnl < 0 ? 'val-negative' : pnl > 0 ? 'val-positive' : ''}>
                            {pnl > 0 ? '+' : ''}{pnl.toFixed(1)}%
                          </td>
                          <td>
                            <span className={`status status-${statusKey.replace('Skipped', '')}`}>
                              {statusKey}
                            </span>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>

          </div>

          <div className="glass-panel" style={{ alignSelf: 'flex-start', position: 'sticky', top: '2rem' }}>
            <div className="panel-header">Live Feed</div>
            <div className="feed-list" style={{ maxHeight: '800px' }}>
              {(state.live_feed || []).slice(0, 20).map((feed: any, i: number) => {
                const ev = feed.original_event;
                return (
                  <div key={i} className="feed-item">
                    <div className="feed-item-header">
                      <span className={`side-${ev.side}`}>{ev.side}</span>
                      <span className="feed-token">{ev.token_id.substring(0, 8)}...</span>
                      <span>${parseFloat(ev.price).toFixed(3)}</span>
                    </div>
                    <div style={{ fontSize: '0.875rem' }}>Size: {parseFloat(ev.size).toFixed(2)}</div>
                    {!feed.validated && feed.reason && (
                      <div className="feed-reason">Skipped: {feed.reason}</div>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        </div>
      ) : (
        <SettingsManager />
      )}
    </div>
  );
}

export default App;
